//! Shadow-git checkpoints — automatic workspace snapshots for 1-click revert.
//!
//! Every filesystem mutation performed by a task (via `file_write`,
//! `create_directory`, `delete_file`, `edit_file`, MCP filesystem writes, …)
//! triggers a commit into a *shadow* git repository that lives OUTSIDE the
//! workspace so users' own `.git/` isn't disturbed.
//!
//! Design:
//!   * Shadow repo lives at `<app-data>/checkpoints/.git`.
//!   * Workspace is treated as the work tree via
//!     `git --git-dir=<shadow>/.git --work-tree=<workspace>`.
//!   * Commits are made with a synthetic author (`Forge OS <forge@localhost>`)
//!     so they never collide with the user's identity.
//!   * A `.gitignore` in the workspace is honored — but we deliberately do NOT
//!     write one; the user can add exclusions manually if they want to skip
//!     e.g. `node_modules/`.
//!   * A checkpoint records `mission_id`, `task_id`, and `tool` in the commit
//!     trailer so `list_checkpoints` can filter by mission.
//!
//! Failure model: git errors are non-fatal. If git is missing, the shadow
//! path is on a read-only mount, or the workspace is huge, we log a warning
//! and continue. Mutations are never blocked by checkpoint failures.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use tokio::sync::Mutex;

const AUTHOR: &str = "Forge OS <forge@localhost>";

#[derive(Clone, Debug, Serialize)]
pub struct Checkpoint {
    pub sha:         String,
    pub short_sha:   String,
    pub subject:     String,
    pub timestamp:   String,
    pub mission_id:  Option<String>,
    pub task_id:     Option<String>,
    pub tool:        Option<String>,
    pub files_changed: usize,
}

/// Thin wrapper around `git` CLI, sequenced through a mutex so concurrent
/// task completions don't race on `git add` / `git commit`.
#[derive(Clone)]
pub struct CheckpointStore {
    workspace:  PathBuf,
    git_dir:    PathBuf,
    lock:       Arc<Mutex<()>>,
    enabled:    bool,
}

