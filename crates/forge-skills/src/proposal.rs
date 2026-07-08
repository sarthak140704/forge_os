//! Writes reflection-suggested skills to disk for human review.
//!
//! The reflector produces a `SuggestedSkill` — a mostly-complete skill draft.
//! The proposal writer serializes it as a `SKILL.md` file with
//! `status: pending_review`, placed under `skills_root/proposed/`.
//!
//! Approving a proposal is a *file-move* operation (see `approve_proposal`).
//! We do not auto-promote — the point of Phase 2 is to keep learning
//! observable and reversible, not to let the agent silently rewrite its own
//! playbooks.

use crate::{Skill, SkillError, SkillFrontMatter, SkillStatus, SkillTrigger};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A skill draft produced by the reflector. Fields mirror `SkillFrontMatter`
/// plus a Markdown body.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SuggestedSkill {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    pub body: String,
    #[serde(default)]
    pub origin_mission_id: String,
}

pub struct ProposalWriter {
    root: PathBuf,
}

impl ProposalWriter {
    pub fn new(skills_root: impl Into<PathBuf>) -> Self {
        Self { root: skills_root.into() }
    }

    /// Cheap read of every already-pending proposal's declared `name` (from
    /// the front-matter). Used for dedup at reflection time. Errors while
    /// parsing individual files are logged and skipped so one malformed
    /// proposal never blocks the check.
    pub fn list_proposal_names(&self) -> Result<Vec<String>, SkillError> {
        let dir = self.root.join("proposed");
        if !dir.exists() { return Ok(Vec::new()); }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") { continue; }
            match std::fs::read_to_string(&path)
                .map_err(SkillError::from)
                .and_then(|s| crate::parse_skill(&s))
            {
                Ok(skill) => out.push(skill.front.name),
                Err(e) => {
                    tracing::warn!(path = %path.display(), err = %e, "skipping malformed proposal during dedup scan");
                }
            }
        }
        Ok(out)
    }

    /// Serialize the suggestion and drop it in `skills_root/proposed/`.
    /// Returns the path written.
    pub fn write_proposal(&self, s: &SuggestedSkill) -> Result<PathBuf, SkillError> {
        let dir = self.root.join("proposed");
        std::fs::create_dir_all(&dir)?;

        let ts = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "unknown".into())
            .replace(':', "-");
        let filename = format!("{}-{ts}.md", sanitize(&s.name));
        let path = dir.join(filename);

        let front = SkillFrontMatter {
            name: s.name.clone(),
            version: "0.1.0".into(),
            description: s.description.clone(),
            status: SkillStatus::PendingReview,
            tools: s.tools.clone(),
            triggers: SkillTrigger {
                keywords: s.keywords.clone(),
                file_globs: Vec::new(),
            },
            inputs: Vec::new(),
            outputs: Vec::new(),
        };
        let yaml = serde_yaml::to_string(&front)?;
        let mut doc = String::with_capacity(yaml.len() + s.body.len() + 32);
        doc.push_str("---\n");
        doc.push_str(&yaml);
        if !yaml.ends_with('\n') { doc.push('\n'); }
        doc.push_str("---\n");
        if !s.origin_mission_id.is_empty() {
            doc.push_str(&format!("<!-- proposed from mission {} -->\n\n", s.origin_mission_id));
        }
        doc.push_str(s.body.trim());
        doc.push('\n');

        std::fs::write(&path, doc)?;
        tracing::info!(path = %path.display(), name = %s.name, "wrote skill proposal");
        Ok(path)
    }
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect()
}

/// Move a proposal file from `proposed/` to the active root. Also rewrites
/// the front-matter `status` to `active`. Returns `(dst_path, sha, name, version)`
/// so the caller can immediately record the promotion in `skills_history` +
/// snapshot the bytes in the content store.
///
/// This is the byte-manipulation half of the promotion. See
/// `runtime::skills::promote_from_proposal` for the full flow that also
/// touches history + version store + event bus.
pub fn approve_proposal(skills_root: &Path, proposal_filename: &str) -> Result<PathBuf, SkillError> {
    let (dst, _sha, _name, _version) = approve_proposal_detail(skills_root, proposal_filename)?;
    Ok(dst)
}

/// Rich variant of [`approve_proposal`] used by the runtime/IPC layer to
/// record a full history row alongside the file move.
///
/// The returned tuple is `(active_path, content_sha, skill_name, version)`.
/// `content_sha` is the SHA-256 of the exact bytes written to `active/`
/// (front-matter flipped to `status: active` before hashing so it matches
/// what the loader will subsequently see).
pub fn approve_proposal_detail(
    skills_root: &Path,
    proposal_filename: &str,
) -> Result<(PathBuf, String, String, String), SkillError> {
    let src = skills_root.join("proposed").join(proposal_filename);
    if !src.exists() {
        return Err(SkillError::Malformed {
            name: proposal_filename.to_string(),
            reason: "no such proposal".into(),
        });
    }
    let raw = std::fs::read_to_string(&src)?;
    let mut skill = crate::parse_skill(&raw)?;
    skill.front.status = SkillStatus::Active;

    let yaml = serde_yaml::to_string(&skill.front)?;
    let mut doc = String::with_capacity(yaml.len() + skill.body.len() + 32);
    doc.push_str("---\n");
    doc.push_str(&yaml);
    if !yaml.ends_with('\n') { doc.push('\n'); }
    doc.push_str("---\n");
    doc.push_str(&skill.body);
    doc.push('\n');

    let sha = crate::versions::SkillVersionStore::hash(doc.as_bytes());
    let name = skill.front.name.clone();
    let version = skill.front.version.clone();

    let dst_dir = skills_root.join("active");
    std::fs::create_dir_all(&dst_dir)?;
    let dst = dst_dir.join(proposal_filename);
    std::fs::write(&dst, doc)?;
    std::fs::remove_file(&src)?;
    Ok((dst, sha, name, version))
}

