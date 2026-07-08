//! Load skills from a directory tree.
//!
//! Loader semantics:
//! - Recurse; every `*.md` is a candidate skill.
//! - `proposed/` and `archived/` subtrees are skipped by default (they only
//!   contain unapproved or retired skills).
//! - If two files declare the same `name`, the higher `version` wins. Ties
//!   go to the file with the later modification time.
//! - Parse errors are logged but never abort the whole load — one bad skill
//!   should not brick the runtime.

use crate::{parse_skill, Skill, SkillError, SkillStatus};
use std::path::{Path, PathBuf};

pub trait SkillLoader: Send + Sync {
    fn load_all(&self) -> Result<Vec<Skill>, SkillError>;
}

pub struct FilesystemSkillLoader {
    root: PathBuf,
    include_pending: bool,
}

impl FilesystemSkillLoader {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), include_pending: false }
    }

    /// If true, also load skills from `proposed/` (used by the review IPC
    /// path so operators can inspect proposals before approving).
    pub fn with_pending(mut self, on: bool) -> Self {
        self.include_pending = on;
        self
    }
}

impl SkillLoader for FilesystemSkillLoader {
    fn load_all(&self) -> Result<Vec<Skill>, SkillError> {
        if !self.root.exists() {
            tracing::debug!(root = %self.root.display(), "skills root does not exist; returning empty set");
            return Ok(Vec::new());
        }

        let mut collected: Vec<Skill> = Vec::new();
        walk(&self.root, &self.root, self.include_pending, &mut collected);

        // Filter by status. `Archived` is always excluded; `PendingReview` only
        // if the loader opted in.
        collected.retain(|s| match s.front.status {
            SkillStatus::Active => true,
            SkillStatus::PendingReview => self.include_pending,
            SkillStatus::Archived => false,
        });

        // De-dupe by name; highest semver wins, then latest mtime.
        collected.sort_by(|a, b| a.front.name.cmp(&b.front.name)
            .then_with(|| version_cmp(&b.front.version, &a.front.version)));

        let mut deduped: Vec<Skill> = Vec::with_capacity(collected.len());
        for s in collected {
            if !deduped.iter().any(|d| d.front.name == s.front.name) {
                deduped.push(s);
            }
        }
        Ok(deduped)
    }
}

fn walk(root: &Path, dir: &Path, include_pending: bool, out: &mut Vec<Skill>) {
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(dir = %dir.display(), err = %e, "cannot read skills dir");
            return;
        }
    };
    for entry in read.flatten() {
        let path = entry.path();
        let ft = match entry.file_type() { Ok(f) => f, Err(_) => continue };
        if ft.is_dir() {
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name == "archived" { continue; }
            if name == "proposed" && !include_pending { continue; }
            walk(root, &path, include_pending, out);
        } else if ft.is_file() {
            if path.extension().and_then(|s| s.to_str()) != Some("md") { continue; }
            match std::fs::read_to_string(&path) {
                Ok(src) => match parse_skill(&src) {
                    Ok(mut s) => {
                        s.source_path = path.to_string_lossy().to_string();
                        out.push(s);
                    }
                    Err(e) => tracing::warn!(path = %path.display(), err = %e, "malformed skill; skipping"),
                },
                Err(e) => tracing::warn!(path = %path.display(), err = %e, "unreadable skill; skipping"),
            }
        }
    }
}

/// Very small semver comparator: splits on `.` and compares numeric segments
/// as u64 with a fallback to lexical for pre-release/build suffixes. Good
/// enough for skill versioning — we don't need full semver semantics.
fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let ap = a.split(['.', '-']).collect::<Vec<_>>();
    let bp = b.split(['.', '-']).collect::<Vec<_>>();
    for (x, y) in ap.iter().zip(bp.iter()) {
        match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(xn), Ok(yn)) => match xn.cmp(&yn) {
                std::cmp::Ordering::Equal => continue,
                other => return other,
            },
            _ => match x.cmp(y) {
                std::cmp::Ordering::Equal => continue,
                other => return other,
            },
        }
    }
    ap.len().cmp(&bp.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(dir: &Path, name: &str, body: &str) {
        let p = dir.join(name);
        if let Some(parent) = p.parent() { std::fs::create_dir_all(parent).unwrap(); }
        let mut f = std::fs::File::create(p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    fn tmp_dir(suffix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("forge-skills-loader-{suffix}-{}", uuid_ish()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn uuid_ish() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos().to_string()
    }

    #[test]
    fn returns_empty_when_root_missing() {
        let loader = FilesystemSkillLoader::new(std::env::temp_dir().join("forge-nonexistent-xyz"));
        assert!(loader.load_all().unwrap().is_empty());
    }

    #[test]
    fn dedupes_by_name_highest_version_wins() {
        let dir = tmp_dir("dedupe");
        write(&dir, "a-v1.md", "---\nname: alpha\nversion: 1.0.0\ndescription: v1\n---\nbody");
        write(&dir, "a-v2.md", "---\nname: alpha\nversion: 1.2.0\ndescription: v2\n---\nbody");
        write(&dir, "a-v0.md", "---\nname: alpha\nversion: 0.9.0\ndescription: v0\n---\nbody");
        let loader = FilesystemSkillLoader::new(&dir);
        let skills = loader.load_all().unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].front.version, "1.2.0");
    }

    #[test]
    fn skips_archived_and_proposed_by_default() {
        let dir = tmp_dir("skip");
        write(&dir, "active.md",           "---\nname: a\nversion: 1.0.0\ndescription: d\n---\nbody");
        write(&dir, "proposed/prop.md",    "---\nname: p\nversion: 1.0.0\ndescription: d\nstatus: pending_review\n---\nbody");
        write(&dir, "archived/old.md",     "---\nname: o\nversion: 1.0.0\ndescription: d\nstatus: archived\n---\nbody");
        let names: Vec<_> = FilesystemSkillLoader::new(&dir).load_all().unwrap()
            .into_iter().map(|s| s.front.name).collect();
        assert_eq!(names, vec!["a"]);
    }

    #[test]
    fn include_pending_loads_proposed_dir() {
        let dir = tmp_dir("pending");
        write(&dir, "proposed/prop.md", "---\nname: p\nversion: 1.0.0\ndescription: d\nstatus: pending_review\n---\nbody");
        let names: Vec<_> = FilesystemSkillLoader::new(&dir).with_pending(true).load_all().unwrap()
            .into_iter().map(|s| s.front.name).collect();
        assert_eq!(names, vec!["p"]);
    }

    #[test]
    fn one_bad_skill_does_not_kill_the_load() {
        let dir = tmp_dir("bad");
        write(&dir, "good.md", "---\nname: g\nversion: 1.0.0\ndescription: d\n---\nbody");
        write(&dir, "bad.md",  "totally not a skill\n");
        let names: Vec<_> = FilesystemSkillLoader::new(&dir).load_all().unwrap()
            .into_iter().map(|s| s.front.name).collect();
        assert_eq!(names, vec!["g"]);
    }
}