impl CheckpointStore {
    /// Initialize (or reuse) a shadow repo. Best-effort — if `git init`
    /// fails, the store is created in a disabled state and every commit is a no-op.
    pub fn init(workspace: PathBuf, git_dir: PathBuf) -> Self {
        let enabled = match Self::try_init(&workspace, &git_dir) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(err = %e, git_dir = %git_dir.display(), "shadow-git init failed; checkpoints disabled");
                false
            }
        };
        Self {
            workspace,
            git_dir,
            lock: Arc::new(Mutex::new(())),
            enabled,
        }
    }

    fn try_init(workspace: &Path, git_dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(workspace).map_err(|e| format!("create workspace: {e}"))?;
        std::fs::create_dir_all(git_dir).map_err(|e| format!("create shadow git dir: {e}"))?;
        // Layout: git_dir IS the git metadata directory (HEAD/refs/config live
        // directly inside it) — passed as the path to `git init --bare`. We
        // then flip `core.bare=false` so working-tree commands (add/commit/
        // reset/clean) work, and point `core.worktree` at the user workspace.
        let head = git_dir.join("HEAD");
        if !head.exists() {
            let out = Command::new("git")
                .arg("init")
                .arg("--bare")
                .arg("--initial-branch=forge-shadow")
                .arg(git_dir)
                .output()
                .map_err(|e| format!("git init: {e}"))?;
            if !out.status.success() {
                return Err(format!("git init failed: {}", String::from_utf8_lossy(&out.stderr)));
            }
            // Flip out of bare mode so `git add` / `git commit` work.
            let out = Command::new("git")
                .arg("--git-dir").arg(git_dir)
                .arg("config").arg("core.bare").arg("false")
                .output().map_err(|e| format!("git config: {e}"))?;
            if !out.status.success() {
                return Err(format!("git config core.bare failed: {}", String::from_utf8_lossy(&out.stderr)));
            }
            // Point at the real workspace as the work tree.
            let set_worktree = Command::new("git")
                .arg("--git-dir").arg(git_dir)
                .arg("config").arg("core.worktree").arg(workspace)
                .output().map_err(|e| format!("git config: {e}"))?;
            if !set_worktree.status.success() {
                return Err(format!("git config core.worktree failed: {}", String::from_utf8_lossy(&set_worktree.stderr)));
            }
            // Disable line-ending normalization so Windows CRLF files aren't
            // rejected by safecrlf and don't get silently rewritten. The
            // shadow repo is a byte-exact snapshot store, not source control.
            for (k, v) in [("core.autocrlf", "false"), ("core.safecrlf", "false")] {
                let _ = Command::new("git")
                    .arg("--git-dir").arg(git_dir)
                    .arg("config").arg(k).arg(v)
                    .output();
            }
            // Seed the tree with an empty root commit so subsequent commits
            // always have a parent and `git log` works even before any change.
            let _ = Command::new("git")
                .arg("--git-dir").arg(git_dir)
                .arg("--work-tree").arg(workspace)
                .arg("commit").arg("--allow-empty").arg("-m").arg("forge: initial shadow checkpoint")
                .arg("--author").arg(AUTHOR)
                .env("GIT_AUTHOR_NAME", "Forge OS")
                .env("GIT_AUTHOR_EMAIL", "forge@localhost")
                .env("GIT_COMMITTER_NAME", "Forge OS")
                .env("GIT_COMMITTER_EMAIL", "forge@localhost")
                .output();
        }
        Ok(())
    }

    fn base_cmd(&self) -> Command {
        let mut c = Command::new("git");
        c.arg("--git-dir").arg(&self.git_dir)
         .arg("--work-tree").arg(&self.workspace)
         .env("GIT_AUTHOR_NAME", "Forge OS")
         .env("GIT_AUTHOR_EMAIL", "forge@localhost")
         .env("GIT_COMMITTER_NAME", "Forge OS")
         .env("GIT_COMMITTER_EMAIL", "forge@localhost");
        c
    }

    /// Snapshot the current workspace. `label` becomes the commit subject.
    /// Trailers embed mission/task/tool for later filtering.
    /// Returns the commit SHA, or `Ok(None)` if there were no changes to commit.
    pub async fn commit(
        &self,
        label: &str,
        mission_id: Option<&str>,
        task_id: Option<&str>,
        tool: Option<&str>,
    ) -> Result<Option<String>, String> {
        if !self.enabled { return Ok(None); }
        let _guard = self.lock.lock().await;

        // `git add -A` stages everything (new/modified/deleted). The workspace
        // may be empty on first mission — `add` still succeeds.
        let add = self.base_cmd().arg("add").arg("-A").output()
            .map_err(|e| format!("git add: {e}"))?;
        if !add.status.success() {
            return Err(format!("git add failed: {}", String::from_utf8_lossy(&add.stderr)));
        }

        // Nothing to commit? Return None so callers don't spam ForgeEvent::CheckpointCreated.
        let diff = self.base_cmd().arg("diff").arg("--cached").arg("--name-only").output()
            .map_err(|e| format!("git diff: {e}"))?;
        if diff.stdout.is_empty() {
            return Ok(None);
        }

        let mut msg = String::from(label);
        msg.push_str("\n\n");
        if let Some(m) = mission_id { msg.push_str(&format!("Forge-Mission-Id: {m}\n")); }
        if let Some(t) = task_id    { msg.push_str(&format!("Forge-Task-Id: {t}\n")); }
        if let Some(t) = tool       { msg.push_str(&format!("Forge-Tool: {t}\n")); }

        let commit = self.base_cmd().arg("commit")
            .arg("--allow-empty-message")
            .arg("-m").arg(&msg)
            .arg("--author").arg(AUTHOR)
            .output().map_err(|e| format!("git commit: {e}"))?;
        if !commit.status.success() {
            return Err(format!("git commit failed: {}", String::from_utf8_lossy(&commit.stderr)));
        }

        // Read back the new HEAD sha.
        let sha_out = self.base_cmd().arg("rev-parse").arg("HEAD").output()
            .map_err(|e| format!("git rev-parse: {e}"))?;
        let sha = String::from_utf8_lossy(&sha_out.stdout).trim().to_string();
        Ok(Some(sha))
    }

    /// Return the most recent N checkpoints, newest first.
    /// Optional `mission_id` filters by the `Forge-Mission-Id` trailer.
    pub async fn list(&self, limit: usize, mission_id: Option<&str>) -> Result<Vec<Checkpoint>, String> {
        if !self.enabled { return Ok(vec![]); }
        let _guard = self.lock.lock().await;
        // Format: <sha>\x1f<short>\x1f<subject>\x1f<iso date>\x1f<body>\x1e
        let out = self.base_cmd()
            .arg("log")
            .arg(format!("-n{}", limit.max(1)))
            .arg("--format=%H\x1f%h\x1f%s\x1f%aI\x1f%b\x1e")
            .arg("--shortstat")
            .output().map_err(|e| format!("git log: {e}"))?;
        if !out.status.success() {
            return Err(format!("git log failed: {}", String::from_utf8_lossy(&out.stderr)));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut result: Vec<Checkpoint> = Vec::new();
        for record in text.split('\x1e') {
            let record = record.trim();
            if record.is_empty() { continue; }
            let mut fields = record.splitn(5, '\x1f');
            let sha       = fields.next().unwrap_or("").trim().to_string();
            let short_sha = fields.next().unwrap_or("").trim().to_string();
            let subject   = fields.next().unwrap_or("").trim().to_string();
            let ts        = fields.next().unwrap_or("").trim().to_string();
            let tail      = fields.next().unwrap_or("");
            if sha.is_empty() { continue; }
            let mut mid = None;
            let mut tid = None;
            let mut tool = None;
            let mut files_changed = 0usize;
            for line in tail.lines() {
                let l = line.trim();
                if let Some(v) = l.strip_prefix("Forge-Mission-Id:") { mid = Some(v.trim().to_string()); }
                else if let Some(v) = l.strip_prefix("Forge-Task-Id:")    { tid = Some(v.trim().to_string()); }
                else if let Some(v) = l.strip_prefix("Forge-Tool:")       { tool = Some(v.trim().to_string()); }
                else if l.contains("file changed") || l.contains("files changed") {
                    // e.g. " 2 files changed, 8 insertions(+), 1 deletion(-)"
                    if let Some(n) = l.split_whitespace().next().and_then(|s| s.parse::<usize>().ok()) {
                        files_changed = n;
                    }
                }
            }
            if let Some(filter) = mission_id {
                if mid.as_deref() != Some(filter) { continue; }
            }
            result.push(Checkpoint {
                sha, short_sha, subject, timestamp: ts,
                mission_id: mid, task_id: tid, tool, files_changed,
            });
        }
        Ok(result)
    }

    /// Hard-reset the workspace to `sha`. Also removes untracked files
    /// created since — this is a destructive operation exposed as a big
    /// red button in the UI.
    pub async fn revert(&self, sha: &str) -> Result<(), String> {
        if !self.enabled { return Err("checkpoints disabled".into()); }
        let _guard = self.lock.lock().await;
        let reset = self.base_cmd().arg("reset").arg("--hard").arg(sha).output()
            .map_err(|e| format!("git reset: {e}"))?;
        if !reset.status.success() {
            return Err(format!("git reset failed: {}", String::from_utf8_lossy(&reset.stderr)));
        }
        let clean = self.base_cmd().arg("clean").arg("-fd").output()
            .map_err(|e| format!("git clean: {e}"))?;
        if !clean.status.success() {
            return Err(format!("git clean failed: {}", String::from_utf8_lossy(&clean.stderr)));
        }
        Ok(())
    }

    pub fn is_enabled(&self) -> bool { self.enabled }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // Skip these tests if git isn't on PATH.
    fn git_available() -> bool {
        Command::new("git").arg("--version").output().is_ok()
    }

    #[tokio::test]
    async fn init_and_commit_and_list_roundtrip() {
        if !git_available() { return; }
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let git_dir = tmp.path().join("shadow").join(".git");
        std::fs::create_dir_all(&workspace).unwrap();
        let store = CheckpointStore::init(workspace.clone(), git_dir.clone());
        assert!(store.is_enabled());

        // Write a file, commit.
        std::fs::write(workspace.join("hello.txt"), "hi").unwrap();
        let sha = store.commit("task: write hello.txt", Some("msn_test"), Some("tsk_1"), Some("file_write")).await.unwrap();
        assert!(sha.is_some());

        let list = store.list(10, None).await.unwrap();
        assert!(list.iter().any(|c| c.mission_id.as_deref() == Some("msn_test")));

        // Filter.
        let filtered = store.list(10, Some("msn_test")).await.unwrap();
        assert!(!filtered.is_empty());
        let none = store.list(10, Some("nope")).await.unwrap();
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn empty_commit_returns_none() {
        if !git_available() { return; }
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let git_dir = tmp.path().join("shadow").join(".git");
        std::fs::create_dir_all(&workspace).unwrap();
        let store = CheckpointStore::init(workspace, git_dir);
        // Nothing changed since the init commit.
        let sha = store.commit("noop", None, None, None).await.unwrap();
        assert!(sha.is_none());
    }

    #[tokio::test]
    async fn revert_restores_file_contents() {
        if !git_available() { return; }
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let git_dir = tmp.path().join("shadow").join(".git");
        std::fs::create_dir_all(&workspace).unwrap();
        let store = CheckpointStore::init(workspace.clone(), git_dir);
        std::fs::write(workspace.join("a.txt"), "original").unwrap();
        let sha = store.commit("v1", None, None, None).await.unwrap().unwrap();
        std::fs::write(workspace.join("a.txt"), "changed").unwrap();
        let _ = store.commit("v2", None, None, None).await.unwrap();
        store.revert(&sha).await.unwrap();
        let restored = std::fs::read_to_string(workspace.join("a.txt")).unwrap();
        assert_eq!(restored, "original");
    }
}
