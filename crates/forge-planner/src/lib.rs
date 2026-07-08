//! Mission planner.
//!
//! Given a `Mission { title, description }` and the set of available tool
//! schemas, ask the LLM to emit a JSON plan describing goals and their tasks.
//! The output is validated against a fixed schema; malformed output triggers
//! one retry with the validation error appended to the prompt.
//!
//! Phase-1 emits a *single-shot* plan. Phase-2 will add continuous
//! re-planning based on `TaskCompleted` / `TaskFailed` events.

use forge_domain::{Goal, GoalId, GoalStatus, Mission, MissionId, Task, TaskStatus, ToolSchema};
use forge_llm::{ChatMessage, CompletionRequest, LlmRouter};
use forge_skills::Skill;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

pub mod reflector;
pub use reflector::{MissionReflection, Reflector};

#[derive(Debug, Error)]
pub enum PlannerError {
    #[error("llm error: {0}")]
    Llm(#[from] forge_llm::LlmError),
    #[error("plan output could not be parsed as JSON: {0}")]
    ParseJson(String),
    #[error("plan output failed validation: {0}")]
    Invalid(String),
    #[error("planner produced no goals")]
    Empty,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RawPlan {
    goals: Vec<RawGoal>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
struct RawGoal {
    id: String,
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    tasks: Vec<RawTask>,
    #[serde(default = "default_confidence")]
    confidence: f32,
}
fn default_confidence() -> f32 { 0.6 }

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RawTask {
    tool: String,
    input: serde_json::Value,
}

/// A finished plan: mission-scoped goals + tasks, ready to be persisted and executed.
#[derive(Clone, Debug)]
pub struct Plan {
    pub mission_id: MissionId,
    pub goals: Vec<Goal>,
    pub tasks: Vec<Task>,
}

/// A revision to an existing plan produced by `Planner::replan`.
///
/// `added_goals` / `added_tasks` are new items to persist and execute.
/// `terminate` signals the planner believes the mission is done and the
/// executor should stop looping.
#[derive(Clone, Debug, Default)]
pub struct PlanDelta {
    pub mission_id: MissionId,
    pub added_goals: Vec<Goal>,
    pub added_tasks: Vec<Task>,
    pub terminate: bool,
    pub reason: String,
}

/// Snapshot of a goal + its tasks, used to build the replan prompt.
#[derive(Clone, Debug)]
pub struct GoalSnapshot {
    pub id: GoalId,
    pub title: String,
    pub description: String,
    pub status: GoalStatus,
    pub tasks: Vec<TaskSnapshot>,
    pub depends_on: Vec<GoalId>,
}

#[derive(Clone, Debug)]
pub struct TaskSnapshot {
    pub tool: String,
    pub status: TaskStatus,
    pub result_summary: Option<String>,
    pub error: Option<String>,
}

pub struct Planner {
    llm: std::sync::Arc<LlmRouter>,
    model: String,
}

impl Planner {
    pub fn new(llm: std::sync::Arc<LlmRouter>, model: impl Into<String>) -> Self {
        Self { llm, model: model.into() }
    }

    pub async fn plan(
        &self,
        mission: &Mission,
        tools: &[ToolSchema],
        skills: &[&Skill],
        project_memory: Option<&str>,
    ) -> Result<Plan, PlannerError> {
        let system = build_system_prompt(tools, skills, project_memory);
        let user = build_user_prompt(mission);

        let mut messages = vec![
            ChatMessage { role: "system".to_string(), content: system.clone() },
            ChatMessage { role: "user".to_string(),   content: user.clone() },
        ];

        let mut last_err: Option<String> = None;
        for attempt in 0..2 {
            let resp = self.llm.complete(CompletionRequest {
                model: self.model.clone(),
                messages: messages.clone(),
                temperature: Some(0.2),
                max_tokens: Some(2048),
                json_mode: true,
                mission_id: Some(mission.id.to_string()),
            }).await?;

            match parse_and_validate(&resp.content, tools) {
                Ok(raw) => return Ok(materialize(mission.id, raw)),
                Err(e) => {
                    tracing::warn!(attempt, error = %e, "planner output invalid");
                    last_err = Some(e.to_string());
                    // Feed the error back in for the second attempt.
                    messages.push(ChatMessage {
                        role: "assistant".to_string(),
                        content: resp.content,
                    });
                    messages.push(ChatMessage {
                        role: "user".to_string(),
                        content: format!(
                            "Your previous plan was invalid: {e}. Return ONLY a JSON object matching the schema. Do not add prose."
                        ),
                    });
                }
            }
        }
        Err(PlannerError::Invalid(last_err.unwrap_or_else(|| "unknown".into())))
    }

    /// Continuous re-planning. Called by the executor after each execution
    /// wave completes. The planner sees the current DAG snapshot + the
    /// mission brief and decides whether to (a) add more goals, (b) terminate
    /// the mission successfully, or (c) give up.
    pub async fn replan(
        &self,
        mission: &Mission,
        tools: &[ToolSchema],
        skills: &[&Skill],
        project_memory: Option<&str>,
        snapshot: &[GoalSnapshot],
        iteration: u32,
    ) -> Result<PlanDelta, PlannerError> {
        let system = build_replan_system_prompt(tools, skills, project_memory);
        let user = build_replan_user_prompt(mission, snapshot, iteration);

        let resp = self.llm.complete(CompletionRequest {
            model: self.model.clone(),
            messages: vec![
                ChatMessage { role: "system".into(), content: system },
                ChatMessage { role: "user".into(),   content: user },
            ],
            temperature: Some(0.2),
            max_tokens: Some(1500),
            json_mode: true,
            mission_id: Some(mission.id.to_string()),
        }).await?;

        let raw = parse_replan(&resp.content, tools)?;
        Ok(materialize_delta(mission.id, raw))
    }

    /// Just-in-time task input rewriter. Called by the executor right before
    /// a goal's tasks run. Given the goal's current pending tasks and the
    /// (goal_title, tool, result_summary) tuples of every completed upstream
    /// task, ask the LLM to produce final task inputs — resolving any
    /// plan-time placeholder tokens like `"[insert directories here]"`,
    /// `"<see prior step>"`, or empty strings that the initial planner
    /// couldn't fill because the upstream data didn't exist yet.
    ///
    /// The output MUST be an array of JSON values whose length matches the
    /// number of input tasks and whose i-th element is the new `input` for
    /// tasks[i]. If the LLM output is malformed, callers should fall back to
    /// the plan-time inputs (this method returns Err in that case).
    pub async fn materialize_task_inputs(
        &self,
        mission_id: Option<MissionId>,
        mission_title: &str,
        goal_title: &str,
        goal_description: &str,
        tasks: &[(String, serde_json::Value)],
        upstream: &[(String, String, String)],
        tools: &[ToolSchema],
    ) -> Result<Vec<serde_json::Value>, PlannerError> {
        // Only include schemas for the tools this goal actually uses — keeps
        // the prompt tight and lets the LLM validate its own output.
        let used: std::collections::HashSet<&str> =
            tasks.iter().map(|(t, _)| t.as_str()).collect();
        let tool_schemas: Vec<_> = tools.iter()
            .filter(|t| used.contains(t.name.as_str()))
            .map(|t| serde_json::json!({
                "name":         t.name,
                "description":  t.description,
                "input_schema": t.input_schema,
            }))
            .collect();
        let tool_blob = serde_json::to_string_pretty(&tool_schemas).unwrap_or_default();

        let upstream_blob = if upstream.is_empty() {
            "(none)".to_string()
        } else {
            upstream.iter().enumerate().map(|(i, (g, tool, res))| {
                format!("[{i}] goal `{g}` → tool `{tool}` returned:\n    {res}")
            }).collect::<Vec<_>>().join("\n\n")
        };

        let tasks_blob = tasks.iter().enumerate().map(|(i, (tool, input))| {
            format!(
                "  {{ \"index\": {i}, \"tool\": \"{tool}\", \"current_input\": {input} }}",
                input = serde_json::to_string(input).unwrap_or_else(|_| "{}".into()),
            )
        }).collect::<Vec<_>>().join(",\n");

        let system = r#"You are the task input materializer for Forge OS. The
initial planner emitted goals + task inputs BEFORE any goal had run, so
task inputs sometimes contain placeholders like "[insert X here]",
"<see prior step>", "TODO", or empty strings that reference data only
available after upstream goals completed.

Your job: given the upstream results and the current task inputs, produce
FINAL task inputs. Each returned input must satisfy the referenced tool's
JSON schema. Preserve inputs unchanged when no placeholder resolution is
needed.

Return ONLY a JSON object of this shape (no prose, no markdown fences):

{
  "inputs": [
    { "index": 0, "input": { ... } },
    { "index": 1, "input": { ... } },
    ...
  ]
}

- One entry per input task, in the same order.
- `input` MUST be a JSON object matching the tool's input schema.
- Do NOT invent tool calls; do NOT change the tool a task uses.
- If you cannot resolve a placeholder, return the current input verbatim.
"#.to_string();

        let user = format!(
            "Mission: {mission_title}\n\
             Goal: {goal_title}\n\
             Goal description: {goal_description}\n\n\
             Tool schemas for this goal:\n{tool_blob}\n\n\
             Upstream results already available:\n{upstream_blob}\n\n\
             Current task inputs (rewrite as needed):\n[\n{tasks_blob}\n]\n",
        );

        let resp = self.llm.complete(CompletionRequest {
            model: self.model.clone(),
            messages: vec![
                ChatMessage { role: "system".into(), content: system },
                ChatMessage { role: "user".into(),   content: user },
            ],
            temperature: Some(0.1),
            max_tokens: Some(1500),
            json_mode: true,
            mission_id: mission_id.map(|m| m.to_string()),
        }).await?;

        parse_materialize(&resp.content, tasks.len())
    }
}

#[derive(Debug, Deserialize)]
struct RawMaterialize {
    inputs: Vec<RawMaterializedInput>,
}

#[derive(Debug, Deserialize)]
struct RawMaterializedInput {
    index: usize,
    input: serde_json::Value,
}

fn parse_materialize(content: &str, expected_len: usize) -> Result<Vec<serde_json::Value>, PlannerError> {
    let cleaned = content.trim();
    let cleaned = cleaned.strip_prefix("```json").or_else(|| cleaned.strip_prefix("```")).unwrap_or(cleaned);
    let cleaned = cleaned.strip_suffix("```").unwrap_or(cleaned).trim();

    let raw: RawMaterialize = serde_json::from_str(cleaned)
        .map_err(|e| PlannerError::ParseJson(e.to_string()))?;

    if raw.inputs.len() != expected_len {
        return Err(PlannerError::Invalid(format!(
            "materializer returned {} inputs, expected {}", raw.inputs.len(), expected_len
        )));
    }
    let mut out: Vec<Option<serde_json::Value>> = vec![None; expected_len];
    for entry in raw.inputs {
        if entry.index >= expected_len {
            return Err(PlannerError::Invalid(format!(
                "materializer index {} out of bounds", entry.index
            )));
        }
        if out[entry.index].is_some() {
            return Err(PlannerError::Invalid(format!(
                "materializer produced duplicate index {}", entry.index
            )));
        }
        out[entry.index] = Some(entry.input);
    }
    out.into_iter().enumerate().map(|(i, o)| o.ok_or_else(||
        PlannerError::Invalid(format!("materializer missed index {i}"))
    )).collect()
}

fn build_system_prompt(
    tools: &[ToolSchema],
    skills: &[&Skill],
    project_memory: Option<&str>,
) -> String {
    let tool_blob = serde_json::to_string_pretty(&tools.iter().map(|t| {
        serde_json::json!({
            "name":         t.name,
            "description":  t.description,
            "input_schema": t.input_schema,
        })
    }).collect::<Vec<_>>()).unwrap_or_default();

    let mut prompt = format!(r#"You are the planner for Forge OS. Break the user's mission into a
directed-acyclic graph of small goals. Each goal has one or more tasks; each
task invokes exactly one tool.

Available tools:
{tool_blob}

Return ONLY a JSON object matching this schema (no prose, no markdown fences):

{{
  "goals": [
    {{
      "id":          "g1",
      "title":       "human-readable short title",
      "description": "why this goal exists",
      "depends_on":  ["g0", ...],   // ids of earlier goals; may be empty
      "confidence":  0.0..1.0,
      "tasks": [
        {{ "tool": "<one of the tool names above>", "input": {{...}} }}
      ]
    }}
  ]
}}

Rules:
- Goal ids are strings like "g1", "g2" — unique within the plan.
- `depends_on` may only reference earlier goal ids; the graph must be acyclic.
- Every task's `input` must satisfy the referenced tool's `input_schema`.
- Prefer many small goals over one large goal.
- Keep the plan under 12 goals. If the mission is too large, plan the first
  slice; the runtime will re-plan after execution.
"#);

    if let Some(mem) = project_memory {
        prompt.push_str("\n\n---\n\nProject context (read carefully — the mission is scoped to this project):\n\n");
        prompt.push_str(mem.trim());
        prompt.push('\n');
    }

    if !skills.is_empty() {
        prompt.push_str("\n\n---\n\nAvailable playbooks (skills). Lean on these when they fit; each is proven procedure. If a skill's `keywords` overlap the mission, its body describes how to approach the work.\n");
        for s in skills {
            prompt.push_str(&format!(
                "\n### Skill: {}\n_{}_\n\n{}\n",
                s.front.name, s.front.description, s.body.trim()
            ));
        }
    }

    prompt
}

fn build_replan_system_prompt(
    tools: &[ToolSchema],
    skills: &[&Skill],
    project_memory: Option<&str>,
) -> String {
    let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    let mut prompt = format!(r#"You are the re-planner for Forge OS. You are in the middle of a mission.
The user's brief and current DAG state are provided. Decide what to do next.

Available tools: {tool_names:?}

Return ONLY a JSON object with this schema (no prose, no markdown fences):

{{
  "terminate": true|false,          // true if the mission is done (success OR unrecoverable failure)
  "reason":    "one-sentence justification",
  "added_goals": [                  // may be empty; ignored when terminate=true
    {{
      "id":          "g100",        // must not collide with existing goal ids listed in the user prompt
      "title":       "…",
      "description": "…",
      "depends_on":  ["g100", …],   // may reference existing goal ids OR new ids in this batch
      "confidence":  0.0..1.0,
      "tasks": [ {{ "tool": "…", "input": {{…}} }} ]
    }}
  ]
}}

Guidelines:
- Prefer `terminate: true` when the existing goals fulfill the mission — do
  NOT invent make-work.
- Add at most 3 new goals per replan pass. Iterate again if more is needed.
- If a previous goal failed and there is no viable retry strategy, choose
  `terminate: true` with `reason` explaining the blocker.
- New goals may depend on existing (completed or pending) goal ids.
"#);

    if let Some(mem) = project_memory {
        prompt.push_str("\n\n---\n\nProject context:\n\n");
        prompt.push_str(mem.trim());
        prompt.push('\n');
    }

    if !skills.is_empty() {
        prompt.push_str("\n\n---\n\nAvailable playbooks:\n");
        for s in skills {
            prompt.push_str(&format!(
                "\n### {}\n{}\n",
                s.front.name, s.body.trim()
            ));
        }
    }
    prompt
}

fn build_replan_user_prompt(m: &Mission, snapshot: &[GoalSnapshot], iteration: u32) -> String {
    let dag = snapshot.iter().map(|g| {
        let tasks = g.tasks.iter().map(|t| {
            let outcome = match t.status {
                TaskStatus::Completed        => format!("completed: {}", t.result_summary.as_deref().unwrap_or("")),
                TaskStatus::Failed           => format!("failed: {}", t.error.as_deref().unwrap_or("<unknown>")),
                TaskStatus::Running          => "running".to_string(),
                TaskStatus::PendingApproval  => "awaiting approval".to_string(),
                TaskStatus::Pending          => "pending".to_string(),
                TaskStatus::Cancelled        => "cancelled".to_string(),
                TaskStatus::Denied           => "denied by policy".to_string(),
            };
            format!("    - [{}] {} → {}", tool_marker(&t.status), t.tool, outcome)
        }).collect::<Vec<_>>().join("\n");
        let deps = if g.depends_on.is_empty() {
            String::new()
        } else {
            format!(" (depends on: {})", g.depends_on.iter().map(|d| d.to_string()).collect::<Vec<_>>().join(", "))
        };
        format!("- goal {} [{:?}]: {}{}\n{}", g.id, g.status, g.title, deps, tasks)
    }).collect::<Vec<_>>().join("\n");

    format!(
        "Mission title: {title}\n\
         Mission description: {desc}\n\
         Replan iteration: {iteration}\n\
         \n\
         Current DAG:\n{dag}\n",
        title = m.title, desc = m.description,
    )
}

fn tool_marker(s: &TaskStatus) -> &'static str {
    match s {
        TaskStatus::Completed => "✓",
        TaskStatus::Failed    => "✗",
        TaskStatus::Running   => "→",
        _ => "·",
    }
}

fn build_user_prompt(m: &Mission) -> String {
    format!("Mission title: {}\n\nMission description:\n{}", m.title, m.description)
}

fn parse_and_validate(content: &str, tools: &[ToolSchema]) -> Result<RawPlan, PlannerError> {
    // Some models still wrap the JSON in a fenced code block; strip it if so.
    let cleaned = content.trim();
    let cleaned = cleaned.strip_prefix("```json").or_else(|| cleaned.strip_prefix("```")).unwrap_or(cleaned);
    let cleaned = cleaned.strip_suffix("```").unwrap_or(cleaned).trim();

    let plan: RawPlan = serde_json::from_str(cleaned).map_err(|e| PlannerError::ParseJson(e.to_string()))?;
    if plan.goals.is_empty() {
        return Err(PlannerError::Empty);
    }
    let tool_names: std::collections::HashSet<&str> =
        tools.iter().map(|t| t.name.as_str()).collect();
    for g in &plan.goals {
        for t in &g.tasks {
            if !tool_names.contains(t.tool.as_str()) {
                return Err(PlannerError::Invalid(format!(
                    "goal `{}` references unknown tool `{}`", g.id, t.tool
                )));
            }
        }
    }
    // Simple cycle check on the raw ids.
    let ids: std::collections::HashSet<&str> = plan.goals.iter().map(|g| g.id.as_str()).collect();
    for g in &plan.goals {
        for d in &g.depends_on {
            if !ids.contains(d.as_str()) {
                return Err(PlannerError::Invalid(format!(
                    "goal `{}` depends on unknown `{}`", g.id, d
                )));
            }
        }
    }
    Ok(plan)
}

fn materialize(mission_id: MissionId, raw: RawPlan) -> Plan {
    // Assign real GoalIds. Keep a map from planner-local id to allocated GoalId.
    let mut mapping: HashMap<String, GoalId> = HashMap::new();
    for g in &raw.goals {
        mapping.insert(g.id.clone(), GoalId::new());
    }
    let mut goals = Vec::with_capacity(raw.goals.len());
    let mut tasks = Vec::new();
    for g in raw.goals {
        let gid = *mapping.get(&g.id).unwrap();
        let deps: Vec<GoalId> = g.depends_on.iter().filter_map(|d| mapping.get(d).copied()).collect();
        let mut goal = Goal::new(mission_id, g.title, g.description, deps);
        goal.id = gid;
        goal.confidence = g.confidence;
        for t in g.tasks {
            let task = Task::new(gid, t.tool, t.input);
            goal.tasks.push(task.id);
            tasks.push(task);
        }
        goals.push(goal);
    }
    Plan { mission_id, goals, tasks }
}

#[derive(Clone, Debug, Deserialize)]
struct RawDelta {
    #[serde(default)]
    terminate: bool,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    added_goals: Vec<RawGoal>,
}

fn parse_replan(content: &str, tools: &[ToolSchema]) -> Result<RawDelta, PlannerError> {
    let cleaned = content.trim();
    let cleaned = cleaned.strip_prefix("```json").or_else(|| cleaned.strip_prefix("```")).unwrap_or(cleaned);
    let cleaned = cleaned.strip_suffix("```").unwrap_or(cleaned).trim();

    let delta: RawDelta = serde_json::from_str(cleaned).map_err(|e| PlannerError::ParseJson(e.to_string()))?;
    let tool_names: std::collections::HashSet<&str> =
        tools.iter().map(|t| t.name.as_str()).collect();
    for g in &delta.added_goals {
        for t in &g.tasks {
            if !tool_names.contains(t.tool.as_str()) {
                return Err(PlannerError::Invalid(format!(
                    "replanned goal `{}` references unknown tool `{}`", g.id, t.tool
                )));
            }
        }
    }
    Ok(delta)
}

fn materialize_delta(mission_id: MissionId, raw: RawDelta) -> PlanDelta {
    // Build a mapping ONLY for new goal ids. Depends_on entries that refer to
    // pre-existing GoalIds (uuid strings) are ignored here — the executor
    // resolves those separately by matching on textual GoalId equality.
    // In practice, LLMs will use fresh short ids for new goals ("g100") and
    // sometimes reference the parseable form of existing goal ids by copying
    // them from the DAG snapshot.
    let mut mapping: HashMap<String, GoalId> = HashMap::new();
    for g in &raw.added_goals {
        mapping.insert(g.id.clone(), GoalId::new());
    }
    let mut goals = Vec::with_capacity(raw.added_goals.len());
    let mut tasks = Vec::new();
    for g in raw.added_goals {
        let gid = *mapping.get(&g.id).unwrap();
        // For each dep, try to interpret it as an existing GoalId first, then
        // fall back to a mapping lookup (new sibling).
        let deps: Vec<GoalId> = g.depends_on.iter().filter_map(|d| {
            use std::str::FromStr;
            if let Ok(gid) = GoalId::from_str(d) {
                Some(gid)
            } else {
                mapping.get(d).copied()
            }
        }).collect();
        let mut goal = Goal::new(mission_id, g.title, g.description, deps);
        goal.id = gid;
        goal.confidence = g.confidence;
        for t in g.tasks {
            let task = Task::new(gid, t.tool, t.input);
            goal.tasks.push(task.id);
            tasks.push(task);
        }
        goals.push(goal);
    }
    PlanDelta { mission_id, added_goals: goals, added_tasks: tasks, terminate: raw.terminate, reason: raw.reason }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_domain::{Permission, ToolSchema};

    fn tools() -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "fs.read".to_string(),
            description: "".to_string(),
            input_schema: serde_json::json!({}),
            permissions: vec![Permission::FsRead],
        }]
    }

    #[test]
    fn parses_valid_plan() {
        let raw = r#"{"goals":[{"id":"g1","title":"t","description":"d","depends_on":[],"tasks":[{"tool":"fs.read","input":{"path":"README.md"}}]}]}"#;
        let plan = parse_and_validate(raw, &tools()).unwrap();
        assert_eq!(plan.goals.len(), 1);
    }

    #[test]
    fn rejects_unknown_tool() {
        let raw = r#"{"goals":[{"id":"g1","title":"t","depends_on":[],"tasks":[{"tool":"nope","input":{}}]}]}"#;
        assert!(parse_and_validate(raw, &tools()).is_err());
    }

    #[test]
    fn materializes_deps() {
        let mid = MissionId::new();
        let raw: RawPlan = serde_json::from_str(r#"{"goals":[
            {"id":"g1","title":"a","depends_on":[],"tasks":[]},
            {"id":"g2","title":"b","depends_on":["g1"],"tasks":[]}
        ]}"#).unwrap();
        let plan = materialize(mid, raw);
        assert_eq!(plan.goals.len(), 2);
        assert_eq!(plan.goals[1].depends_on.len(), 1);
        assert_eq!(plan.goals[1].depends_on[0], plan.goals[0].id);
    }

