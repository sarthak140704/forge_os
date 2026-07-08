//! Runtime feature flags.
//!
//! Small typed struct read once at boot from `<db_dir>/feature-flags.toml`.
//! Missing file, unreadable file, or unknown fields → default flags (all
//! experimental features off).
//!
//! Env overrides: `FORGE_FLAG_<UPPER_SNAKE>=1|0` wins over the file. This
//! lets local dev flip a flag without editing the file.
//!
//! To add a flag: add a field with `#[serde(default)]`, extend
//! `apply_env_overrides`, and consult it from the relevant crate through
//! `runtime.flags`. Missing flag → default value; new binaries reading old
//! files never fail.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct FeatureFlags {
    /// Enable the just-in-time task input materializer. On by default when
    /// LLM providers exist; can be forced off for debugging plan-time args.
    pub materializer: MaterializerFlag,
    /// Enable episodic memory recall (retrieve summaries of past missions
    /// with similar text and inject into the planner prompt).
    pub episodic_recall: EpisodicRecallFlag,
    /// Emit MissionCostSummary events after mission terminal transition.
    pub cost_summary: CostSummaryFlag,
}

impl FeatureFlags {
    /// Load flags from the given file, honoring env overrides. Missing file
    /// → defaults + env.
    pub fn load(path: &Path) -> Self {
        let mut flags: FeatureFlags = if path.exists() {
            match std::fs::read_to_string(path) {
                Ok(body) => match toml::from_str(&body) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!(err = %e, path = %path.display(), "feature-flags.toml parse failed; using defaults");
                        FeatureFlags::default()
                    }
                },
                Err(e) => {
                    tracing::warn!(err = %e, path = %path.display(), "feature-flags.toml unreadable; using defaults");
                    FeatureFlags::default()
                }
            }
        } else {
            FeatureFlags::default()
        };
        flags.apply_env_overrides();
        flags
    }

    fn apply_env_overrides(&mut self) {
        if let Some(v) = env_bool("FORGE_FLAG_MATERIALIZER")           { self.materializer.enabled = v; }
        if let Some(v) = env_bool("FORGE_FLAG_EPISODIC_RECALL")        { self.episodic_recall.enabled = v; }
        if let Some(v) = env_bool("FORGE_FLAG_COST_SUMMARY")           { self.cost_summary.enabled = v; }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct MaterializerFlag { pub enabled: bool }
impl Default for MaterializerFlag { fn default() -> Self { Self { enabled: true } } }

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct EpisodicRecallFlag { pub enabled: bool, pub max_recall: usize }
impl Default for EpisodicRecallFlag { fn default() -> Self { Self { enabled: true, max_recall: 3 } } }

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct CostSummaryFlag { pub enabled: bool }
impl Default for CostSummaryFlag { fn default() -> Self { Self { enabled: true } } }

fn env_bool(name: &str) -> Option<bool> {
    std::env::var(name).ok().and_then(|s| match s.trim().to_ascii_lowercase().as_str() {
        "1" | "true"  | "yes" | "on"  => Some(true),
        "0" | "false" | "no"  | "off" => Some(false),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_have_all_features_on() {
        let f = FeatureFlags::default();
        assert!(f.materializer.enabled);
        assert!(f.episodic_recall.enabled);
        assert!(f.cost_summary.enabled);
        assert_eq!(f.episodic_recall.max_recall, 3);
    }

    #[test]
    fn missing_file_returns_defaults() {
        let f = FeatureFlags::load(Path::new("nonexistent-path-1234.toml"));
        assert!(f.materializer.enabled);
    }

    #[test]
    fn parses_toml_and_overrides_defaults() {
        let dir = std::env::temp_dir().join(format!("forge-flags-{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("feature-flags.toml");
        std::fs::write(&path,
            "[materializer]\nenabled = false\n[episodic_recall]\nmax_recall = 5\n"
        ).unwrap();
        let f = FeatureFlags::load(&path);
        assert!(!f.materializer.enabled);
        assert_eq!(f.episodic_recall.max_recall, 5);
        assert!(f.cost_summary.enabled);
    }

    #[test]
    fn env_override_wins() {
        std::env::set_var("FORGE_FLAG_COST_SUMMARY", "false");
        let f = FeatureFlags::load(Path::new("nonexistent-path.toml"));
        assert!(!f.cost_summary.enabled);
        std::env::remove_var("FORGE_FLAG_COST_SUMMARY");
    }

    #[test]
    fn env_bool_parses_common_forms() {
        for s in ["1","true","yes","on","TRUE","On","Yes"] {
            std::env::set_var("FORGE_TEST_FLAG", s);
            assert_eq!(env_bool("FORGE_TEST_FLAG"), Some(true), "case: {s}");
        }
        for s in ["0","false","no","off","False","OFF","No"] {
            std::env::set_var("FORGE_TEST_FLAG", s);
            assert_eq!(env_bool("FORGE_TEST_FLAG"), Some(false), "case: {s}");
        }
        std::env::set_var("FORGE_TEST_FLAG", "maybe");
        assert_eq!(env_bool("FORGE_TEST_FLAG"), None);
        std::env::remove_var("FORGE_TEST_FLAG");
    }
}