/// Move every active file whose front-matter `name` matches to
/// `<skills_root>/archived/`. Idempotent — returns the count moved (0 if
/// none). Prior versions of this helper stopped after the first hit, but
/// the promote→rollback→retire path can legitimately leave multiple stale
/// `.md` files in `active/` for the same skill (proposal timestamped
/// filenames + the canonical `<name>.md` produced by `restore_from_bytes`).
/// Retiring must clean them all up so a later `promote` doesn't resurrect
/// a stale sha.
pub fn retire_active_skill(skills_root: &Path, name: &str) -> Result<usize, SkillError> {
    let active_dir = skills_root.join("active");
    if !active_dir.exists() { return Ok(0); }
    let mut moved = 0usize;
    for entry in std::fs::read_dir(&active_dir)?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") { continue; }
        let raw = match std::fs::read_to_string(&path) { Ok(r) => r, Err(_) => continue };
        let parsed = match crate::parse_skill(&raw) { Ok(s) => s, Err(_) => continue };
        if parsed.front.name != name { continue; }
        let dst_dir = skills_root.join("archived");
        std::fs::create_dir_all(&dst_dir)?;
        let file_name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let mut dst = dst_dir.join(&file_name);
        // On name collision in archived/, disambiguate by suffixing a counter.
        let mut n = 1usize;
        while dst.exists() {
            dst = dst_dir.join(format!("{n}-{file_name}"));
            n += 1;
        }
        std::fs::rename(&path, &dst)?;
        moved += 1;
    }
    Ok(moved)
}

/// Restore the exact bytes stored at `sha` to `<skills_root>/active/<name>.md`,
/// replacing whatever active version of `name` currently exists. Used by
/// `rollback_skill`. Returns the written active path.
pub fn restore_from_bytes(
    skills_root: &Path,
    name: &str,
    bytes: &[u8],
) -> Result<PathBuf, SkillError> {
    // Move any current active version aside first.
    let _ = retire_active_skill(skills_root, name)?;
    let dst_dir = skills_root.join("active");
    std::fs::create_dir_all(&dst_dir)?;
    // Use a stable filename derived from the skill name (roll-back writes
    // one file, not a timestamped one).
    let filename = format!("{}.md", sanitize(name));
    let dst = dst_dir.join(&filename);
    std::fs::write(&dst, bytes)?;
    Ok(dst)
}

/// Delete a proposal outright.
pub fn reject_proposal(skills_root: &Path, proposal_filename: &str) -> Result<(), SkillError> {
    let path = skills_root.join("proposed").join(proposal_filename);
    if path.exists() { std::fs::remove_file(&path)?; }
    Ok(())
}

/// Inspect proposals without loading them into the registry. Returns the
/// parsed skills together with their absolute paths.
pub fn list_proposals(skills_root: &Path) -> Result<Vec<Skill>, SkillError> {
    let dir = skills_root.join("proposed");
    if !dir.exists() { return Ok(Vec::new()); }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") { continue; }
        let raw = std::fs::read_to_string(&path)?;
        match crate::parse_skill(&raw) {
            Ok(mut s) => {
                s.source_path = path.to_string_lossy().to_string();
                out.push(s);
            }
            Err(e) => tracing::warn!(path = %path.display(), err = %e, "malformed proposal; skipping"),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!("forge-proposal-{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn writes_valid_skill_file_and_reparses() {
        let root = tmp();
        let w = ProposalWriter::new(&root);
        let path = w.write_proposal(&SuggestedSkill {
            name: "deploy-runbook".into(),
            description: "How we deploy".into(),
            tools: vec!["shell.run".into()],
            keywords: vec!["deploy".into(), "release".into()],
            body: "1. Bump version\n2. Push tag\n".into(),
            origin_mission_id: "m-123".into(),
        }).unwrap();

        assert!(path.exists());
        let raw = std::fs::read_to_string(&path).unwrap();
        let parsed = crate::parse_skill(&raw).unwrap();
        assert_eq!(parsed.front.name, "deploy-runbook");
        assert_eq!(parsed.front.status, SkillStatus::PendingReview);
        assert!(parsed.body.contains("Bump version"));
    }

    #[test]
    fn approve_moves_to_active_and_flips_status() {
        let root = tmp();
        let w = ProposalWriter::new(&root);
        let path = w.write_proposal(&SuggestedSkill {
            name: "t".into(), description: "d".into(),
            tools: vec![], keywords: vec!["t".into()],
            body: "b".into(), origin_mission_id: String::new(),
        }).unwrap();

        let file = path.file_name().unwrap().to_string_lossy().to_string();
        let dst = approve_proposal(&root, &file).unwrap();
        assert!(dst.starts_with(root.join("active")));
        assert!(!path.exists());
        let raw = std::fs::read_to_string(&dst).unwrap();
        let parsed = crate::parse_skill(&raw).unwrap();
        assert_eq!(parsed.front.status, SkillStatus::Active);
    }

    #[test]
    fn reject_deletes_file() {
        let root = tmp();
        let w = ProposalWriter::new(&root);
        let path = w.write_proposal(&SuggestedSkill {
            name: "t".into(), description: "d".into(),
            tools: vec![], keywords: vec![], body: "b".into(), origin_mission_id: String::new(),
        }).unwrap();
        let file = path.file_name().unwrap().to_string_lossy().to_string();
        reject_proposal(&root, &file).unwrap();
        assert!(!path.exists());
    }
}