    #[test]
    fn system_prompt_includes_skills_and_memory() {
        use forge_skills::parse_skill;
        let s = parse_skill("---\nname: rust-crate\nversion: 1.0.0\ndescription: d\n---\n# body\ncargo test\n").unwrap();
        let prompt = build_system_prompt(&tools(), &[&s], Some("this project uses tokio"));
        assert!(prompt.contains("Project context"));
        assert!(prompt.contains("this project uses tokio"));
        assert!(prompt.contains("Available playbooks"));
        assert!(prompt.contains("rust-crate"));
        assert!(prompt.contains("cargo test"));
    }

    #[test]
    fn system_prompt_omits_sections_when_empty() {
        let prompt = build_system_prompt(&tools(), &[], None);
        assert!(!prompt.contains("Project context"));
        assert!(!prompt.contains("Available playbooks"));
    }

    #[test]
    fn parses_replan_terminate() {
        let raw = r#"{"terminate": true, "reason": "all done", "added_goals": []}"#;
        let d = parse_replan(raw, &tools()).unwrap();
        assert!(d.terminate);
        assert_eq!(d.reason, "all done");
        assert!(d.added_goals.is_empty());
    }

    #[test]
    fn parses_replan_added_goals() {
        let raw = r#"{
            "terminate": false,
            "reason": "need to verify",
            "added_goals": [
                {"id": "g99", "title": "verify", "description": "d", "depends_on": [], "tasks":[{"tool":"fs.read","input":{}}]}
            ]
        }"#;
        let d = parse_replan(raw, &tools()).unwrap();
        assert!(!d.terminate);
        assert_eq!(d.added_goals.len(), 1);

