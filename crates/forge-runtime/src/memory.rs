//! Project memory — the per-repository "you are here" briefing that gets
//! injected into every planner call for this workspace.
//!
//! Precedence (first hit wins):
//! 1. `.forge.md`        — Forge-specific override
//! 2. `AGENTS.md`        — community convention (Aider, Continue, Cline, etc.)
//! 3. `CONTRIBUTING.md`  — last-ditch fallback; often has commands + conventions
//!
//! Truncated at 8 KB to keep planning prompts within a reasonable window.

use std::path::{Path, PathBuf};

const MAX_BYTES: usize = 8 * 1024;

const CANDIDATES: &[&str] = &[".forge.md", "AGENTS.md", "CONTRIBUTING.md"];

/// Result of `ProjectMemory::load`. `source` is the filename that was found
/// (relative to the workspace root); `content` is the body, truncated to
/// `MAX_BYTES` if necessary.
#[derive(Clone, Debug)]
pub struct ProjectMemory {
    pub source: String,
    pub content: String,
    pub truncated: bool,
}

impl ProjectMemory {
    /// Walk the candidate list in order; return the first one that exists and
    /// is readable. Missing files or read errors are logged and skipped.
    pub fn load(workspace_root: &Path) -> Option<Self> {
        for name in CANDIDATES {
            let path: PathBuf = workspace_root.join(name);
            if !path.exists() { continue; }
            match std::fs::read_to_string(&path) {
                Ok(mut body) => {
                    let truncated = body.len() > MAX_BYTES;
                    if truncated {
                        // Truncate on a char boundary.
                        let mut cut = MAX_BYTES;
                        while cut > 0 && !body.is_char_boundary(cut) { cut -= 1; }
                        body.truncate(cut);
                        body.push_str("\n… (truncated) …\n");
                    }
                    tracing::debug!(source = %name, bytes = body.len(), truncated, "loaded project memory");
                    return Some(ProjectMemory {
                        source: (*name).to_string(),
                        content: body,
                        truncated,
                    });
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), err = %e, "project memory unreadable; trying next candidate");
                }
            }
        }
        None
    }

    /// Format the memory as a prompt-friendly block.
    pub fn to_prompt_section(&self) -> String {
        format!(
            "## Project context (from {})\n\n{}\n",
            self.source,
            self.content.trim(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!("forge-memory-{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn write(dir: &Path, name: &str, body: &str) {
        let mut f = std::fs::File::create(dir.join(name)).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn returns_none_when_nothing_present() {
        let dir = tmp();
        assert!(ProjectMemory::load(&dir).is_none());
    }

    #[test]
    fn dot_forge_beats_agents_beats_contributing() {
        let dir = tmp();
        write(&dir, "CONTRIBUTING.md", "contrib body");
        assert_eq!(ProjectMemory::load(&dir).unwrap().source, "CONTRIBUTING.md");

        write(&dir, "AGENTS.md", "agents body");
        assert_eq!(ProjectMemory::load(&dir).unwrap().source, "AGENTS.md");

        write(&dir, ".forge.md", "forge body");
        let m = ProjectMemory::load(&dir).unwrap();
        assert_eq!(m.source, ".forge.md");
        assert_eq!(m.content.trim(), "forge body");
    }

    #[test]
    fn truncates_oversized_memory() {
        let dir = tmp();
        write(&dir, "AGENTS.md", &"x".repeat(20 * 1024));
        let m = ProjectMemory::load(&dir).unwrap();
        assert!(m.truncated);
        assert!(m.content.len() < 20 * 1024);
        assert!(m.content.ends_with("(truncated) …\n"));
    }

    #[test]
    fn formats_prompt_section() {
        let m = ProjectMemory { source: ".forge.md".into(), content: "body".into(), truncated: false };
        let s = m.to_prompt_section();
        assert!(s.contains(".forge.md"));
        assert!(s.contains("body"));
    }
}
