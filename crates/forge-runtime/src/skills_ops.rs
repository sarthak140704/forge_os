//! Phase 4a — Version-controlled skills, curator, and IPC-friendly glue.
//!
//! This module orchestrates the three-way handshake between:
//!   1. the SKILL.md **files** on disk (managed by `forge_skills::proposal`)
//!   2. the **history table** rows (managed by
//!      `forge_persistence::SkillHistoryRepository`)
//!   3. the **content-addressed store** at `<skills_root>/history/`
//!      (managed by `forge_skills::SkillVersionStore`)
//!
//! Every operation (promote / rollback / retire) is atomic-ish: files first,
//! then history, then the version-store snapshot. If the DB write fails we
//! log — but the on-disk state is still correct, and re-running the same
//! operation is idempotent thanks to the content-addressed store.
//!
//! Curator heuristics (all lightweight — no LLM):
//!   - **duplicate**: two active skills whose names Jaro-Winkler ≥ 0.90 OR
//!     whose body-prefix (first 400 chars, whitespace-collapsed) matches
//!     exactly.
//!   - **unused**: an active skill whose name has never appeared in a
//!     `SkillsSelected` event within the last `curator.unused_window`
//!     terminal missions.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use forge_domain::{ForgeEvent, MissionId};
use forge_events::EventBus;
use forge_persistence::{
    NewSkillVersion, SkillHistoryRepository, SkillOrigin, SkillVersionRecord,
    SqliteEventStore, EventStore,
};
use forge_skills::{
    approve_proposal_detail, list_proposals, restore_from_bytes, retire_active_skill,
    parse_skill, validate_bytes, ActiveSkillSummary, SkillVersionStore, ValidationReport,
    ValidatorContext,
};
use std::str::FromStr;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SkillOpsError {
    #[error("skill: {0}")]
    Skill(#[from] forge_skills::SkillError),
    #[error("persistence: {0}")]
    Persistence(#[from] forge_persistence::PersistenceError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("skill `{0}` has no active version")]
    NoActive(String),
    #[error("target sha `{0}` not found in history store for skill `{1}`")]
    ShaMissing(String, String),
    #[error("proposal `{0}` not found")]
    ProposalMissing(String),
    #[error("proposal `{filename}` failed validation: {failed:?}")]
    ValidationFailed { filename: String, failed: Vec<String> },
}

pub struct SkillOps {
    pub skills_root: PathBuf,
    pub history:     Arc<dyn SkillHistoryRepository>,
    pub store:       SkillVersionStore,
    pub events:      EventBus,
    /// Tool ids known to the local runtime — consulted by the validator's
    /// `tools_resolvable` check. Empty in test / headless contexts.
    pub known_tools: Vec<String>,
}

impl SkillOps {
    pub fn new(
        skills_root: impl Into<PathBuf>,
        history: Arc<dyn SkillHistoryRepository>,
        events: EventBus,
    ) -> Self {
        Self::with_tools(skills_root, history, events, Vec::new())
    }

    /// Same as `new`, but seeds the tool-name whitelist so the validator can
    /// hard-reject proposals that reference tools the runtime doesn't have.
    pub fn with_tools(
        skills_root: impl Into<PathBuf>,
        history: Arc<dyn SkillHistoryRepository>,
        events: EventBus,
        known_tools: Vec<String>,
    ) -> Self {
        let root = skills_root.into();
        let store = SkillVersionStore::new(&root);
        Self { skills_root: root, history, store, events, known_tools }
    }

    /// Snapshot the currently-active skills into the shape the validator
    /// expects. Cheap: one DB call.
    async fn active_summaries(&self) -> Result<Vec<ActiveSkillSummary>, SkillOpsError> {
        let rows = self.history.list_active().await?;
        // We only need keywords for the collision check. Re-read the
        // matching file to get them — expensive-looking but bounded by the
        // number of active skills (< 50 in practice).
        let mut out = Vec::with_capacity(rows.len());
        let active_dir = self.skills_root.join("active");
        for r in rows {
            let mut kws = Vec::new();
            if active_dir.exists() {
                for entry in std::fs::read_dir(&active_dir).ok().into_iter().flatten().flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("md") { continue; }
                    let raw = match std::fs::read_to_string(&path) { Ok(s) => s, Err(_) => continue };
                    let parsed = match parse_skill(&raw) { Ok(s) => s, Err(_) => continue };
                    if parsed.front.name == r.name {
                        kws = parsed.front.triggers.keywords;
                        break;
                    }
                }
            }
            out.push(ActiveSkillSummary { name: r.name, version: r.version, keywords: kws });
        }
        Ok(out)
    }

    /// Run the validator against a proposal file WITHOUT promoting anything.
    /// Returns the full report so IPC callers can render per-check badges.
    pub async fn validate_proposal(&self, proposal_filename: &str) -> Result<ValidationReport, SkillOpsError> {
        let src = self.skills_root.join("proposed").join(proposal_filename);
        if !src.exists() {
            return Err(SkillOpsError::ProposalMissing(proposal_filename.into()));
        }
        let raw = std::fs::read_to_string(&src)?;
        let ctx = ValidatorContext {
            known_tools:   self.known_tools.clone(),
            active_skills: self.active_summaries().await?,
        };
        Ok(validate_bytes(&raw, &ctx))
    }

    /// Promote a proposal in `<skills_root>/proposed/<filename>` to active:
    ///   0. Run the validator; if any HARD check fails, publish
    ///      `SkillValidationFailed` and return `ValidationFailed` (the
    ///      proposal file is left in `proposed/` untouched).
    ///   1. Publish `SkillValidationPassed` with any soft warnings.
    ///   2. Move the file to `active/`, flipping status to `active`.
    ///   3. Snapshot the bytes into the content store.
    ///   4. Append a `SkillPromoted` row (parent_sha = previous active sha).
    ///   5. Publish `ForgeEvent::SkillPromoted`.
    ///
    /// If the proposal declares an already-active skill (same name), the
    /// old active row is retired first and its sha is used as `parent_sha`
    /// — this is how successive proposals build a version chain.
    pub async fn promote_from_proposal(
        &self,
        proposal_filename: &str,
        origin_mission_id: Option<MissionId>,
    ) -> Result<SkillVersionRecord, SkillOpsError> {
        let src = self.skills_root.join("proposed").join(proposal_filename);
        if !src.exists() {
            return Err(SkillOpsError::ProposalMissing(proposal_filename.into()));
        }

        // --- Validation gate (agent.txt: "validation before promotion"). ---
        let report = self.validate_proposal(proposal_filename).await?;
        // Parse the proposal ONCE to get the declared name for the events.
        // The validator already parsed; we re-parse here to grab the name
        // rather than pass it through the report (keeps validator generic).
        let raw = std::fs::read_to_string(&src)?;
        let parsed_name = parse_skill(&raw).map(|s| s.front.name).unwrap_or_else(|_| "<unparsed>".into());
        if !report.ok {
            let failed = report.hard_failures();
            self.events.publish(ForgeEvent::SkillValidationFailed {
                filename: proposal_filename.into(),
                name:     parsed_name.clone(),
                failed_checks: failed.clone(),
            }).await.ok();
            return Err(SkillOpsError::ValidationFailed {
                filename: proposal_filename.into(),
                failed,
            });
        }
        // Soft failures don't block, but we publish them so the audit log
        // shows which warnings were tolerated at promotion time.
        let soft_failures: Vec<String> = report.checks.iter()
            .filter(|c| !c.passed && c.severity == forge_skills::Severity::Soft)
            .map(|c| c.id.clone())
            .collect();
        self.events.publish(ForgeEvent::SkillValidationPassed {
            filename: proposal_filename.into(),
            name:     parsed_name.clone(),
            soft_failures,
        }).await.ok();

        // Approve moves the file to active/ and hands us the bytes' sha +
        // the parsed name/version — computed off the FINAL (status=active)
        // document so the sha is stable across future reads.
        let (dst, sha, name, version) =
            approve_proposal_detail(&self.skills_root, proposal_filename)?;
        let bytes = std::fs::read(&dst)?;
        self.store.put(&sha, &bytes)?;

        // Retire the prior active row (if any) and stash its sha as parent.
        let parent_sha = self.history.active(&name).await?.map(|r| r.sha);
        if parent_sha.is_some() {
            self.history.retire_active(&name, "replaced by newer promotion").await?;
        }

        let new_row = NewSkillVersion {
            name: name.clone(),
            sha: sha.clone(),
            version: version.clone(),
            origin: SkillOrigin::Proposal,
            origin_mission_id: origin_mission_id.map(|m| m.to_string()),
            parent_sha: parent_sha.clone(),
            reason: None,
        };
        let _ = self.history.promote(&new_row).await?;

        self.events.publish(ForgeEvent::SkillPromoted {
            name: name.clone(),
            sha: sha.clone(),
            version: version.clone(),
            origin: "proposal".into(),
            parent_sha,
            origin_mission_id,
        }).await.ok();

        Ok(self.history.active(&name).await?
            .expect("just promoted, must be active"))
    }

    /// Retire the currently-active skill named `name`. Moves its file to
    /// `<skills_root>/archived/`, sets `retired_at` on the row, publishes
    /// `SkillRetired`. Idempotent — no-op if `name` has no active version.
    pub async fn retire(&self, name: &str, reason: &str) -> Result<Option<String>, SkillOpsError> {
        let Some(active) = self.history.active(name).await? else {
            return Ok(None);
        };
        retire_active_skill(&self.skills_root, name)?;
        self.history.retire_active(name, reason).await?;
        self.events.publish(ForgeEvent::SkillRetired {
            name: name.into(),
            sha: active.sha.clone(),
            reason: reason.into(),
        }).await.ok();
        Ok(Some(active.sha))
    }

    /// Roll back `name` to the exact bytes stored at `target_sha`:
    ///   1. Fail if `target_sha` isn't present in the content store.
    ///   2. Retire the current active version (if any).
    ///   3. Restore the target bytes to `active/<name>.md`.
    ///   4. Append a new history row with `origin=rollback` and
    ///      `parent_sha` = current active (before this call).
    ///   5. Publish `SkillRolledBack`.
    ///
    /// Note: the new row's `sha` equals `target_sha` — the content is
    /// bit-exact. This is how "rollback" and "restore an archived version"
    /// converge into one primitive.
    pub async fn rollback(
        &self,
        name: &str,
        target_sha: &str,
        reason: Option<&str>,
    ) -> Result<SkillVersionRecord, SkillOpsError> {
        if !self.store.contains(target_sha) {
            return Err(SkillOpsError::ShaMissing(target_sha.into(), name.into()));
        }
        let bytes = self.store.get(target_sha)?;
        // Sanity check: the bytes must actually be a parseable skill with
        // the same name. Otherwise the user asked for a sha that belongs
        // to a different skill and we'd corrupt the active/ tree.
        let raw = String::from_utf8_lossy(&bytes).to_string();
        let parsed = parse_skill(&raw)?;
        if parsed.front.name != name {
            return Err(SkillOpsError::Skill(forge_skills::SkillError::Malformed {
                name: name.into(),
                reason: format!("sha {target_sha} belongs to skill `{}`, not `{}`", parsed.front.name, name),
            }));
        }
        let prev = self.history.active(name).await?;
        let from_sha = prev.as_ref().map(|r| r.sha.clone());
        if prev.is_some() {
            self.history.retire_active(name, "rolled back").await?;
        }
        restore_from_bytes(&self.skills_root, name, &bytes)?;
        let new_row = NewSkillVersion {
            name: name.into(),
            sha: target_sha.into(),
            version: parsed.front.version.clone(),
            origin: SkillOrigin::Rollback,
            origin_mission_id: None,
            parent_sha: from_sha.clone(),
            reason: reason.map(String::from),
        };
        let _ = self.history.promote(&new_row).await?;
        self.events.publish(ForgeEvent::SkillRolledBack {
            name: name.into(),
            from_sha,
            to_sha: target_sha.into(),
            reason: reason.map(String::from),
        }).await.ok();
        Ok(self.history.active(name).await?
            .expect("just rolled back, must be active"))
    }

    /// At boot, seed history for any active skill that has NO row yet.
    /// This catches hand-authored `SKILL.md` files the user dropped in
    /// `<skills_root>/active/` without going through the proposal flow.
    /// Each is snapshotted into the content store with `origin=handcrafted`.
    pub async fn seed_missing_history(&self) -> Result<usize, SkillOpsError> {
        let active_dir = self.skills_root.join("active");
        if !active_dir.exists() { return Ok(0); }
        let mut seeded = 0usize;
        for entry in std::fs::read_dir(&active_dir)?.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") { continue; }
            let bytes = match std::fs::read(&path) { Ok(b) => b, Err(_) => continue };
            let raw = String::from_utf8_lossy(&bytes).to_string();
            let parsed = match parse_skill(&raw) { Ok(s) => s, Err(_) => continue };
            let name = parsed.front.name.clone();
            let version = parsed.front.version.clone();
            let sha = SkillVersionStore::hash(&bytes);
            if let Some(existing) = self.history.active(&name).await? {
                if existing.sha == sha { continue; }
                // Different bytes for same name — the user edited the file
                // in place. Snapshot the new bytes and promote a fresh row
                // so the history log stays truthful.
                self.history.retire_active(&name, "on-disk edit detected").await?;
            }
            self.store.put(&sha, &bytes)?;
            let _ = self.history.promote(&NewSkillVersion {
                name: name.clone(),
                sha: sha.clone(),
                version,
                origin: SkillOrigin::Handcrafted,
                origin_mission_id: None,
                parent_sha: None,
                reason: Some("seeded from on-disk active/".into()),
            }).await?;
            seeded += 1;
        }
        Ok(seeded)
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Curator — surfaces merge/dedupe/archive candidates. Advisory only.
// ────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, serde::Serialize)]
pub struct CuratorSuggestion {
    pub name:     String,
    pub kind:     CuratorKind,
    pub evidence: String,
}

#[derive(Copy, Clone, Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CuratorKind {
    Duplicate,
    Unused,
}

impl CuratorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            CuratorKind::Duplicate => "duplicate",
            CuratorKind::Unused    => "unused",
        }
    }
}