        let mid = MissionId::new();
        let delta = materialize_delta(mid, d);
        assert_eq!(delta.added_goals.len(), 1);
        assert_eq!(delta.added_tasks.len(), 1);
        assert!(!delta.terminate);
    }

    #[test]
    fn replan_rejects_unknown_tool() {
        let raw = r#"{"terminate": false, "added_goals": [
            {"id":"g1","title":"t","description":"d","depends_on":[],"tasks":[{"tool":"badtool","input":{}}]}
        ]}"#;
        assert!(parse_replan(raw, &tools()).is_err());
    }

    #[test]
    fn materializer_parses_reordered_indices() {
        let raw = r#"{"inputs":[
            {"index":1,"input":{"path":"WORKSPACE_SUMMARY.md","content":"Layout: dir1, dir2"}},
            {"index":0,"input":{"path":"."}}
        ]}"#;
        let out = parse_materialize(raw, 2).unwrap();
        assert_eq!(out[0], serde_json::json!({"path":"."}));
        assert_eq!(out[1]["path"], serde_json::json!("WORKSPACE_SUMMARY.md"));
    }

    #[test]
    fn materializer_rejects_length_mismatch() {
        let raw = r#"{"inputs":[{"index":0,"input":{}}]}"#;
        let err = parse_materialize(raw, 3).unwrap_err();
        matches!(err, PlannerError::Invalid(_));
    }

    #[test]
    fn materializer_rejects_out_of_bounds_index() {
        let raw = r#"{"inputs":[{"index":5,"input":{}},{"index":1,"input":{}}]}"#;
        assert!(parse_materialize(raw, 2).is_err());
    }

    #[test]
    fn materializer_rejects_duplicate_index() {
        let raw = r#"{"inputs":[{"index":0,"input":{}},{"index":0,"input":{}}]}"#;
        assert!(parse_materialize(raw, 2).is_err());
    }

    #[test]
    fn materializer_strips_code_fences() {
        let raw = "```json\n{\"inputs\":[{\"index\":0,\"input\":{\"a\":1}}]}\n```";
        let out = parse_materialize(raw, 1).unwrap();
        assert_eq!(out[0], serde_json::json!({"a":1}));
    }
}
