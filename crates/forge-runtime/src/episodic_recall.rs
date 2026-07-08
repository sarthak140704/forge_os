//! Episodic memory recall.
//!
//! Before the planner emits a plan, look up terminal prior missions whose
//! title/description overlap the current mission's keywords. Format their
//! outcomes as a short block prepended to project memory so the planner can
//! learn from prior attempts (successful patterns AND failure modes).
//!
//! This is intentionally low-tech: no embeddings, no vector index — just a
//! keyword LIKE match on the missions table. Cheap, deterministic, and good
//! enough to bootstrap the learning loop. A vector-search backend can slot
//! in behind the same interface later.

use forge_domain::Mission;
use forge_persistence::{MissionRepository, ReflectionRepository, ReflectionRecord};
use std::sync::Arc;

/// A single prior-attempt snippet, formatted for injection into the prompt.
#[derive(Clone, Debug)]
pub struct RecalledMission {
    pub title:       String,
    pub outcome:     String,
    pub summary:     String,
}

/// Extract keywords from a title (+ optional description) for matching.
/// Rule: lowercase, split on non-alphanumeric, keep tokens length >= 4,
/// drop stopwords. Deduplicate. Cap at 8 keywords.
pub fn extract_keywords(text: &str) -> Vec<String> {
    const STOPWORDS: &[&str] = &[
        "with","from","that","this","have","been","will","should","when","what",
        "then","them","your","yours","into","also","some","other","which","their",
        "there","these","those","just","like","make","made","using","used","use",
        "about","would","could","might","need","needs","only","over","under","after",
        "before","between","because","would","done","onto","over","again","still",
    ];
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for tok in text.split(|c: char| !c.is_alphanumeric()) {
        let t = tok.to_lowercase();
        if t.len() < 4 { continue; }
        if STOPWORDS.contains(&t.as_str()) { continue; }
        if seen.insert(t.clone()) { out.push(t); }
        if out.len() >= 8 { break; }
    }
    out
}

/// Build the "Prior attempts" block for a mission. Returns `None` if
/// disabled or nothing found. Best-effort — repository errors are logged
/// and treated as "no recall".
pub async fn build_recall_block(
    missions_repo: &Arc<dyn MissionRepository>,
    reflections_repo: &Arc<dyn ReflectionRepository>,
    current: &Mission,
    max_recall: usize,
) -> Option<String> {
    if max_recall == 0 { return None; }
    let text = format!("{} {}", current.title, current.description);
    let keywords = extract_keywords(&text);
    if keywords.is_empty() { return None; }

    let matches = match missions_repo.search_similar(&keywords, max_recall * 2).await {
        Ok(v) => v,
        Err(e) => { tracing::warn!(err = %e, "episodic recall search failed"); return None; }
    };
    // Exclude self (defensive — the current mission is not terminal yet, so
    // it shouldn't match, but a re-run of plan_and_run on a re-opened mission
    // could hit this).
    let mut hits: Vec<Mission> = matches.into_iter().filter(|m| m.id != current.id).collect();
    hits.truncate(max_recall);
    if hits.is_empty() { return None; }

    let mut lines: Vec<String> = Vec::new();
    lines.push("## Prior attempts (episodic recall)".into());
    lines.push(
        "The following past missions overlap the current one. Consider what worked, \
         what failed, and what you'd do differently."
            .into(),
    );
    for m in hits {
        let outcome = format!("{:?}", m.status);
        let reflection = match reflections_repo.list_for_mission(m.id).await {
            Ok(mut rs) => rs.pop(),
            Err(_) => None,
        };
        let summary = reflection
            .and_then(|r: ReflectionRecord| extract_summary_from_reflection(&r.payload))
            .unwrap_or_else(|| m.description.chars().take(240).collect::<String>());
        lines.push(format!("- **{}** — outcome: {}\n  {}", m.title, outcome, summary));
    }
    Some(lines.join("\n"))
}

/// Reflections are stored as JSON; try to pull a short summary out. If
/// parsing fails, return None so the caller falls back to mission description.
fn extract_summary_from_reflection(payload: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(payload).ok()?;
    // Preferred order: what_worked, what_failed, root_causes, or the whole
    // "summary" field if present.
    if let Some(s) = v.get("summary").and_then(|x| x.as_str()) {
        return Some(s.chars().take(240).collect());
    }
    let mut parts: Vec<String> = Vec::new();
    for key in ["what_worked", "what_failed", "root_causes"] {
        if let Some(arr) = v.get(key).and_then(|x| x.as_array()) {
            let joined: String = arr.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join("; ");
            if !joined.is_empty() {
                parts.push(format!("{}: {}", key, joined));
            }
        }
    }
    if parts.is_empty() { None } else { Some(parts.join(" | ").chars().take(320).collect()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_keywords_basic() {
        let kw = extract_keywords("Create a Python CLI that prints hello world");
        assert!(kw.contains(&"python".to_string()));
        assert!(kw.contains(&"prints".to_string()) || kw.contains(&"hello".to_string()) || kw.contains(&"world".to_string()));
        assert!(!kw.contains(&"a".to_string()));
    }

    #[test]
    fn extract_keywords_drops_stopwords_and_shorts() {
        let kw = extract_keywords("The dog is on the roof");
        assert!(!kw.contains(&"the".to_string()));
        assert!(!kw.contains(&"is".to_string()));
        assert!(!kw.contains(&"on".to_string()));
        assert!(kw.contains(&"roof".to_string()));
    }

    #[test]
    fn extract_keywords_dedupes_and_caps() {
        let text = "alpha alpha beta beta beta gamma gamma delta epsilon zeta eta theta iota kappa";
        let kw = extract_keywords(text);
        assert!(kw.len() <= 8);
        assert_eq!(kw.iter().filter(|k| *k == "alpha").count(), 1);
    }

    #[test]
    fn extract_summary_from_reflection_prefers_summary_field() {
        let payload = r#"{"summary":"we shipped it","what_failed":["x"]}"#;
        let s = extract_summary_from_reflection(payload).unwrap();
        assert!(s.starts_with("we shipped"));
    }

    #[test]
    fn extract_summary_from_reflection_falls_back_to_arrays() {
        let payload = r#"{"what_worked":["planning","tools"],"what_failed":["timing"]}"#;
        let s = extract_summary_from_reflection(payload).unwrap();
        assert!(s.contains("planning"));
        assert!(s.contains("timing"));
    }

    #[test]
    fn extract_summary_from_reflection_returns_none_on_invalid_json() {
        assert!(extract_summary_from_reflection("not json").is_none());
    }
}