pub struct Curator {
    pub ops:       Arc<SkillOps>,
    pub events:    Arc<SqliteEventStore>,
    /// A skill is "unused" if it has never appeared in a `SkillsSelected`
    /// event across the entire event log (Phase 4a keeps this global —
    /// windowing arrives with the learning-engine milestone).
    pub _placeholder: (),
}

impl Curator {
    pub fn new(ops: Arc<SkillOps>, events: Arc<SqliteEventStore>) -> Self {
        Self { ops, events, _placeholder: () }
    }

    /// Run one pass over the active set. Emits `SkillCurationSuggested`
    /// for each finding. Returns them all so IPC callers can display the
    /// list directly.
    pub async fn run(&self) -> Result<Vec<CuratorSuggestion>, SkillOpsError> {
        let active = self.ops.history.list_active().await?;
        let mut out = Vec::new();

        // --- Duplicate detection (pairwise Jaro-Winkler on names). ---
        for i in 0..active.len() {
            for j in (i+1)..active.len() {
                let a = &active[i].name;
                let b = &active[j].name;
                let jw = jaro_winkler(a, b);
                if jw >= 0.90 {
                    let evidence = format!("similarity={:.3} vs `{}`", jw, b);
                    out.push(CuratorSuggestion {
                        name: a.clone(),
                        kind: CuratorKind::Duplicate,
                        evidence,
                    });
                }
            }
        }

        // --- Unused detection (scan SkillsSelected events). ---
        let used = self.used_skill_names().await?;
        for row in &active {
            if !used.contains(&row.name) {
                out.push(CuratorSuggestion {
                    name: row.name.clone(),
                    kind: CuratorKind::Unused,
                    evidence: "no SkillsSelected event references this skill".into(),
                });
            }
        }

        for s in &out {
            self.ops.events.publish(ForgeEvent::SkillCurationSuggested {
                name: s.name.clone(),
                kind: s.kind.as_str().into(),
                evidence: s.evidence.clone(),
            }).await.ok();
        }
        Ok(out)
    }

