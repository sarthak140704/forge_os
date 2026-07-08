//! Policy engine.
//!
//! Rules are declared in YAML and evaluated in order. First matching rule
//! determines the decision. If no rule matches, the default is `Allow` for
//! Phase 1 — this is deliberately permissive during development; production
//! deployments should append a catch-all `deny` rule.
//!
//! Rule schema (see `config/policy.default.yaml`):
//!
//! ```yaml
//! rules:
//!   - name: deny_shell_outside_workspace
//!     when_tool: shell.run
//!     require_input_string_prefix:
//!       field: cwd
//!       prefix_of: workspace_root
//!     on_fail: deny
//!
//!   - name: approve_destructive_shell
//!     when_tool: shell.run
//!     require_input_string_not_matches:
//!       field: command
//!       regex: "^(rm|del|format|shutdown|rmdir)\\b"
//!     on_fail: require_approval
//! ```
//!
//! Phase-2 will add a real CEL-like expression language. Phase-1 keeps the
//! DSL declarative-but-limited so the engine is a few hundred lines of Rust,
//! easy to audit, and cannot execute arbitrary code.

use forge_domain::PolicyDecision;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PolicyError {
    #[error("failed to load policy file: {0}")]
    Load(#[from] std::io::Error),
    #[error("failed to parse policy YAML: {0}")]
    Parse(#[from] serde_yaml::Error),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OnFail {
    Deny,
    RequireApproval,
    Allow, // no-op; useful for documenting explicit allowances
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct StringPrefixCheck {
    pub field: String,
    /// Special values: "workspace_root" resolves at eval time.
    pub prefix_of: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct StringMatchesCheck {
    pub field: String,
    pub regex: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Rule {
    pub name: String,
    #[serde(default)]
    pub when_tool: Option<String>,
    #[serde(default)]
    pub require_input_string_prefix: Option<StringPrefixCheck>,
    /// Denies (or requests approval) if the field DOES match the regex.
    /// Useful for "block destructive commands" patterns.
    #[serde(default)]
    pub deny_if_input_string_matches: Option<StringMatchesCheck>,
    pub on_fail: OnFail,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct PolicyFile {
    #[serde(default)]
    pub rules: Vec<Rule>,
}

/// The runtime context a rule can see. Kept intentionally small.
pub struct EvalCtx<'a> {
    pub tool: &'a str,
    pub input: &'a serde_json::Value,
    pub workspace_root: &'a PathBuf,
}

pub struct PolicyEngine {
    rules: Vec<Rule>,
}

impl PolicyEngine {
    pub fn empty() -> Self { Self { rules: Vec::new() } }

    pub fn from_yaml(yaml: &str) -> Result<Self, PolicyError> {
        let file: PolicyFile = serde_yaml::from_str(yaml)?;
        Ok(Self { rules: file.rules })
    }

    pub fn from_file(path: &std::path::Path) -> Result<Self, PolicyError> {
        let yaml = std::fs::read_to_string(path)?;
        Self::from_yaml(&yaml)
    }

    pub fn evaluate(&self, ctx: &EvalCtx<'_>) -> PolicyDecision {
        for rule in &self.rules {
            if let Some(t) = &rule.when_tool {
                if t != ctx.tool { continue; }
            }
            if let Some(check) = &rule.require_input_string_prefix {
                let value = ctx.input.get(&check.field).and_then(|v| v.as_str()).unwrap_or("");
                let prefix = resolve_special(&check.prefix_of, ctx);
                if !value.starts_with(&prefix) {
                    return decision(rule, format!(
                        "field `{}` (`{}`) must start with `{}`", check.field, value, prefix
                    ));
                }
            }
            if let Some(check) = &rule.deny_if_input_string_matches {
                let value = ctx.input.get(&check.field).and_then(|v| v.as_str()).unwrap_or("");
                let re = match regex_lite_compile(&check.regex) {
                    Ok(re) => re,
                    Err(_) => continue,
                };
                if re.is_match(value) {
                    return decision(rule, format!(
                        "field `{}` (`{}`) matches denied pattern `{}`", check.field, value, check.regex
                    ));
                }
            }
        }
        PolicyDecision::Allow
    }
}

fn decision(rule: &Rule, reason: String) -> PolicyDecision {
    match rule.on_fail {
        OnFail::Deny => PolicyDecision::Deny { rule: rule.name.clone(), reason },
        OnFail::RequireApproval => PolicyDecision::RequireApproval { rule: rule.name.clone(), reason },
        OnFail::Allow => PolicyDecision::Allow,
    }
}

fn resolve_special(v: &str, ctx: &EvalCtx<'_>) -> String {
    match v {
        "workspace_root" => ctx.workspace_root.display().to_string(),
        other => other.to_string(),
    }
}

// A tiny regex fallback so we don't pull in the full `regex` crate for a
// handful of alternation-style patterns. Supports `^`, `\\b`, `|`, `(a|b)`,
// and literal characters. Anything more complex → returns Err and the rule
// is skipped (fail-open, logged in Phase 2).
mod regex_lite {
    pub struct Re { anchored: bool, alts: Vec<String> }
    impl Re {
        pub fn is_match(&self, hay: &str) -> bool {
            if self.anchored {
                self.alts.iter().any(|a| hay.starts_with(a.as_str()))
            } else {
                self.alts.iter().any(|a| hay.contains(a.as_str()))
            }
        }
    }
    pub fn compile(pattern: &str) -> Result<Re, &'static str> {
        // Strip a leading ^
        let (anchored, rest) = if let Some(stripped) = pattern.strip_prefix('^') { (true, stripped) } else { (false, pattern) };
        // Strip trailing \b (we treat it as "end of alternative" — good enough).
        let rest = rest.trim_end_matches("\\b");
        // Strip optional grouping parens around a top-level alternation.
        let rest = rest.trim_start_matches('(').trim_end_matches(')');
        // Reject anything with additional meta chars we don't handle.
        if rest.contains('[') || rest.contains(']') || rest.contains('*') || rest.contains('+')
            || rest.contains('?') || rest.contains('.') || rest.contains('{')
            || rest.contains('\\') {
            return Err("unsupported metachar");
        }
        let alts: Vec<String> = rest.split('|').map(|s| s.to_string()).collect();
        Ok(Re { anchored, alts })
    }
}
use regex_lite::Re as LiteRe;
fn regex_lite_compile(p: &str) -> Result<LiteRe, &'static str> { regex_lite::compile(p) }

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn workspace_scoped_shell_ok_and_denied() {
        let yaml = r#"
rules:
  - name: deny_shell_outside_workspace
    when_tool: shell.run
    require_input_string_prefix:
      field: cwd
      prefix_of: workspace_root
    on_fail: deny
"#;
        let engine = PolicyEngine::from_yaml(yaml).unwrap();
        let wsroot = PathBuf::from("C:/ws");
        let ok = engine.evaluate(&EvalCtx {
            tool: "shell.run",
            input: &json!({ "command": "echo hi", "cwd": "C:/ws/sub" }),
            workspace_root: &wsroot,
        });
        assert!(matches!(ok, PolicyDecision::Allow));
        let bad = engine.evaluate(&EvalCtx {
            tool: "shell.run",
            input: &json!({ "command": "echo hi", "cwd": "C:/other" }),
            workspace_root: &wsroot,
        });
        assert!(matches!(bad, PolicyDecision::Deny { .. }));
    }

    #[test]
    fn destructive_shell_requires_approval() {
        let yaml = r#"
rules:
  - name: approve_destructive_shell
    when_tool: shell.run
    deny_if_input_string_matches:
      field: command
      regex: "^(rm|del|format|shutdown)\\b"
    on_fail: require_approval
"#;
        let engine = PolicyEngine::from_yaml(yaml).unwrap();
        let wsroot = PathBuf::from("C:/ws");
        let d = engine.evaluate(&EvalCtx {
            tool: "shell.run", input: &json!({ "command": "rm -rf /" }), workspace_root: &wsroot,
        });
        assert!(matches!(d, PolicyDecision::RequireApproval { .. }));
        let a = engine.evaluate(&EvalCtx {
            tool: "shell.run", input: &json!({ "command": "ls" }), workspace_root: &wsroot,
        });
        assert!(matches!(a, PolicyDecision::Allow));
    }
}
