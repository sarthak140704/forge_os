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
// Curator — Phase 4c: **actionable** merge/dedupe/archive engine.
//
// Phases:
//   4a: Advisory — emitted `SkillCurationSuggested` only. Kept intact for
//       backwards-compat callers.
//   4c: Actionable — same discovery pass but the caller can opt into
//       `act = true` to auto-archive dupes and drop merge proposals into
//       `proposed/`. Wraps existing `SkillOps::retire` + `ProposalWriter`
//       so every action still flows through the append-only history.
//
// Never touches skills that already have a pending proposal (avoids
// double-work with AutoPromoter) and never archives a skill that
// participates in a `skills_selected` event within the recent window.
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
    MergeCandidate,
}

impl CuratorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            CuratorKind::Duplicate      => "duplicate",
            CuratorKind::Unused         => "unused",
            CuratorKind::MergeCandidate => "merge_candidate",
        }
    }
}

/// Tunable thresholds for the Curator. Sensible defaults matched to the
/// smoke suite; override via `RuntimeConfig.curator`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CuratorPolicy {
    /// Name Jaro-Winkler ≥ this → treat as duplicate. Default 0.92.
    pub name_similarity_threshold: f64,
    /// Body Jaccard ≥ this → treat as duplicate. Default 0.85.
    pub body_similarity_threshold: f64,
    /// Body Jaccard in `[merge_low, body_similarity_threshold)` → propose merge.
    /// Default 0.55.
    pub merge_similarity_low: f64,
    /// Skills that appear in a `SkillsSelected` event within the last N
    /// terminal missions are protected from auto-archive. Default 5.
    pub protect_recent_usage_missions: usize,
    /// If true, `Curator::run` immediately applies decisions
    /// (retire dupes, write merge proposals). If false, callers get the
    /// report and choose per-suggestion. Default false.
    pub auto_act: bool,
}

impl Default for CuratorPolicy {
    fn default() -> Self {
        Self {
            name_similarity_threshold: 0.92,
            body_similarity_threshold: 0.85,
            merge_similarity_low:      0.55,
            protect_recent_usage_missions: 5,
            auto_act: false,
        }
    }
}

/// Structured outcome of a scan — what the Curator saw AND what it did.
#[derive(Clone, Debug, serde::Serialize)]
pub struct CuratorReport {
    pub suggestions:      Vec<CuratorSuggestion>,
    /// (archived_name, kept_name) pairs. Empty when `auto_act=false`.
    pub auto_archived:    Vec<(String, String)>,
    /// Proposal filenames written under `proposed/`. Empty when
    /// `auto_act=false`.
    pub merge_proposals:  Vec<String>,
}

pub struct Curator {
    pub ops:       Arc<SkillOps>,
    pub events:    Arc<SqliteEventStore>,
    pub policy:    CuratorPolicy,
}

impl Curator {
    /// Legacy constructor — advisory-only defaults for callers that predate
    /// the policy struct (kept green for existing tests).
    pub fn new(ops: Arc<SkillOps>, events: Arc<SqliteEventStore>) -> Self {
        Self { ops, events, policy: CuratorPolicy::default() }
    }

    pub fn with_policy(
        ops: Arc<SkillOps>,
        events: Arc<SqliteEventStore>,
        policy: CuratorPolicy,
    ) -> Self {
        Self { ops, events, policy }
    }

    /// Legacy shim: run advisory-only, publish `SkillCurationSuggested` for
    /// every finding, return the list. Preserved so existing `Curator::run`
    /// callers keep compiling.
    pub async fn run(&self) -> Result<Vec<CuratorSuggestion>, SkillOpsError> {
        let report = self.scan(false).await?;
        for s in &report.suggestions {
            self.ops.events.publish(ForgeEvent::SkillCurationSuggested {
                name: s.name.clone(),
                kind: s.kind.as_str().into(),
                evidence: s.evidence.clone(),
            }).await.ok();
        }
        Ok(report.suggestions)
    }