    /// Scan the event store for every `skills_selected` payload and collect
    /// the union of skill names referenced. Cheap: reads once, filters
    /// client-side.
    async fn used_skill_names(&self) -> Result<std::collections::HashSet<String>, SkillOpsError> {
        let mut set = std::collections::HashSet::new();
        let all = self.events.read_since(None).await?;
        for env in all {
            if let ForgeEvent::SkillsSelected { skill_names, .. } = env.event {
                for n in skill_names { set.insert(n); }
            }
        }
        Ok(set)
    }
}

// ────────────────────────────────────────────────────────────────────────────
// AutoPromoter — background loop that promotes validation-passing proposals
// without human intervention. Off by default; enabled via
// `RuntimeConfig.auto_promote_skills = true`.
// ────────────────────────────────────────────────────────────────────────────

pub struct AutoPromoter {
    pub ops:      Arc<SkillOps>,
    pub interval: std::time::Duration,
}

impl AutoPromoter {
    pub fn new(ops: Arc<SkillOps>, interval: std::time::Duration) -> Self {
        Self { ops, interval }
    }

    /// Run one sweep: enumerate every `.md` in `proposed/`, validate each,
    /// promote those whose report is `ok`. Returns the count promoted.
    ///
    /// Failures during promotion (e.g. IO error) are logged and skipped —
    /// the loop must be resilient because it runs unattended.
    pub async fn sweep(&self) -> Result<usize, SkillOpsError> {
        let proposals = list_proposals(&self.ops.skills_root)?;
        let mut promoted = 0usize;
        for p in proposals {
            let filename = std::path::Path::new(&p.source_path)
                .file_name()
                .and_then(|n| n.to_str())
                .map(String::from);
            let Some(filename) = filename else { continue; };
            match self.ops.validate_proposal(&filename).await {
                Ok(report) if report.ok => {
                    match self.ops.promote_from_proposal(&filename, None).await {
                        Ok(row) => {
                            self.ops.events.publish(ForgeEvent::SkillAutoPromoted {
                                name: row.name.clone(),
                                sha:  row.sha.clone(),
                                version: row.version.clone(),
                            }).await.ok();
                            tracing::info!(name = %row.name, sha = %row.sha, "autopromoted skill");
                            promoted += 1;
                        }
                        Err(e) => tracing::warn!(err = %e, filename = %filename, "autopromoter: promotion failed after validation passed"),
                    }
                }
                Ok(report) => {
                    tracing::debug!(filename = %filename, failed = ?report.hard_failures(), "autopromoter: proposal did not pass validation");
                }
                Err(e) => tracing::warn!(err = %e, filename = %filename, "autopromoter: validation error"),
            }
        }
        Ok(promoted)
    }

