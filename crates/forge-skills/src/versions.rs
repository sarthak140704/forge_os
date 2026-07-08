//! Content-addressed skill version store.
//!
//! Every promoted, rolled-back, or curated skill is snapshotted here as a
//! byte-for-byte copy of its SKILL.md, keyed by the SHA-256 of its bytes.
//! Combined with the append-only `skills_history` table in
//! `forge_persistence`, this gives us:
//!
//! - **Version control** — every promotion is a distinct entry that never
//!   overwrites a previous one.
//! - **Bit-exact rollback** — `rollback_skill(name, sha)` restores the
//!   same bytes that were active at that sha's promotion time.
//! - **Reproducibility** — reflection can re-run against the exact skill
//!   text that was live at any past moment.
//!
//! Layout:
//!
//! ```text
//! <skills_root>/
//!   active/            ← live skills (loaded by SkillRegistry)
//!   proposed/          ← awaiting human approval
//!   archived/          ← retired skills (kept for audit; never loaded)
//!   history/           ← content-addressed store (this module)
//!     a3f/
//!       a3f8b1…c07d.md ← full SKILL.md bytes; first 3 chars are shard dir
//! ```
//!
//! History files are keyed by hex SHA-256 with a 3-char shard prefix to
//! avoid ever having tens of thousands of files in one directory.
//!
//! This module does no I/O beyond std::fs; it never touches the DB. The
//! caller (approve/rollback/retire in `proposal.rs` + runtime IPC) is
//! responsible for the `SkillHistoryRepository` writes so that the history
//! row and the file always land together (or both fail).

use crate::SkillError;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub struct SkillVersionStore {
    root: PathBuf,
}

impl SkillVersionStore {
    /// `skills_root` is the parent of `active/`, `proposed/`, etc. The
    /// history store lives at `<skills_root>/history/`.
    pub fn new(skills_root: impl Into<PathBuf>) -> Self {
        Self { root: skills_root.into().join("history") }
    }

    /// SHA-256 of the given bytes, hex-encoded (64 chars, lowercase).
    /// The canonical id we use everywhere for "which version".
    pub fn hash(bytes: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(bytes);
        hex::encode(h.finalize())
    }

    /// Path we'd store `sha` at. Does NOT create the file — see `put`.
    /// Two-level shard prevents any single dir from ballooning.
    pub fn path_for(&self, sha: &str) -> PathBuf {
        let shard = &sha[..3.min(sha.len())];
        self.root.join(shard).join(format!("{sha}.md"))
    }

    /// Write `bytes` under `sha` (idempotent — no-op if already present).
    /// Returns the resolved path.
    pub fn put(&self, sha: &str, bytes: &[u8]) -> Result<PathBuf, SkillError> {
        let path = self.path_for(sha);
        if path.exists() { return Ok(path); }
        if let Some(dir) = path.parent() { std::fs::create_dir_all(dir)?; }
        std::fs::write(&path, bytes)?;
        Ok(path)
    }

    /// Read the raw SKILL.md bytes for `sha`. Returns `Malformed` if the
    /// object isn't present.
    pub fn get(&self, sha: &str) -> Result<Vec<u8>, SkillError> {
        let path = self.path_for(sha);
        if !path.exists() {
            return Err(SkillError::Malformed {
                name:   sha.to_string(),
                reason: format!("history object not found at {}", path.display()),
            });
        }
        Ok(std::fs::read(&path)?)
    }

    /// Does this sha exist in the store?
    pub fn contains(&self, sha: &str) -> bool {
        self.path_for(sha).exists()
    }

    /// Root of the history store — exposed for BUILD_LOG / diagnostics only.
    pub fn root(&self) -> &Path { &self.root }
}

/// Convenience: compute the sha of a file (used at boot to seed history for
/// any hand-authored skill that lacks a prior promotion record).
pub fn hash_file(path: &Path) -> Result<String, SkillError> {
    let bytes = std::fs::read(path)?;
    Ok(SkillVersionStore::hash(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!("forge-vstore-{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn hash_is_stable_and_hex() {
        let a = SkillVersionStore::hash(b"hello world");
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a, SkillVersionStore::hash(b"hello world"));
        assert_ne!(a, SkillVersionStore::hash(b"hello world!"));
    }

    #[test]
    fn put_get_roundtrip_and_idempotent() {
        let root = tmp();
        let store = SkillVersionStore::new(&root);
        let sha = SkillVersionStore::hash(b"skill body v1");
        let p1  = store.put(&sha, b"skill body v1").unwrap();
        assert!(p1.exists());
        assert!(store.contains(&sha));
        // Second put with same sha is a no-op — no error, no rewrite.
        let p2 = store.put(&sha, b"skill body v1").unwrap();
        assert_eq!(p1, p2);
        let bytes = store.get(&sha).unwrap();
        assert_eq!(bytes, b"skill body v1");
    }

    #[test]
    fn get_missing_returns_malformed() {
        let root = tmp();
        let store = SkillVersionStore::new(&root);
        let err = store.get("deadbeef").unwrap_err();
        matches!(err, SkillError::Malformed { .. });
    }
}