    /// Full Phase 4c scan.
    ///
    /// - `apply=false` returns the report only; nothing on disk changes,
    ///   still emits `SkillCurationSuggested` per finding so the timeline
    ///   sees "the curator ran".
    /// - `apply=true` also archives duplicate losers via
    ///   `SkillOps::retire` (emits `SkillRetired` + `SkillAutoArchived`)
    ///   and writes merge proposals via `ProposalWriter` (emits
    ///   `SkillMergeProposed`).
    ///
    /// The `apply` flag lets a UI do "dry run → confirm → apply" without
    /// duplicating logic. When policy.auto_act is true the callers ignore
    /// this arg and pass `true`.
    pub async fn scan(&self, apply: bool) -> Result<CuratorReport, SkillOpsError> {
        let active = self.ops.history.list_active().await?;
        let bodies = self.read_active_bodies(&active).await?;
        let used_recent = self.recently_used_names().await?;
        let pending = self.pending_proposal_names();

        let mut suggestions   = Vec::new();
        let mut auto_archived = Vec::new();
        let mut merge_props   = Vec::new();

        // Pass 1: pairwise dedupe + merge candidates.
        // `archived_this_pass` tracks skills we already retired so we don't
        // then also merge-propose them; also avoids double-retiring a skill
        // that pairs with two others.
        let mut archived_this_pass: std::collections::HashSet<String> = Default::default();

        for i in 0..active.len() {
            for j in (i+1)..active.len() {
                let a = &active[i];
                let b = &active[j];
                if archived_this_pass.contains(&a.name) || archived_this_pass.contains(&b.name) {
                    continue;
                }
                let name_sim = jaro_winkler(&a.name, &b.name);
                let body_a = bodies.get(&a.name).map(|s| s.as_str()).unwrap_or("");
                let body_b = bodies.get(&b.name).map(|s| s.as_str()).unwrap_or("");
                let body_sim = forge_skills::body_similarity(body_a, body_b);
                let subset   = forge_skills::subset_ratio(body_a, body_b);

                // Rule 1 — auto-archive worthy duplicates.
                //   name Jaro-Winkler high OR body Jaccard high OR one is
                //   a proper subset of the other.
                let duplicate = name_sim >= self.policy.name_similarity_threshold
                    || body_sim >= self.policy.body_similarity_threshold
                    || subset >= 0.95;

                if duplicate {
                    let rule = if subset >= 0.95            { "subset_of" }
                               else if body_sim >= self.policy.body_similarity_threshold { "body_similar" }
                               else                        { "name_similar" };
                    let sim  = subset.max(body_sim).max(name_sim);
                    let evidence = format!(
                        "duplicate of `{}` (rule={rule}, name_jw={:.3}, body_jaccard={:.3}, subset={:.3})",
                        b.name, name_sim, body_sim, subset,
                    );
                    suggestions.push(CuratorSuggestion {
                        name: a.name.clone(),
                        kind: CuratorKind::Duplicate,
                        evidence: evidence.clone(),
                    });

                    if apply {
                        // Pick the loser: lower usage first, then lower
                        // semver, then alphabetically later. Never
                        // auto-archive a skill used in the recent window.
                        let (loser, keeper) = self.pick_loser(a, b, &used_recent);
                        if let (Some(loser), Some(keeper)) = (loser, keeper) {
                            if archived_this_pass.contains(&loser.name) { continue; }
                            match self.ops.retire(&loser.name, &format!(
                                "curator auto-archived: {rule} to `{}`", keeper.name
                            )).await {
                                Ok(Some(sha)) => {
                                    self.ops.events.publish(ForgeEvent::SkillAutoArchived {
                                        archived_name: loser.name.clone(),
                                        archived_sha:  sha,
                                        kept_name:     keeper.name.clone(),
                                        similarity:    sim,
                                        rule:          rule.into(),
                                    }).await.ok();
                                    auto_archived.push((loser.name.clone(), keeper.name.clone()));
                                    archived_this_pass.insert(loser.name.clone());
                                }
                                Ok(None) => {}
                                Err(e) => tracing::warn!(err = %e, name = %loser.name, "curator: retire failed"),
                            }
                        }
                    }
                    continue;
                }

                // Rule 2 — merge candidates: bodies moderately overlap but
                // neither dominates. Propose a merged skill (advisory even
                // without apply; only written to disk when apply=true).
                if body_sim >= self.policy.merge_similarity_low
                    && body_sim < self.policy.body_similarity_threshold
                {
                    let evidence = format!(
                        "merge candidate with `{}` (body_jaccard={:.3})",
                        b.name, body_sim,
                    );
                    suggestions.push(CuratorSuggestion {
                        name: a.name.clone(),
                        kind: CuratorKind::MergeCandidate,
                        evidence: evidence.clone(),
                    });

                    if apply {
                        let merged_name = merged_name_of(&a.name, &b.name);
                        // Skip if there's already a pending proposal by
                        // this merged name (idempotent across sweeps).
                        if pending.contains(&merged_name) { continue; }
                        match self.write_merge_proposal(a, b, body_a, body_b).await {
                            Ok(filename) => {
                                self.ops.events.publish(ForgeEvent::SkillMergeProposed {
                                    proposal_filename: filename.clone(),
                                    merged_name:       merged_name.clone(),
                                    source_a:          a.name.clone(),
                                    source_b:          b.name.clone(),
                                    body_similarity:   body_sim,
                                }).await.ok();
                                merge_props.push(filename);
                            }
                            Err(e) => tracing::warn!(err = %e, "curator: merge proposal write failed"),
                        }
                    }
                }
            }
        }

        // Pass 2: unused-in-recent-window flag.
        for row in &active {
            if archived_this_pass.contains(&row.name) { continue; }
            if used_recent.contains(&row.name) { continue; }
            let evidence = format!(
                "not used in the last {} terminal missions", self.policy.protect_recent_usage_missions,
            );
            suggestions.push(CuratorSuggestion {
                name: row.name.clone(),
                kind: CuratorKind::Unused,
                evidence,
            });
        }

        // Always echo the advisory events so the timeline records a scan.
        for s in &suggestions {
            self.ops.events.publish(ForgeEvent::SkillCurationSuggested {
                name: s.name.clone(),
                kind: s.kind.as_str().into(),
                evidence: s.evidence.clone(),
            }).await.ok();
        }

        Ok(CuratorReport {
            suggestions,
            auto_archived,
            merge_proposals: merge_props,
        })
    }