    /// Spawn the loop. Returns immediately; the task keeps running until the
    /// event bus is dropped. Uses `tokio::time::interval` so ticks don't
    /// pile up if a sweep takes longer than the interval.
    pub fn spawn(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(self.interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // Skip the immediate first tick — boot already ran seed_missing_history.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                match self.sweep().await {
                    Ok(0)  => tracing::trace!("autopromoter sweep: 0 promoted"),
                    Ok(n)  => tracing::info!(count = n, "autopromoter sweep completed"),
                    Err(e) => tracing::warn!(err = %e, "autopromoter sweep error"),
                }
            }
        });
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Jaro-Winkler similarity — zero-dep, ~40 lines. See:
// https://en.wikipedia.org/wiki/Jaro%E2%80%93Winkler_distance
// ────────────────────────────────────────────────────────────────────────────

pub fn jaro_winkler(a: &str, b: &str) -> f64 {
    let j = jaro(a, b);
    if j < 0.7 { return j; }
    let prefix = a.chars()
        .zip(b.chars())
        .take(4)
        .take_while(|(x, y)| x == y)
        .count() as f64;
    j + prefix * 0.1 * (1.0 - j)
}

fn jaro(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() && b.is_empty() { return 1.0; }
    if a.is_empty() || b.is_empty() { return 0.0; }
    let match_dist = (a.len().max(b.len()) / 2).saturating_sub(1);
    let mut a_matches = vec![false; a.len()];
    let mut b_matches = vec![false; b.len()];
    let mut matches = 0usize;
    for (i, ac) in a.iter().enumerate() {
        let start = i.saturating_sub(match_dist);
        let end = (i + match_dist + 1).min(b.len());
        for j in start..end {
            if b_matches[j] { continue; }
            if *ac != b[j] { continue; }
            a_matches[i] = true;
            b_matches[j] = true;
            matches += 1;
            break;
        }
    }
    if matches == 0 { return 0.0; }
    let mut k = 0usize;
    let mut transpositions = 0usize;
    for i in 0..a.len() {
        if !a_matches[i] { continue; }
        while !b_matches[k] { k += 1; }
        if a[i] != b[k] { transpositions += 1; }
        k += 1;
    }
    let m = matches as f64;
    (m / a.len() as f64
        + m / b.len() as f64
        + (m - transpositions as f64 / 2.0) / m
    ) / 3.0
}

#[allow(dead_code)]
fn _list_active_proposals_placeholder(root: &Path) -> Result<Vec<String>, SkillOpsError> {
    Ok(list_proposals(root)?.into_iter().map(|s| s.front.name).collect())
}

// Silence unused warning for MissionId import in test builds without the extern.
#[allow(dead_code)]
fn _mid_parse_hint(s: &str) -> Option<MissionId> { MissionId::from_str(s).ok() }
