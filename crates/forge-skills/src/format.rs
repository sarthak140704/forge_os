//! `SKILL.md` format: YAML front-matter fenced with `---`, then Markdown body.
//!
//! Front-matter fields (required in **bold**, optional otherwise):
//!
//! ```yaml
//! name:        rust-crate           # bold, kebab-case, unique
//! version:     1.0.0                # bold, semver
//! description: Format/lint/test Rust crates      # bold
//! status:      active               # optional: active | pending_review | archived
//! tools:       [fs.read, shell.run] # optional but recommended
//! triggers:                         # optional; used by the registry to rank
//!   keywords:  [rust, cargo, crate]
//!   file_globs: ["**/Cargo.toml"]
//! inputs:                           # optional
//!   - name: crate_path
//!     type: string
//!     description: Path to the crate root
//! outputs: [ ... ]                  # optional
//! ```
//!
//! Everything after the closing `---` is treated as the Markdown body — the
//! prose "how to do this" that the planner will inject into its system prompt.

use crate::SkillError;
use serde::{Deserialize, Serialize};

/// Fully parsed skill.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Skill {
    pub front: SkillFrontMatter,
    /// Markdown body — the human-readable procedure that gets injected into
    /// the planner's system prompt.
    pub body: String,
    /// Absolute path on disk. Empty when the skill was parsed from an in-memory
    /// string (tests + reflection proposals).
    #[serde(default)]
    pub source_path: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkillFrontMatter {
    pub name: String,
    pub version: String,
    pub description: String,

    #[serde(default = "default_status")]
    pub status: SkillStatus,

    #[serde(default)]
    pub tools: Vec<String>,

    #[serde(default)]
    pub triggers: SkillTrigger,

    #[serde(default)]
    pub inputs: Vec<SkillIo>,

    #[serde(default)]
    pub outputs: Vec<SkillIo>,
}

fn default_status() -> SkillStatus { SkillStatus::Active }

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillStatus {
    Active,
    PendingReview,
    Archived,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SkillTrigger {
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub file_globs: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkillIo {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub description: String,
}

/// Parse a `SKILL.md` string.
///
/// The document *must* begin with a `---` line, then YAML, then a closing
/// `---` line. Anything before the opening fence is rejected — we don't want
/// to silently accept skills without metadata.
pub fn parse_skill(source: &str) -> Result<Skill, SkillError> {
    // Normalize line endings so tests + real files behave identically.
    let normalized = source.replace("\r\n", "\n");
    let src = normalized.trim_start_matches('\u{feff}'); // strip BOM if present

    let after_open = src
        .strip_prefix("---\n")
        .ok_or(SkillError::MissingFrontMatter)?;

    // Find the closing fence: a line that is exactly `---`.
    let (yaml, body) = split_at_fence(after_open).ok_or_else(|| SkillError::Malformed {
        name: "<unknown>".into(),
        reason: "no closing `---` fence for front-matter".into(),
    })?;

    let front: SkillFrontMatter = serde_yaml::from_str(yaml)?;

    if front.name.trim().is_empty() { return Err(SkillError::MissingField("name")); }
    if front.version.trim().is_empty() { return Err(SkillError::MissingField("version")); }
    if front.description.trim().is_empty() { return Err(SkillError::MissingField("description")); }

    Ok(Skill {
        front,
        body: body.trim().to_string(),
        source_path: String::new(),
    })
}

fn split_at_fence(after_open: &str) -> Option<(&str, &str)> {
    // Look for a line that is exactly "---" (possibly followed by \n or EOF).
    let mut cursor = 0usize;
    for line in after_open.split_inclusive('\n') {
        let trimmed = line.trim_end_matches('\n');
        if trimmed == "---" {
            let yaml_end = cursor;
            let body_start = cursor + line.len();
            return Some((&after_open[..yaml_end], &after_open[body_start..]));
        }
        cursor += line.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---\n\
        name: rust-crate\n\
        version: 1.0.0\n\
        description: Format, lint, and test a Rust crate\n\
        tools: [fs.read, shell.run]\n\
        triggers:\n  \
          keywords: [rust, cargo]\n\
        ---\n\
        # Rust Crate\n\
        \n\
        1. Run `cargo fmt --check`.\n\
        2. Run `cargo clippy -- -D warnings`.\n";

    #[test]
    fn parses_minimal_skill() {
        let s = parse_skill(SAMPLE).unwrap();
        assert_eq!(s.front.name, "rust-crate");
        assert_eq!(s.front.version, "1.0.0");
        assert_eq!(s.front.status, SkillStatus::Active);
        assert_eq!(s.front.tools, vec!["fs.read", "shell.run"]);
        assert_eq!(s.front.triggers.keywords, vec!["rust", "cargo"]);
        assert!(s.body.starts_with("# Rust Crate"));
        assert!(s.body.contains("cargo fmt --check"));
    }

    #[test]
    fn rejects_missing_front_matter() {
        let err = parse_skill("# Just markdown, no metadata\n").unwrap_err();
        assert!(matches!(err, SkillError::MissingFrontMatter));
    }

    #[test]
    fn rejects_unterminated_front_matter() {
        let err = parse_skill("---\nname: foo\nversion: 1.0.0\ndescription: d\n").unwrap_err();
        assert!(matches!(err, SkillError::Malformed { .. }));
    }

    #[test]
    fn rejects_missing_name() {
        let src = "---\nname: ''\nversion: 1.0.0\ndescription: d\n---\nbody";
        let err = parse_skill(src).unwrap_err();
        assert!(matches!(err, SkillError::MissingField("name")));
    }

    #[test]
    fn handles_crlf_and_bom() {
        let bom_crlf = "\u{feff}---\r\nname: x\r\nversion: 0.1.0\r\ndescription: d\r\n---\r\nbody\r\n";
        let s = parse_skill(bom_crlf).unwrap();
        assert_eq!(s.front.name, "x");
        assert_eq!(s.body, "body");
    }

    #[test]
    fn parses_status_pending_review() {
        let src = "---\nname: n\nversion: 0.1.0\ndescription: d\nstatus: pending_review\n---\nb";
        let s = parse_skill(src).unwrap();
        assert_eq!(s.front.status, SkillStatus::PendingReview);
    }
}