    /// Read the raw markdown bodies of every currently-active skill. Missing
    /// files are silently skipped (the history row wins over disk).
    async fn read_active_bodies(&self, active: &[SkillVersionRecord])
        -> Result<std::collections::HashMap<String, String>, SkillOpsError>
    {
        use forge_skills::parse_skill;
        let mut out: std::collections::HashMap<String, String> = Default::default();
        let dir = self.ops.skills_root.join("active");
        if !dir.exists() { return Ok(out); }
        let by_name: std::collections::HashSet<&str> =
            active.iter().map(|r| r.name.as_str()).collect();
        for entry in std::fs::read_dir(&dir)?.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") { continue; }
            let raw = match std::fs::read_to_string(&path) { Ok(s) => s, Err(_) => continue };
            let parsed = match parse_skill(&raw) { Ok(s) => s, Err(_) => continue };
            if by_name.contains(parsed.front.name.as_str()) {
                out.insert(parsed.front.name, parsed.body);
            }
        }
        Ok(out)
    }

    /// Union of skill names referenced by `SkillsSelected` events tied to
    /// the most recent N *terminal* missions. Falls back to the global
    /// "any mission" set when the terminal count is smaller than the window.
    async fn recently_used_names(&self)
        -> Result<std::collections::HashSet<String>, SkillOpsError>
    {
        use forge_domain::ForgeEvent as FE;
        let all = self.events.read_since(None).await?;
        // Collect (mission_id, is_terminal, timestamp) then pick most-
        // recent-terminal N. Terminal = completed/failed/cancelled.
        let mut terminal_missions: Vec<String> = Vec::new();
        for env in &all {
            if let FE::MissionStatusChanged { id, to, .. } = &env.event {
                let s = format!("{to:?}").to_lowercase();
                if s.contains("completed") || s.contains("failed") || s.contains("cancelled") {
                    terminal_missions.push(id.to_string());
                }
            }
        }
        let recent: std::collections::HashSet<String> = terminal_missions
            .into_iter()
            .rev()
            .take(self.policy.protect_recent_usage_missions)
            .collect();

        // If we have fewer terminal missions than the window, keep the
        // union global — new users shouldn't have every skill flagged as
        // "unused" just because they've only run 2 missions total.
        let global = recent.is_empty();

        let mut used = std::collections::HashSet::new();
        for env in all {
            if let FE::SkillsSelected { mission_id, skill_names } = env.event {
                if global || recent.contains(&mission_id.to_string()) {
                    for n in skill_names { used.insert(n); }
                }
            }
        }
        Ok(used)
    }

    /// Front-matter names of every skill currently sitting in `proposed/`.
    fn pending_proposal_names(&self) -> std::collections::HashSet<String> {
        list_proposals(&self.ops.skills_root)
            .map(|v| v.into_iter().map(|s| s.front.name).collect())
            .unwrap_or_default()
    }

    /// Pick which of two duplicate skills to archive.
    /// Never picks a recently-used skill as the loser.
    fn pick_loser<'r>(
        &self,
        a: &'r SkillVersionRecord,
        b: &'r SkillVersionRecord,
        used_recent: &std::collections::HashSet<String>,
    ) -> (Option<&'r SkillVersionRecord>, Option<&'r SkillVersionRecord>) {
        let a_used = used_recent.contains(&a.name);
        let b_used = used_recent.contains(&b.name);
        // If both were used recently, don't archive either.
        if a_used && b_used { return (None, None); }
        // If exactly one was used, that one wins.
        if a_used { return (Some(b), Some(a)); }
        if b_used { return (Some(a), Some(b)); }
        // Neither used: pick alphabetically later as loser (deterministic).
        if a.name <= b.name { (Some(b), Some(a)) } else { (Some(a), Some(b)) }
    }

    /// Write a merged proposal combining `a` and `b`'s bodies + tools +
    /// keywords. Returns the just-written filename (not full path).
    async fn write_merge_proposal(
        &self,
        a: &SkillVersionRecord,
        b: &SkillVersionRecord,
        body_a: &str,
        body_b: &str,
    ) -> Result<String, SkillOpsError> {
        use forge_skills::{parse_skill, merge_bodies, union_dedup, ProposalWriter, SuggestedSkill};
        // Read tools + keywords straight off disk — the history row doesn't
        // carry them.
        let (tools_a, kws_a) = self.front_tools_keywords(&a.name)?;
        let (tools_b, kws_b) = self.front_tools_keywords(&b.name)?;
        let _ = parse_skill; // silence unused-import warning if parse_skill isn't used elsewhere.
        let merged_name = merged_name_of(&a.name, &b.name);
        let merged_body = merge_bodies(body_a, body_b);
        let suggested = SuggestedSkill {
            name:              merged_name.clone(),
            description:       format!(
                "Merged skill combining `{}` and `{}` (curator auto-generated; review before promoting)",
                a.name, b.name,
            ),
            tools:             union_dedup(&tools_a, &tools_b),
            keywords:          union_dedup(&kws_a, &kws_b),
            body:              merged_body,
            origin_mission_id: format!("curator-merge-{}-{}", a.name, b.name),
        };
        let writer = ProposalWriter::new(&self.ops.skills_root);
        let path = writer.write_proposal(&suggested)?;
        let filename = path.file_name()
            .and_then(|n| n.to_str())
            .map(String::from)
            .unwrap_or_default();
        Ok(filename)
    }

    fn front_tools_keywords(&self, name: &str) -> Result<(Vec<String>, Vec<String>), SkillOpsError> {
        use forge_skills::parse_skill;
        let dir = self.ops.skills_root.join("active");
        if !dir.exists() { return Ok((Vec::new(), Vec::new())); }
        for entry in std::fs::read_dir(&dir)?.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") { continue; }
            let raw = match std::fs::read_to_string(&path) { Ok(s) => s, Err(_) => continue };
            let parsed = match parse_skill(&raw) { Ok(s) => s, Err(_) => continue };
            if parsed.front.name == name {
                return Ok((parsed.front.tools, parsed.front.triggers.keywords));
            }
        }
        Ok((Vec::new(), Vec::new()))
    }
}

