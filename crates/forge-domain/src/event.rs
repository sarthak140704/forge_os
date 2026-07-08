use crate::{EventId, GoalId, GoalStatus, MissionId, MissionStatus, TaskId};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Every state change in Forge OS is an event. Events are appended to the
/// event store, broadcast in-process, and emitted to the UI. Nothing that
/// changes state should escape this enum.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ForgeEvent {
    MissionCreated { id: MissionId, title: String },
    MissionPlanningStarted { id: MissionId },
    MissionPlanningCompleted { id: MissionId, goal_count: usize },
    MissionPlanningFailed { id: MissionId, error: String },
    MissionStatusChanged { id: MissionId, from: MissionStatus, to: MissionStatus },
    /// User-initiated cancellation was requested (before the executor's
    /// cooperative cancellation token flips the mission to `Cancelled`).
    MissionCancelRequested { id: MissionId },

    GoalCreated { id: GoalId, mission_id: MissionId, title: String, depends_on: Vec<GoalId> },
    GoalStatusChanged { id: GoalId, from: GoalStatus, to: GoalStatus },

    TaskCreated { id: TaskId, goal_id: GoalId, tool: String },
    TaskStarted { id: TaskId },
    TaskCompleted { id: TaskId, result_summary: String },
    TaskFailed { id: TaskId, error: String },

    ToolInvoked { task_id: TaskId, tool: String },
    PolicyDenied { task_id: TaskId, rule: String, reason: String },
    PolicyApprovalRequested { task_id: TaskId, rule: String, reason: String },
    PolicyApprovalGranted { task_id: TaskId },

    LlmRequested { request_id: String, provider: String, model: String, #[serde(default)] mission_id: Option<MissionId> },
    LlmResponded { request_id: String, latency_ms: u64, prompt_tokens: usize, completion_tokens: usize, #[serde(default)] mission_id: Option<MissionId>, #[serde(default)] provider: String, #[serde(default)] model: String },
    LlmFailed { request_id: String, provider: String, error: String, #[serde(default)] mission_id: Option<MissionId> },

    // ---- Phase 2: skills + continuous re-planning + reflection ----
    /// Emitted when the mission's initial plan is being computed and the
    /// planner has decided which skills (if any) to lean on.
    SkillsSelected { mission_id: MissionId, skill_names: Vec<String> },

    /// Emitted before each replan pass. `iteration` is 1-indexed; iteration 0
    /// is the initial plan (recorded via `MissionPlanningStarted`).
    ReplanRequested { mission_id: MissionId, iteration: u32 },

    /// Emitted after a successful replan. `added_goals` counts new goals
    /// created by this pass — zero means the planner chose to terminate.
    PlanRevised { mission_id: MissionId, iteration: u32, added_goals: usize },

    /// Emitted when the replan loop hits its safety cap. Mission proceeds
    /// with whatever plan already exists — we never runaway-loop.
    ReplanCapExceeded { mission_id: MissionId, iteration: u32 },

    /// Emitted once the reflector has produced a post-mission analysis.
    /// The full artifact lives in the reflections table; this event only
    /// carries a summary for the UI.
    MissionReflectionCompleted {
        mission_id: MissionId,
        insights_count: usize,
        suggested_skills: Vec<String>,
    },

    /// Emitted for each skill proposal the reflector wrote to disk.
    SkillProposalWritten { mission_id: MissionId, name: String, path: String },

    // ---- Phase 2: MCP plugins ----
    /// A configured MCP server was started and reported its tools.
    /// `tools` is the list of tool names as they appear in the forge registry
    /// (already `mcp.<server>.<tool>`-namespaced).
    McpServerStarted { name: String, tools: Vec<String> },

    /// A configured MCP server failed to start or crashed during boot. Runtime
    /// continues without it; this event is the only surface.
    McpServerFailed { name: String, error: String },

    /// An MCP-hosted tool was invoked. Distinct from `ToolInvoked` (which is
    /// task-scoped) — this is the plugin-boundary event so we can attribute
    /// latency/cost to a specific server. Emitted immediately before the call.
    McpToolInvoked { server: String, tool: String, task_id: TaskId },

    /// A task's `input` was rewritten at execution time using the results of
    /// its goal's upstream dependencies. This closes the gap where the initial
    /// planner emits placeholder args (e.g. "[insert directories here]") that
    /// cannot be filled in until earlier goals have actually run. Emitted
    /// once per refreshed task, immediately before the policy check.
    TaskInputRefreshed { task_id: TaskId, tool: String },

    /// Roll-up of LLM cost for a mission. Emitted once after the mission
    /// reaches a terminal state (Completed/Failed/Cancelled). Aggregates
    /// every LlmResponded event whose `mission_id` matches, so operators can
    /// see per-mission token spend without scanning the event stream.
    MissionCostSummary {
        mission_id:        MissionId,
        llm_calls:         usize,
        prompt_tokens:     usize,
        completion_tokens: usize,
        total_latency_ms:  u64,
    },

    /// Episodic recall surfaced prior missions to seed the planner. Emitted
    /// once per plan/replan attempt when the recall block is non-empty so
    /// operators can see which past attempts influenced this plan.
    EpisodicRecallSurfaced {
        mission_id:  MissionId,
        keywords:    Vec<String>,
        prior_count: usize,
        /// First ~300 chars of the injected recall block, for quick UI display.
        block_preview: String,
    },

    /// A shadow-git checkpoint was written after a mutating tool. Surfaces
    /// in the timeline so users can spot recoverable snapshots without opening
    /// the settings modal.
    CheckpointCreated {
        sha:        String,
        short_sha:  String,
        tool:       String,
        mission_id: Option<MissionId>,
        task_id:    Option<TaskId>,
        label:      String,
    },

    /// A mutating tool ran but the shadow-git store found no workspace
    /// changes to commit (e.g. the same bytes were re-written). Surfaced so
    /// the UI can show clear "no-op" feedback instead of silently swallowing
    /// the auto-snapshot attempt.
    CheckpointSkipped {
        tool:       String,
        mission_id: Option<MissionId>,
        task_id:    Option<TaskId>,
        reason:     String,
    },

    // ---- Phase 4a: version-controlled skills ----

    /// A skill was promoted — either from a proposal, hand-authored on disk,
    /// or via rollback. Every promotion appends a row to `skills_history`
    /// and snapshots the SKILL.md bytes in the content-addressed store.
    /// Reversible via `SkillRolledBack`.
    SkillPromoted {
        name:              String,
        sha:               String,
        version:           String,
        origin:            String, // handcrafted | proposal | curated | rollback
        parent_sha:        Option<String>,
        origin_mission_id: Option<MissionId>,
    },

    /// The active version of a skill was replaced by a prior version.
    /// Not destructive — the "new" active row appends to history like any
    /// other promotion, but its `origin=rollback` and its bytes are read
    /// verbatim from the content store at `target_sha`.
    SkillRolledBack {
        name:            String,
        from_sha:        Option<String>,
        to_sha:          String,
        reason:          Option<String>,
    },

    /// A skill was retired — its active history row now has `retired_at`
    /// set and the active file was moved to `archived/`. Skills can be
    /// re-promoted later; retirement is not deletion.
    SkillRetired {
        name:   String,
        sha:    String,
        reason: String,
    },

    /// The curator flagged a skill as a candidate for merge/dedupe/archive.
    /// Purely advisory — the timeline surfaces it so the operator can
    /// decide. The `kind` field is one of: `duplicate`, `unused`,
    /// `merge_candidate`.
    SkillCurationSuggested {
        name:     String,
        kind:     String,
        evidence: String,
    },

    /// A proposal cleared every hard validator check. Emitted BEFORE the
    /// promotion writes to disk so audit trails show validation gates were
    /// run first. `soft_failures` lists the ids of any warnings (e.g.
    /// keyword collision) that did not block promotion.
    SkillValidationPassed {
        filename:      String,
        name:          String,
        soft_failures: Vec<String>,
    },

    /// A proposal tripped at least one hard validator check. The proposal
    /// remains in `proposed/`; no active state changed. `failed_checks`
    /// lists the ids that failed (e.g. `body_length`, `tools_resolvable`).
    SkillValidationFailed {
        filename:      String,
        name:          String,
        failed_checks: Vec<String>,
    },

    /// The autopromoter background loop promoted a proposal without human
    /// approval because it passed validation and the runtime is configured
    /// with `auto_promote_skills = true`. The corresponding
    /// `SkillPromoted` event is also published; this variant exists so the
    /// timeline can distinguish "human clicked approve" from "autopromoter
    /// approved on its own".
    SkillAutoPromoted {
        name:    String,
        sha:     String,
        version: String,
    },
}

impl ForgeEvent {
    pub fn aggregate_id(&self) -> String {
        use ForgeEvent::*;
        match self {
            MissionCreated { id, .. }
            | MissionPlanningStarted { id }
            | MissionPlanningCompleted { id, .. }
            | MissionPlanningFailed { id, .. }
            | MissionStatusChanged { id, .. }
            | MissionCancelRequested { id } => id.to_string(),

            GoalCreated { id, .. }
            | GoalStatusChanged { id, .. } => id.to_string(),

            TaskCreated { id, .. }
            | TaskStarted { id }
            | TaskCompleted { id, .. }
            | TaskFailed { id, .. }
            | ToolInvoked { task_id: id, .. }
            | PolicyDenied { task_id: id, .. }
            | PolicyApprovalRequested { task_id: id, .. }
            | PolicyApprovalGranted { task_id: id }
            | TaskInputRefreshed { task_id: id, .. } => id.to_string(),

            LlmRequested { request_id, .. }
            | LlmResponded { request_id, .. }
            | LlmFailed { request_id, .. } => format!("llm_{request_id}"),

            SkillsSelected { mission_id, .. }
            | ReplanRequested { mission_id, .. }
            | PlanRevised { mission_id, .. }
            | ReplanCapExceeded { mission_id, .. }
            | MissionReflectionCompleted { mission_id, .. }
            | SkillProposalWritten { mission_id, .. }
            | MissionCostSummary { mission_id, .. }
            | EpisodicRecallSurfaced { mission_id, .. } => mission_id.to_string(),

            McpServerStarted { name, .. }
            | McpServerFailed { name, .. } => format!("mcp_{name}"),
            McpToolInvoked { server, tool, .. } => format!("mcp_{server}_{tool}"),

            CheckpointCreated { sha, .. } => format!("checkpoint_{sha}"),
            CheckpointSkipped { task_id, .. } => task_id.map(|t| t.to_string()).unwrap_or_else(|| "checkpoint_skip".into()),

            SkillPromoted { name, .. }
            | SkillRolledBack { name, .. }
            | SkillRetired { name, .. }
            | SkillCurationSuggested { name, .. }
            | SkillAutoPromoted { name, .. } => format!("skill_{name}"),

            SkillValidationPassed { name, .. }
            | SkillValidationFailed { name, .. } => format!("skill_{name}"),
        }
    }

    pub fn kind(&self) -> AggregateKind {
        use ForgeEvent::*;
        match self {
            MissionCreated { .. }
            | MissionPlanningStarted { .. }
            | MissionPlanningCompleted { .. }
            | MissionPlanningFailed { .. }
            | MissionStatusChanged { .. }
            | MissionCancelRequested { .. } => AggregateKind::Mission,
            GoalCreated { .. } | GoalStatusChanged { .. } => AggregateKind::Goal,
            TaskCreated { .. }
            | TaskStarted { .. }
            | TaskCompleted { .. }
            | TaskFailed { .. }
            | ToolInvoked { .. }
            | PolicyDenied { .. }
            | PolicyApprovalRequested { .. }
            | PolicyApprovalGranted { .. }
            | TaskInputRefreshed { .. } => AggregateKind::Task,
            LlmRequested { .. } | LlmResponded { .. } | LlmFailed { .. } => AggregateKind::Llm,

            SkillsSelected { .. }
            | ReplanRequested { .. }
            | PlanRevised { .. }
            | ReplanCapExceeded { .. }
            | MissionReflectionCompleted { .. }
            | SkillProposalWritten { .. }
            | MissionCostSummary { .. }
            | EpisodicRecallSurfaced { .. } => AggregateKind::Mission,

            McpServerStarted { .. }
            | McpServerFailed { .. }
            | McpToolInvoked { .. } => AggregateKind::Plugin,

            CheckpointCreated { .. } => AggregateKind::Mission,
            CheckpointSkipped { .. } => AggregateKind::Mission,

            SkillPromoted { .. }
            | SkillRolledBack { .. }
            | SkillRetired { .. }
            | SkillCurationSuggested { .. }
            | SkillValidationPassed { .. }
            | SkillValidationFailed { .. }
            | SkillAutoPromoted { .. } => AggregateKind::Skill,
        }
    }

    pub fn event_type(&self) -> &'static str {
        use ForgeEvent::*;
        match self {
            MissionCreated { .. } => "mission_created",
            MissionPlanningStarted { .. } => "mission_planning_started",
            MissionPlanningCompleted { .. } => "mission_planning_completed",
            MissionPlanningFailed { .. } => "mission_planning_failed",
            MissionStatusChanged { .. } => "mission_status_changed",
            MissionCancelRequested { .. } => "mission_cancel_requested",
            GoalCreated { .. } => "goal_created",
            GoalStatusChanged { .. } => "goal_status_changed",
            TaskCreated { .. } => "task_created",
            TaskStarted { .. } => "task_started",
            TaskCompleted { .. } => "task_completed",
            TaskFailed { .. } => "task_failed",
            ToolInvoked { .. } => "tool_invoked",
            PolicyDenied { .. } => "policy_denied",
            PolicyApprovalRequested { .. } => "policy_approval_requested",
            PolicyApprovalGranted { .. } => "policy_approval_granted",
            LlmRequested { .. } => "llm_requested",
            LlmResponded { .. } => "llm_responded",
            LlmFailed { .. } => "llm_failed",

            SkillsSelected { .. } => "skills_selected",
            ReplanRequested { .. } => "replan_requested",
            PlanRevised { .. } => "plan_revised",
            ReplanCapExceeded { .. } => "replan_cap_exceeded",
            MissionReflectionCompleted { .. } => "mission_reflection_completed",
            SkillProposalWritten { .. } => "skill_proposal_written",

            McpServerStarted { .. } => "mcp_server_started",
            McpServerFailed { .. } => "mcp_server_failed",
            McpToolInvoked { .. } => "mcp_tool_invoked",
            TaskInputRefreshed { .. } => "task_input_refreshed",
            MissionCostSummary { .. } => "mission_cost_summary",
            EpisodicRecallSurfaced { .. } => "episodic_recall_surfaced",
            CheckpointCreated { .. } => "checkpoint_created",
            CheckpointSkipped { .. } => "checkpoint_skipped",
            SkillPromoted { .. } => "skill_promoted",
            SkillRolledBack { .. } => "skill_rolled_back",
            SkillRetired { .. } => "skill_retired",
            SkillCurationSuggested { .. } => "skill_curation_suggested",
            SkillValidationPassed { .. } => "skill_validation_passed",
            SkillValidationFailed { .. } => "skill_validation_failed",
            SkillAutoPromoted { .. } => "skill_auto_promoted",
        }
    }
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregateKind {
    Mission,
    Goal,
    Task,
    Llm,
    Plugin,
    Skill,
}

impl AggregateKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            AggregateKind::Mission => "mission",
            AggregateKind::Goal => "goal",
            AggregateKind::Task => "task",
            AggregateKind::Llm => "llm",
            AggregateKind::Plugin => "plugin",
            AggregateKind::Skill => "skill",
        }
    }
}

/// Envelope written to the event store: sequence + timestamp + payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub seq: EventId,
    #[serde(with = "time::serde::rfc3339")]
    pub ts: OffsetDateTime,
    pub event: ForgeEvent,
}
