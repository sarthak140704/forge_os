//! Phase 4b — Skill validation gate.
//!
//! Before a proposed skill can be promoted into `active/`, it runs through a
//! **pure-Rust** validator (no LLM, no network). This encodes agent.txt's
//! "*Learning should require validation before promotion*" rule as an
//! auditable, deterministic check.
//!
//! The validator returns a `ValidationReport` — a list of per-check outcomes
//! plus an overall `ok` boolean. A single hard-fail check flips `ok = false`.
//! Soft warnings do NOT block promotion but are surfaced through IPC so the
//! human reviewer sees them.
//!
//! # Checks
//!
//! | id                        | severity | rule                                                                                          |
//! |---------------------------|----------|-----------------------------------------------------------------------------------------------|
//! | `parses`                  | hard     | Front-matter parses as `SkillFrontMatter`; body is present.                                   |
//! | `body_length`             | hard     | Body has ≥ 40 non-whitespace characters (rules out empty-shell proposals).                    |
//! | `has_trigger`             | hard     | At least one keyword OR one file-glob in `triggers` (otherwise the registry can't select it). |
//! | `tools_declared`          | hard     | `tools` is non-empty (a tools-less skill has nothing to compose).                             |
//! | `tools_resolvable`        | hard     | Every declared tool matches either a local ToolRegistry entry OR an MCP prefix `mcp:*`.       |
//! | `no_name_collision`       | soft     | Warns if an ACTIVE skill of a different name matches ≥ 3 keywords → likely dedupe candidate.  |
//! | `version_monotonic`       | soft     | Warns if a prior active version exists and this proposal's version < that version.            |
//! | `keywords_normalised`     | soft     | Warns if any keyword contains uppercase (the registry matches case-insensitively — noise).    |
//!
//! # Design notes
//!
//! - Callers pass a `ValidatorContext` with the set of known tool names +
//!   the currently-active skills. Neither is mutated. This keeps the
//!   validator side-effect free and trivial to test.
//! - Tool resolution accepts an `mcp:` prefix so proposals can reference MCP
//!   servers the runtime might load in a different session. We can't enforce
//!   that the server actually exists without a live registry, and forcing
//!   that check would make offline validation impossible.