/// Deterministic name for the auto-generated merge of two skills. Both
/// orderings collapse to the same output so the pending-proposal dedup
/// works.
fn merged_name_of(a: &str, b: &str) -> String {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    format!("{lo}-plus-{hi}")
}

// ────────────────────────────────────────────────────────────────────────────
// CuratorSweeper — background loop (mirror of AutoPromoter).
// Off by default; enabled via `RuntimeConfig.curator_sweep_enabled = true`.
// ────────────────────────────────────────────────────────────────────────────

pub struct CuratorSweeper {
    pub curator:  Arc<Curator>,
    pub interval: std::time::Duration,
    pub apply:    bool,
}

impl CuratorSweeper {
    pub fn new(curator: Arc<Curator>, interval: std::time::Duration, apply: bool) -> Self {
        Self { curator, interval, apply }
    }

    pub fn spawn(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(self.interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker.tick().await; // skip immediate first tick
            loop {
                ticker.tick().await;
                match self.curator.scan(self.apply).await {
                    Ok(r) => tracing::info!(
                        suggestions   = r.suggestions.len(),
                        auto_archived = r.auto_archived.len(),
                        merge_props   = r.merge_proposals.len(),
                        "curator sweep completed",
                    ),
                    Err(e) => tracing::warn!(err = %e, "curator sweep error"),
                }
            }
        });
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
