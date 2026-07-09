//! User memory — personal preferences and workflow notes that follow the user
//! across every workspace. Loaded once at boot and injected into every planner
//! prompt just below the project memory block.
//!
//! Lookup order (first hit wins):
//!  1. `$FORGE_USER_MEMORY` (explicit override)
//!  2. `%APPDATA%\com.sarthak.forgeos\user.md`  (Tauri app data dir on Windows)
//!  3. `~/.forge/user.md`                       (POSIX-style fallback)
//!
//! Truncated at 4 KB — smaller than project memory because it's global.

use std::path::{Path, PathBuf};

const MAX_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug)]
pub struct UserMemory {
    pub source:    PathBuf,
    pub content:   String,
    pub truncated: bool,
}

impl UserMemory {
    /// Walk the candidate list in order; return the first one that exists and
    /// is readable. Missing files or read errors are logged and skipped.
    /// `app_data_dir` is the platform-specific per-app data dir (Tauri
    /// provides this on Windows); pass `None` to skip that lookup.
    pub fn load(app_data_dir: Option<&Path>) -> Option<Self> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(explicit) = std::env::var("FORGE_USER_MEMORY") {
            if !explicit.is_empty() { candidates.push(PathBuf::from(explicit)); }
        }
        if let Some(app) = app_data_dir {
            candidates.push(app.join("user.md"));
        }
        if let Some(home) = home_dir() {
            candidates.push(home.join(".forge").join("user.md"));
        }

        for path in candidates {
            if !path.exists() { continue; }
            match std::fs::read_to_string(&path) {
                Ok(mut body) => {
                    let truncated = body.len() > MAX_BYTES;
                    if truncated {
                        let mut cut = MAX_BYTES;
                        while cut > 0 && !body.is_char_boundary(cut) { cut -= 1; }
                        body.truncate(cut);
                        body.push_str("\n… (truncated) …\n");
                    }
                    tracing::debug!(source = %path.display(), bytes = body.len(), truncated, "loaded user memory");
                    return Some(UserMemory { source: path, content: body, truncated });
                }
                Err(e) => tracing::warn!(path = %path.display(), err = %e, "user memory unreadable; trying next candidate"),
            }
        }
        None
    }

    /// Format the memory as a prompt-friendly block.
    pub fn to_prompt_section(&self) -> String {
        format!(
            "## User preferences (from {})\n\n{}\n",
            self.source.display(),
            self.content.trim(),
        )
    }
}

fn home_dir() -> Option<PathBuf> {
    // Cross-platform: HOME on Unix, USERPROFILE on Windows.
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // These tests mutate the process-global `FORGE_USER_MEMORY` env var, which
    // races under cargo's parallel test runner. Serialize them with a shared
    // lock; recover from poisoning so one panicking test doesn't cascade.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!("forge-user-mem-{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    #[test]
    fn returns_none_when_nothing_present() {
        let _g = env_guard();
        let dir = tmp();
        std::env::remove_var("FORGE_USER_MEMORY");
        assert!(UserMemory::load(Some(&dir)).is_none());
    }

    #[test]
    fn reads_from_app_data_dir() {
        let _g = env_guard();
        let dir = tmp();
        write(&dir, "user.md", "prefer rust over go");
        std::env::remove_var("FORGE_USER_MEMORY");
        let m = UserMemory::load(Some(&dir)).unwrap();
        assert!(m.content.contains("prefer rust"));
        assert_eq!(m.source, dir.join("user.md"));
    }

    #[test]
    fn env_override_wins() {
        let _g = env_guard();
        let dir = tmp();
        write(&dir, "user.md", "app data body");
        let ex = write(&dir, "explicit.md", "explicit body");
        std::env::set_var("FORGE_USER_MEMORY", ex.display().to_string());
        let m = UserMemory::load(Some(&dir)).unwrap();
        assert!(m.content.contains("explicit body"));
        std::env::remove_var("FORGE_USER_MEMORY");
    }

    #[test]
    fn truncates_oversized_memory() {
        let _g = env_guard();
        let dir = tmp();
        std::env::remove_var("FORGE_USER_MEMORY");
        write(&dir, "user.md", &"y".repeat(20 * 1024));
        let m = UserMemory::load(Some(&dir)).unwrap();
        assert!(m.truncated);
        assert!(m.content.len() < 20 * 1024);
    }

    #[test]
    fn formats_prompt_section() {
        let m = UserMemory { source: PathBuf::from("/tmp/user.md"), content: "body".into(), truncated: false };
        let s = m.to_prompt_section();
        assert!(s.contains("User preferences"));
        assert!(s.contains("body"));
    }
}