use crate::{parse_skill, Skill, SkillError};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Hard,
    Soft,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidationCheck {
    pub id:       String,
    pub severity: Severity,
    pub passed:   bool,
    /// Empty string if `passed`. On failure, a human-readable explanation.
    pub message:  String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidationReport {
    /// True iff every HARD check passed. Soft failures do not affect `ok`.
    pub ok:     bool,
    pub checks: Vec<ValidationCheck>,
}

impl ValidationReport {
    /// Convenience: names of every failed check, hard OR soft.
    pub fn failed(&self) -> Vec<String> {
        self.checks.iter().filter(|c| !c.passed).map(|c| c.id.clone()).collect()
    }

    /// Only the HARD failures. Empty iff `ok == true`.
    pub fn hard_failures(&self) -> Vec<String> {
        self.checks.iter().filter(|c| !c.passed && c.severity == Severity::Hard).map(|c| c.id.clone()).collect()
    }
}

/// Minimal snapshot of an already-active skill that the validator needs.
/// Deliberately does NOT depend on `SkillHistoryRepository` so this crate
/// stays free of `forge-persistence`.
#[derive(Clone, Debug, Default)]
pub struct ActiveSkillSummary {
    pub name:     String,
    pub version:  String,
    pub keywords: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ValidatorContext {
    /// Case-sensitive tool ids known to the local runtime, e.g. `fs.read`,
    /// `shell.run`. MCP-provided tools do not need to be listed — the
    /// validator accepts them via the `mcp:` prefix convention.
    pub known_tools:    Vec<String>,
    /// Currently-active skills. Used only for collision + monotonic checks.
    pub active_skills:  Vec<ActiveSkillSummary>,
}

/// Parse the raw SKILL.md bytes and run every check. Never panics; a bad
/// front-matter parse degrades to a single hard `parses` failure with the
/// error message.
pub fn validate_bytes(raw: &str, ctx: &ValidatorContext) -> ValidationReport {
    match parse_skill(raw) {
        Ok(skill) => validate_skill(&skill, ctx),
        Err(e) => ValidationReport {
            ok: false,
            checks: vec![ValidationCheck {
                id:       "parses".into(),
                severity: Severity::Hard,
                passed:   false,
                message:  format!("front-matter parse error: {e}"),
            }],
        },
    }
}

/// Same as `validate_bytes` but on an already-parsed skill. Useful when the
/// caller already parsed once and wants to avoid double-work.
pub fn validate_skill(s: &Skill, ctx: &ValidatorContext) -> ValidationReport {
    let mut checks = Vec::with_capacity(8);

    // parses — trivially true here, since we got a Skill in.
    checks.push(pass("parses", Severity::Hard));

    // body_length
    let body_len = s.body.split_whitespace().map(str::len).sum::<usize>();
    if body_len >= 40 {
        checks.push(pass("body_length", Severity::Hard));
    } else {
        checks.push(fail("body_length", Severity::Hard,
            format!("body has only {body_len} non-whitespace chars; expected ≥ 40")));
    }

    // has_trigger
    let has_trigger = !s.front.triggers.keywords.is_empty() || !s.front.triggers.file_globs.is_empty();
    if has_trigger {
        checks.push(pass("has_trigger", Severity::Hard));
    } else {
        checks.push(fail("has_trigger", Severity::Hard,
            "must declare at least one keyword or file_glob".into()));
    }

    // tools_declared
    if !s.front.tools.is_empty() {
        checks.push(pass("tools_declared", Severity::Hard));
    } else {
        checks.push(fail("tools_declared", Severity::Hard,
            "must declare at least one tool".into()));
    }

    // tools_resolvable
    let known: std::collections::HashSet<&str> = ctx.known_tools.iter().map(|s| s.as_str()).collect();
    let unresolved: Vec<&str> = s.front.tools.iter()
        .filter(|t| !known.contains(t.as_str()) && !t.starts_with("mcp:"))
        .map(|s| s.as_str())
        .collect();
    if unresolved.is_empty() {
        checks.push(pass("tools_resolvable", Severity::Hard));
    } else {
        checks.push(fail("tools_resolvable", Severity::Hard,
            format!("unknown tool(s): {} — declare them in the tool registry or prefix with `mcp:`", unresolved.join(", "))));
    }

    // no_name_collision (soft)
    let this_kws: std::collections::HashSet<&str> = s.front.triggers.keywords.iter().map(|k| k.as_str()).collect();
    let mut collisions: Vec<(&str, usize)> = Vec::new();
    for other in &ctx.active_skills {
        if other.name == s.front.name { continue; }
        let overlap = other.keywords.iter().filter(|k| this_kws.contains(k.as_str())).count();
        if overlap >= 3 {
            collisions.push((other.name.as_str(), overlap));
        }
    }
    if collisions.is_empty() {
        checks.push(pass("no_name_collision", Severity::Soft));
    } else {
        let detail = collisions.iter().map(|(n,c)| format!("`{n}` ({c} kw)")).collect::<Vec<_>>().join(", ");
        checks.push(fail("no_name_collision", Severity::Soft,
            format!("shares ≥3 keywords with: {detail}")));
    }

    // version_monotonic (soft)
    if let Some(prev) = ctx.active_skills.iter().find(|a| a.name == s.front.name) {
        if version_less(&s.front.version, &prev.version) {
            checks.push(fail("version_monotonic", Severity::Soft,
                format!("proposed version {} < currently-active {}", s.front.version, prev.version)));
        } else {
            checks.push(pass("version_monotonic", Severity::Soft));
        }
    } else {
        checks.push(pass("version_monotonic", Severity::Soft));
    }

    // keywords_normalised (soft)
    let bad: Vec<&str> = s.front.triggers.keywords.iter()
        .filter(|k| k.chars().any(|c| c.is_ascii_uppercase()))
        .map(|s| s.as_str())
        .collect();
    if bad.is_empty() {
        checks.push(pass("keywords_normalised", Severity::Soft));
    } else {
        checks.push(fail("keywords_normalised", Severity::Soft,
            format!("keyword(s) contain uppercase — matcher is case-insensitive: {}", bad.join(", "))));
    }

    let ok = checks.iter().all(|c| c.passed || c.severity == Severity::Soft);
    ValidationReport { ok, checks }
}

/// Validate the file at `path` (reads it once, then delegates to `validate_bytes`).
/// Returns a hard-fail report on IO error rather than propagating — this lets
/// the UI show WHY a proposal failed rather than a stack trace.
pub fn validate_file(path: &std::path::Path, ctx: &ValidatorContext) -> Result<ValidationReport, SkillError> {
    let raw = std::fs::read_to_string(path)?;
    Ok(validate_bytes(&raw, ctx))
}

// ---------- helpers ------------------------------------------------------

fn pass(id: &str, sev: Severity) -> ValidationCheck {
    ValidationCheck { id: id.into(), severity: sev, passed: true, message: String::new() }
}
fn fail(id: &str, sev: Severity, msg: String) -> ValidationCheck {
    ValidationCheck { id: id.into(), severity: sev, passed: false, message: msg }
}

/// Cheap 3-component semver compare (major.minor.patch). Non-numeric tail is
/// ignored. Missing components default to 0. Returns true iff `a < b`.
fn version_less(a: &str, b: &str) -> bool {
    let pa = parse3(a);
    let pb = parse3(b);
    pa < pb
}

fn parse3(v: &str) -> (u64, u64, u64) {
    let mut it = v.split(|c: char| c == '.' || c == '-' || c == '+');
    let a = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let b = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let c = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (a, b, c)
}

// ---------- tests --------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ValidatorContext {
        ValidatorContext {
            known_tools: vec!["fs.read".into(), "fs.write".into(), "shell.run".into()],
            active_skills: Vec::new(),
        }
    }

    fn good_body() -> &'static str {
        r#"---
name: demo-skill
version: 0.1.0
description: A demo skill for tests
status: active
tools:
  - fs.read
triggers:
  keywords: [demo, test]
  file_globs: []
inputs: []
outputs: []
---
# demo-skill

This skill exists to exercise the validator. It has more than forty non-whitespace characters in its body to satisfy the body_length check.
"#
    }

    #[test]
    fn good_skill_passes_all_checks() {
        let r = validate_bytes(good_body(), &ctx());
        assert!(r.ok, "expected ok, got failures: {:?}", r.failed());
        assert!(r.checks.iter().all(|c| c.passed), "some check failed: {:?}", r.checks);
    }

    #[test]
    fn empty_body_fails_hard() {
        let body = good_body().replace(
            "This skill exists to exercise the validator. It has more than forty non-whitespace characters in its body to satisfy the body_length check.\n",
            "hi\n",
        );
        let r = validate_bytes(&body, &ctx());
        assert!(!r.ok);
        assert!(r.hard_failures().contains(&"body_length".to_string()));
    }

    #[test]
    fn no_trigger_fails_hard() {
        let body = good_body().replace("keywords: [demo, test]", "keywords: []");
        let r = validate_bytes(&body, &ctx());
        assert!(!r.ok);
        assert!(r.hard_failures().contains(&"has_trigger".to_string()));
    }

    #[test]
    fn no_tools_fails_hard() {
        let body = good_body().replace(
            "tools:\n  - fs.read",
            "tools: []",
        );
        let r = validate_bytes(&body, &ctx());
        assert!(!r.ok);
        assert!(r.hard_failures().contains(&"tools_declared".to_string()));
    }

    #[test]
    fn unknown_tool_fails_hard_but_mcp_prefix_passes() {
        // unknown, no prefix → hard fail
        let body_unknown = good_body().replace("- fs.read", "- unknown.tool");
        let r = validate_bytes(&body_unknown, &ctx());
        assert!(r.hard_failures().contains(&"tools_resolvable".to_string()));

        // mcp: prefix → allowed
        let body_mcp = good_body().replace("- fs.read", "- mcp:whatever");
        let r = validate_bytes(&body_mcp, &ctx());
        assert!(r.ok, "mcp: prefix should be accepted: {:?}", r.failed());
    }

    #[test]
    fn bad_front_matter_returns_single_parses_failure() {
        let r = validate_bytes("not a skill file", &ctx());
        assert!(!r.ok);
        let ids: Vec<&str> = r.checks.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["parses"]);
    }

    #[test]
    fn keyword_collision_is_soft() {
        let mut c = ctx();
        c.active_skills.push(ActiveSkillSummary {
            name: "other-skill".into(),
            version: "0.1.0".into(),
            keywords: vec!["demo".into(), "test".into(), "extra".into()],
        });
        // ...proposal declares keywords: [demo, test] → only 2 overlap → no warning
        let r = validate_bytes(good_body(), &c);
        assert!(r.ok, "2 keyword overlap should NOT trip the ≥3 threshold");

        // Now add a third overlapping keyword to the proposal
        let body3 = good_body().replace("keywords: [demo, test]", "keywords: [demo, test, extra]");
        let r3 = validate_bytes(&body3, &c);
        assert!(r3.ok, "collision is soft, ok stays true");
        assert!(r3.failed().contains(&"no_name_collision".to_string()));
    }

    #[test]
    fn version_regression_is_soft_warning() {
        let mut c = ctx();
        c.active_skills.push(ActiveSkillSummary {
            name: "demo-skill".into(),
            version: "0.5.0".into(),
            keywords: vec![],
        });
        let r = validate_bytes(good_body(), &c); // proposal is 0.1.0
        assert!(r.ok, "version regression is only a soft warning");
        assert!(r.failed().contains(&"version_monotonic".to_string()));
    }

    #[test]
    fn uppercase_keyword_is_soft_warning() {
        let body = good_body().replace("keywords: [demo, test]", "keywords: [Demo, test]");
        let r = validate_bytes(&body, &ctx());
        assert!(r.ok);
        assert!(r.failed().contains(&"keywords_normalised".to_string()));
    }

    #[test]
    fn version_less_orders_correctly() {
        assert!(version_less("0.1.0", "0.2.0"));
        assert!(version_less("0.1.0", "1.0.0"));
        assert!(!version_less("1.0.0", "0.9.9"));
        assert!(!version_less("0.1.0", "0.1.0"));
        assert!(version_less("0.1.0-alpha", "0.2.0"));
    }
}
