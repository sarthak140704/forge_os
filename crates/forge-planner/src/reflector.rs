//! Reflection pass.
//!
//! After a mission terminates (Completed, Failed, or Canceled) we run a
//! single LLM call that reviews the entire event history and produces a
//! structured `MissionReflection`. This is the "learning" seed:
//! - `what_worked` / `what_failed` become part of the episodic memory
//! - `insights` inform future planning heuristics
//! - `suggested_skills` become skill proposals in `skills_root/proposed/`
//!
//! Nothing in the reflector auto-modifies the runtime. It is a *proposer*.
//! Human review is the promotion gate.

use crate::PlannerError;
use forge_llm::{ChatMessage, CompletionRequest, LlmRouter};
use forge_skills::SuggestedSkill;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub struct Reflector {
    llm: Arc<LlmRouter>,
    model: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MissionReflection {
    pub what_worked: Vec<String>,
    pub what_failed: Vec<String>,
    pub insights: Vec<String>,
    #[serde(default)]
    pub suggested_skills: Vec<SuggestedSkill>,
}

impl Reflector {
    pub fn new(llm: Arc<LlmRouter>, model: impl Into<String>) -> Self {
        Self { llm, model: model.into() }
    }

    /// Run one reflection pass. `event_summary` is expected to be a
    /// deterministic newline-separated log of the mission's events (produced
    /// by the caller from `EventStore::list_for_mission`).
    ///
    /// Failures during reflection are non-fatal — they never fail the mission
    /// itself; callers should log and continue.
    pub async fn reflect(
        &self,
        mission_id: Option<forge_domain::MissionId>,
        mission_title: &str,
        mission_description: &str,
        outcome: &str,
        event_summary: &str,
    ) -> Result<MissionReflection, PlannerError> {
        let system = SYSTEM_PROMPT.to_string();
        let user = format!(
            "Mission title: {title}\n\
             Mission description: {desc}\n\
             Outcome: {outcome}\n\
             \n\
             Event log (chronological):\n{log}\n",
            title = mission_title,
            desc = mission_description,
            outcome = outcome,
            log = event_summary,
        );

        let resp = self.llm.complete(CompletionRequest {
            model: self.model.clone(),
            messages: vec![
                ChatMessage { role: "system".into(), content: system },
                ChatMessage { role: "user".into(),   content: user },
            ],
            temperature: Some(0.3),
            max_tokens: Some(1500),
            json_mode: true,
            mission_id: mission_id.map(|m| m.to_string()),
        }).await?;

        let cleaned = strip_fences(&resp.content);
        let reflection: MissionReflection = serde_json::from_str(cleaned)
            .map_err(|e| PlannerError::ParseJson(e.to_string()))?;
        Ok(reflection)
    }
}

const SYSTEM_PROMPT: &str = r##"You are the reflection engine for Forge OS. You review a completed mission
and produce a JSON post-mortem. Be honest about failures — do not sugar-coat.

Return ONLY a JSON object matching this schema (no prose, no markdown fences):

{
  "what_worked":  ["short bullet ...", "short bullet ..."],
  "what_failed":  ["short bullet ...", "short bullet ..."],
  "insights":     ["actionable insight ...", "actionable insight ..."],
  "suggested_skills": [
    {
      "name":        "kebab-case-name",
      "description": "one-sentence summary",
      "tools":       ["fs.read", "shell.run"],
      "keywords":    ["kw1", "kw2"],
      "body":        "# Playbook body in Markdown\n\n1. Step ...\n2. Step ...\n"
    }
  ]
}

Rules:
- Only suggest a new skill if this mission demonstrated a genuinely reusable
  procedure that is not already in the built-in skill set. Prefer zero
  suggested skills over speculative ones.
- Skill `body` must be actionable Markdown a future planner can follow.
- Never suggest a skill that would require tools not present in this mission.
- Keep every bullet under 20 words.
- If the mission was outright cancelled with no work done, return empty arrays.
"##;

fn strip_fences(s: &str) -> &str {
    let s = s.trim();
    let s = s.strip_prefix("```json").or_else(|| s.strip_prefix("```")).unwrap_or(s);
    s.strip_suffix("```").unwrap_or(s).trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_full_reflection() {
        let raw = r##"{
            "what_worked": ["planner picked rust-crate skill"],
            "what_failed": ["shell mkdir -p emitted a literal -p dir on windows"],
            "insights":    ["normalise mkdir -p in the shell tool"],
            "suggested_skills": [{
                "name": "windows-shell-hygiene",
                "description": "Normalise unix-isms in shell commands on windows",
                "tools": ["shell.run"],
                "keywords": ["windows", "shell"],
                "body": "# Windows Shell Hygiene\n\n- Replace `mkdir -p X` with `mkdir X`."
            }]
        }"##;
        let r: MissionReflection = serde_json::from_str(raw).unwrap();
        assert_eq!(r.what_worked.len(), 1);
        assert_eq!(r.suggested_skills.len(), 1);
        assert_eq!(r.suggested_skills[0].name, "windows-shell-hygiene");
    }

    #[test]
    fn deserializes_empty_reflection() {
        let raw = r#"{"what_worked":[],"what_failed":[],"insights":[]}"#;
        let r: MissionReflection = serde_json::from_str(raw).unwrap();
        assert!(r.suggested_skills.is_empty());
    }

    #[test]
    fn strip_fences_handles_markdown() {
        assert_eq!(strip_fences("```json\n{}\n```"), "{}");
        assert_eq!(strip_fences("```\n{}\n```"), "{}");
        assert_eq!(strip_fences("{}"), "{}");
    }
}
