//! Mission facade — the API surface the runtime exposes to the outside world
//! (Tauri IPC, tests, an eventual HTTP server).

use forge_domain::{ForgeEvent, Goal, GoalId, GoalStatus, Mission, MissionId, MissionStatus, MissionSummary, Task, TaskId, TaskStatus};
use forge_events::EventBus;
use forge_execution::ExecutionEngine;
use forge_llm::LlmRouter;
use forge_persistence::{GoalRepository, MissionRepository, ReflectionRepository, TaskRepository};
use forge_planner::{GoalSnapshot, Planner, Reflector, TaskSnapshot};
use forge_skills::{ProposalWriter, SkillRegistry};
use forge_tools::ToolRegistry;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MissionError {
    #[error("persistence: {0}")]
    Persist(#[from] forge_persistence::PersistenceError),
    #[error("events: {0}")]
    Events(#[from] forge_events::EventBusError),
    #[error("planner: {0}")]
    Planner(#[from] forge_planner::PlannerError),
    #[error("execution: {0}")]
    Execution(#[from] forge_execution::ExecutionError),
    #[error("invalid state transition: {0}")]
    State(String),
}

/// Cap on continuous re-planning iterations per mission. The initial plan is
/// iteration 0; each replan is 1..=REPLAN_CAP. Prevents runaway loops.
pub const REPLAN_CAP: u32 = 5;

/// Hard ceiling on total goals per mission — safety valve if the planner
/// insists on adding more.
pub const TOTAL_GOAL_CAP: usize = 30;

/// Configuration for the reflection + learning pass. Optional — if the
/// reflector or proposal writer are missing, that path is skipped silently.
#[derive(Clone)]
pub struct LearningDeps {
    pub reflector:       Option<Arc<Reflector>>,
    pub proposal_writer: Option<Arc<ProposalWriter>>,
    pub reflections:     Arc<dyn ReflectionRepository>,
}

#[derive(Clone)]
pub struct MissionService {
    pub missions:  Arc<dyn MissionRepository>,
    pub goals:     Arc<dyn GoalRepository>,
    pub tasks:     Arc<dyn TaskRepository>,
    pub events:    EventBus,
    pub planner:   Arc<Planner>,
    pub execution: ExecutionEngine,
    pub tools:     Arc<ToolRegistry>,
    pub skills:    Arc<SkillRegistry>,
    pub learning:  LearningDeps,
    /// Optional project memory block loaded once at runtime boot. Injected
    /// into every planner call verbatim.
    pub project_memory: Option<Arc<str>>,
    /// LLM router — held so we can drain per-mission cost buckets after
    /// terminal transitions and emit `MissionCostSummary` events. Optional
    /// (tests may not care about cost tracking).
    pub llm_router: Option<Arc<LlmRouter>>,
    /// Episodic recall: opaque async producer that, given the current
    /// mission, returns a "prior attempts" block to inject alongside the
    /// project memory. `None` disables recall. The runtime supplies an
    /// implementation backed by `MissionRepository::search_similar`.
    pub episodic_recall: Option<Arc<dyn EpisodicRecall>>,
}

/// Recall provider — the runtime binds this to a keyword search over prior
/// Episodic recall — build a "prior attempts" block that summarizes matching
/// terminal missions + their reflections. Kept as a trait so the mission
/// crate doesn't depend on the concrete search impl.
#[async_trait::async_trait]
pub trait EpisodicRecall: Send + Sync {
    async fn recall_for(&self, mission: &Mission) -> Option<RecallSurface>;
}

/// What `EpisodicRecall::recall_for` returns when it has something to inject.
/// The `block` gets merged into planner memory; the other fields drive the
/// `EpisodicRecallSurfaced` event so operators can see what influenced the plan.
#[derive(Clone, Debug)]
pub struct RecallSurface {
    pub block:       String,
    pub keywords:    Vec<String>,
    pub prior_count: usize,
}

impl MissionService {
    pub async fn create(&self, title: String, description: String) -> Result<MissionId, MissionError> {
        let mission = Mission::new_draft(title.clone(), description);
        self.missions.insert(&mission).await?;
        self.events.publish(ForgeEvent::MissionCreated {
            id: mission.id, title,
        }).await?;
        Ok(mission.id)
    }

    /// Plan (LLM call) + persist goals/tasks + hand off to executor.
    /// Runs the executor + replan loop as a background task; returns
    /// immediately with the id.
    pub async fn plan_and_run(&self, id: MissionId) -> Result<(), MissionError> {
        // Step 1: transition Draft → Planning.
        let mut mission = self.missions.get(id).await?;
        if !mission.status.can_transition(&MissionStatus::Planning) {
            return Err(MissionError::State(format!(
                "mission {id} in {:?} cannot transition to Planning", mission.status
            )));
        }
        let from = mission.status.clone();
        mission.transition_to(MissionStatus::Planning).map_err(|e| MissionError::State(e.to_string()))?;
        self.missions.update(&mission).await?;
        self.events.publish(ForgeEvent::MissionStatusChanged { id, from, to: MissionStatus::Planning }).await?;
        self.events.publish(ForgeEvent::MissionPlanningStarted { id }).await?;

        // Step 2: select skills.
        let matches = self.skills.select_for_mission(&mission.title, &mission.description);
        let selected: Vec<&forge_skills::Skill> = matches.iter().take(4).map(|m| m.skill).collect();
        let selected_names: Vec<String> = selected.iter().map(|s| s.front.name.clone()).collect();
        if !selected_names.is_empty() {
            self.events.publish(ForgeEvent::SkillsSelected {
                mission_id: id, skill_names: selected_names.clone(),
            }).await?;
        }

        // Step 3: initial plan.
        let recall: Option<RecallSurface> = if let Some(ep) = self.episodic_recall.as_ref() {
            ep.recall_for(&mission).await
        } else { None };
        if let Some(r) = recall.as_ref() {
            let preview: String = r.block.chars().take(300).collect();
            let _ = self.events.publish(ForgeEvent::EpisodicRecallSurfaced {
                mission_id: id,
                keywords: r.keywords.clone(),
                prior_count: r.prior_count,
                block_preview: preview,
            }).await;
        }
        let recall_block = recall.as_ref().map(|r| r.block.as_str());
        let combined_memory: Option<String> = match (self.project_memory.as_deref(), recall_block) {
            (Some(pm), Some(rb)) => Some(format!("{rb}\n\n---\n\n{pm}")),
            (Some(pm), None)     => Some(pm.to_string()),
            (None,     Some(rb)) => Some(rb.to_string()),
            (None,     None)     => None,
        };
        let memory = combined_memory.as_deref();
        let plan = match self.planner.plan(&mission, &self.tools.schemas(), &selected, memory).await {
            Ok(p) => p,
            Err(e) => {
                self.events.publish(ForgeEvent::MissionPlanningFailed { id, error: e.to_string() }).await?;
                let from = mission.status.clone();
                mission.transition_to(MissionStatus::Failed).ok();
                self.missions.update(&mission).await?;
                self.events.publish(ForgeEvent::MissionStatusChanged { id, from, to: MissionStatus::Failed }).await?;
                return Err(MissionError::Planner(e));
            }
        };

        for goal in &plan.goals { self.persist_new_goal(id, goal).await?; }
        for task in &plan.tasks { self.persist_new_task(task).await?; }
        self.events.publish(ForgeEvent::MissionPlanningCompleted { id, goal_count: plan.goals.len() }).await?;

        // Transition Planning → Ready → Running.
        for next in [MissionStatus::Ready, MissionStatus::Running] {
            let from = mission.status.clone();
            mission.transition_to(next.clone()).map_err(|e| MissionError::State(e.to_string()))?;
            self.missions.update(&mission).await?;
            self.events.publish(ForgeEvent::MissionStatusChanged { id, from, to: next }).await?;
        }

        // Kick off the replan loop + reflection in the background so IPC returns fast.
        let this = self.clone();
        let selected_owned: Vec<forge_skills::Skill> = selected.into_iter().cloned().collect();
        tokio::spawn(async move {
            if let Err(e) = this.run_with_replan_loop(id, selected_owned).await {
                tracing::error!(mission_id = %id, err = %e, "mission loop failed");
            }
            // Drain per-mission LLM cost bucket and emit summary event.
            // Best-effort — router may be absent (tests) or bucket may be empty
            // (mission never called an LLM successfully).
            if let Some(router) = this.llm_router.as_ref() {
                if let Some(cost) = router.drain_mission_cost(&id.to_string()) {
                    let _ = this.events.publish(ForgeEvent::MissionCostSummary {
                        mission_id: id,
                        llm_calls:  cost.calls,
                        prompt_tokens:     cost.prompt_tokens,
                        completion_tokens: cost.completion_tokens,
                        total_latency_ms:  cost.total_latency_ms,
                    }).await;
                }
            }
            if let Err(e) = this.reflect_and_learn(id).await {
                tracing::warn!(mission_id = %id, err = %e, "reflection pass failed; continuing");
            }
        });
        Ok(())
    }

    /// Run the executor; then repeatedly ask the planner to replan until it
    /// terminates, we hit the cap, the mission is cancelled, or the total
    /// goal count exceeds `TOTAL_GOAL_CAP`.
    async fn run_with_replan_loop(
        &self,
        id: MissionId,
        selected_skills: Vec<forge_skills::Skill>,
    ) -> Result<(), MissionError> {
        let skill_refs: Vec<&forge_skills::Skill> = selected_skills.iter().collect();
        let memory = self.project_memory.as_deref();

        for iteration in 1..=REPLAN_CAP {
            // Execute whatever's currently in the DAG until quiescent.
            if let Err(e) = self.execution.run_mission(id).await {
                tracing::error!(mission_id = %id, err = %e, "execution wave failed");
                return Err(MissionError::Execution(e));
            }

            // Check whether mission is already terminal (cancelled by user, all completed, all failed).
            let mission = self.missions.get(id).await?;
            if matches!(mission.status, MissionStatus::Cancelled | MissionStatus::Failed | MissionStatus::Completed) {
                tracing::info!(mission_id = %id, status = ?mission.status, iteration, "mission terminal, exiting replan loop");
                return Ok(());
            }

            // Ask planner to replan.
            self.events.publish(ForgeEvent::ReplanRequested { mission_id: id, iteration }).await?;
            let snapshot = self.build_snapshot(id).await?;

            let delta = match self.planner.replan(
                &mission, &self.tools.schemas(), &skill_refs, memory, &snapshot, iteration,
            ).await {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(mission_id = %id, iteration, err = %e, "replan call failed; treating as terminate");
                    self.events.publish(ForgeEvent::PlanRevised { mission_id: id, iteration, added_goals: 0 }).await?;
                    break;
                }
            };

            if delta.terminate {
                tracing::info!(mission_id = %id, iteration, reason = %delta.reason, "planner terminated mission");
                self.events.publish(ForgeEvent::PlanRevised { mission_id: id, iteration, added_goals: 0 }).await?;
                break;
            }

            // Enforce total goal cap.
            let existing = self.goals.list_for_mission(id).await?.len();
            let would_add = delta.added_goals.len();
            if existing + would_add > TOTAL_GOAL_CAP {
                tracing::warn!(mission_id = %id, iteration, existing, would_add, "replan would exceed total goal cap; stopping");
                self.events.publish(ForgeEvent::ReplanCapExceeded { mission_id: id, iteration }).await?;
                break;
            }
            if would_add == 0 {
                // Empty non-terminate delta: nothing more the planner wants to add.
                self.events.publish(ForgeEvent::PlanRevised { mission_id: id, iteration, added_goals: 0 }).await?;
                break;
            }

            for g in &delta.added_goals { self.persist_new_goal(id, g).await?; }
            for t in &delta.added_tasks { self.persist_new_task(t).await?; }
            self.events.publish(ForgeEvent::PlanRevised { mission_id: id, iteration, added_goals: would_add }).await?;

            if iteration == REPLAN_CAP {
                self.events.publish(ForgeEvent::ReplanCapExceeded { mission_id: id, iteration }).await?;
            }
        }

        // Final drain — pick up any goals added in the last replan iteration.
        if let Err(e) = self.execution.run_mission(id).await {
            tracing::error!(mission_id = %id, err = %e, "final execution wave failed");
            return Err(MissionError::Execution(e));
        }

        // The executor only transitions to Completed / Cancelled — it leaves
        // failed missions in Running so the replan loop can add corrective
        // goals. Now that we've exhausted the loop, take responsibility for
        // flipping Running → Failed if any goal ended in Failed and the
        // mission never made it to Completed.
        let mut mission = self.missions.get(id).await?;
        if matches!(mission.status, MissionStatus::Running) {
            let goals = self.goals.list_for_mission(id).await?;
            let has_failed = goals.iter().any(|g| matches!(g.status, forge_domain::GoalStatus::Failed));
            let all_done = goals.iter().all(|g| matches!(
                g.status,
                forge_domain::GoalStatus::Completed | forge_domain::GoalStatus::Skipped
            ));
            let next = if has_failed {
                MissionStatus::Failed
            } else if all_done {
                MissionStatus::Completed
            } else {
                // Still work in progress somehow — leave as-is; the executor
                // is idempotent and will pick it up on the next call.
                return Ok(());
            };
            if mission.status.can_transition(&next) {
                let from = mission.status.clone();
                mission.transition_to(next.clone()).ok();
                self.missions.update(&mission).await?;
                self.events.publish(ForgeEvent::MissionStatusChanged {
                    id, from, to: next,
                }).await?;
            }
        }
        Ok(())
    }

    /// Post-mission reflection + skill proposal writing. Best-effort — any
    /// failure is logged but does not affect the mission's status.
    async fn reflect_and_learn(&self, id: MissionId) -> Result<(), MissionError> {
        let mission = self.missions.get(id).await?;
        let outcome = format!("{:?}", mission.status);

        let Some(reflector) = self.learning.reflector.clone() else {
            tracing::debug!(mission_id = %id, "no reflector configured; skipping reflection");
            return Ok(());
        };

        // Build a compact event summary from what we've persisted.
        let goals = self.goals.list_for_mission(id).await?;
        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("mission title:       {}", mission.title));
        lines.push(format!("mission description: {}", mission.description));
        lines.push(format!("final status:        {outcome}"));
        for g in &goals {
            lines.push(format!("- goal {} [{:?}] {}", g.id, g.status, g.title));
            for t in self.tasks.list_for_goal(g.id).await.unwrap_or_default() {
                let tail = match t.status {
                    TaskStatus::Completed => t.result.as_ref().map(|v| format!(" → {}", truncate(&v.to_string(), 200))).unwrap_or_default(),
                    TaskStatus::Failed    => t.error.as_ref().map(|e| format!(" ✗ {}", truncate(e, 200))).unwrap_or_default(),
                    _ => String::new(),
                };
                lines.push(format!("    · {} [{:?}]{}", t.tool, t.status, tail));
            }
        }
        let summary = lines.join("\n");

        let reflection = match reflector.reflect(Some(id), &mission.title, &mission.description, &outcome, &summary).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(mission_id = %id, err = %e, "reflection LLM call failed");
                return Ok(());
            }
        };

        // Persist reflection.
        let payload = serde_json::to_string(&reflection).unwrap_or_else(|_| "{}".into());
        if let Err(e) = self.learning.reflections.insert(id, &outcome, &payload).await {
            tracing::warn!(mission_id = %id, err = %e, "failed to persist reflection");
        }

        // Write skill proposals.
        let mut written_names: Vec<String> = Vec::new();
        if let Some(writer) = self.learning.proposal_writer.clone() {
            // Dedup: skip suggestions whose name matches an already-active or
            // already-proposed skill. Reflectors on different missions often
            // hallucinate the same skill; approving each duplicate wastes review.
            let active_names = self.skills.names();
            let pending_names: std::collections::HashSet<String> = writer
                .list_proposal_names()
                .unwrap_or_default()
                .into_iter()
                .collect();
            for suggestion in &reflection.suggested_skills {
                if active_names.contains(&suggestion.name) {
                    tracing::info!(mission_id = %id, name = %suggestion.name, "dedup: suggestion matches an active skill; skipping");
                    continue;
                }
                if pending_names.contains(&suggestion.name) {
                    tracing::info!(mission_id = %id, name = %suggestion.name, "dedup: proposal with same name already pending; skipping");
                    continue;
                }
                let mut suggestion = suggestion.clone();
                suggestion.origin_mission_id = id.to_string();
                match writer.write_proposal(&suggestion) {
                    Ok(path) => {
                        self.events.publish(ForgeEvent::SkillProposalWritten {
                            mission_id: id,
                            name: suggestion.name.clone(),
                            path: path.to_string_lossy().to_string(),
                        }).await.ok();
                        written_names.push(suggestion.name.clone());
                    }
                    Err(e) => tracing::warn!(mission_id = %id, name = %suggestion.name, err = %e, "failed to write proposal"),
                }
            }
        }

        self.events.publish(ForgeEvent::MissionReflectionCompleted {
            mission_id: id,
            insights_count: reflection.insights.len(),
            suggested_skills: written_names,
        }).await?;
        Ok(())
    }

    /// Build a snapshot of the current DAG for the replanner.
    async fn build_snapshot(&self, mission_id: MissionId) -> Result<Vec<GoalSnapshot>, MissionError> {
        let goals = self.goals.list_for_mission(mission_id).await?;
        let mut out: Vec<GoalSnapshot> = Vec::with_capacity(goals.len());
        for g in goals {
            let tasks = self.tasks.list_for_goal(g.id).await.unwrap_or_default();
            let task_snaps: Vec<TaskSnapshot> = tasks.into_iter().map(|t| TaskSnapshot {
                tool: t.tool,
                status: t.status,
                result_summary: t.result.as_ref().map(|v| truncate(&v.to_string(), 240)),
                error: t.error,
            }).collect();
            out.push(GoalSnapshot {
                id: g.id,
                title: g.title,
                description: g.description,
                status: g.status,
                tasks: task_snaps,
                depends_on: g.depends_on,
            });
        }
        Ok(out)
    }

    async fn persist_new_goal(&self, mission_id: MissionId, goal: &Goal) -> Result<(), MissionError> {
        self.goals.insert(goal).await?;
        self.events.publish(ForgeEvent::GoalCreated {
            id: goal.id, mission_id, title: goal.title.clone(), depends_on: goal.depends_on.clone(),
        }).await?;
        Ok(())
    }

    async fn persist_new_task(&self, task: &Task) -> Result<(), MissionError> {
        self.tasks.insert(task).await?;
        self.events.publish(ForgeEvent::TaskCreated {
            id: task.id, goal_id: task.goal_id, tool: task.tool.clone(),
        }).await?;
        Ok(())
    }

    pub async fn cancel(&self, id: MissionId) -> Result<(), MissionError> {
        // Emit BEFORE calling execution.cancel — the token flip is
        // synchronous but the mission may not actually transition to
        // Cancelled for a while (waiting for tasks/approvals to notice).
        // Emitting first gives the UI immediate feedback.
        let _ = self.events.publish(ForgeEvent::MissionCancelRequested { id }).await;
        self.execution.cancel(id);
        Ok(())
    }

    /// Extend an existing mission with a follow-up prompt.
    ///
    /// Semantics:
    ///   * Mission must be terminal (Completed / Failed / Cancelled). Extending
    ///     a running mission is intentionally not supported yet — the replan
    ///     loop already handles automatic mid-run refinement.
    ///   * The prompt is appended to the mission's description under a
    ///     "Follow-up request" marker so the planner sees it as new context.
    ///   * The mission is transitioned Terminal → Draft, then plan_and_run
    ///     is invoked again. Existing goals/tasks are preserved in the DB;
    ///     the planner will emit a fresh set of goals that reference (or
    ///     ignore) the prior ones as it sees fit.
    pub async fn extend(&self, id: MissionId, prompt: String) -> Result<(), MissionError> {
        let mut mission = self.missions.get(id).await?;
        if !mission.status.is_terminal() {
            return Err(MissionError::State(format!(
                "cannot extend mission in status {:?} — wait for it to reach a terminal state or cancel it first",
                mission.status
            )));
        }
        let separator = "\n\n---\n\n### Follow-up request\n";
        mission.description.push_str(separator);
        mission.description.push_str(prompt.trim());

        let from = mission.status.clone();
        mission.transition_to(MissionStatus::Draft)
            .map_err(|e| MissionError::State(e.to_string()))?;
        self.missions.update(&mission).await?;
        self.events.publish(ForgeEvent::MissionStatusChanged {
            id, from, to: MissionStatus::Draft,
        }).await?;

        // Re-enter the standard plan → run → reflect flow. plan_and_run
        // transitions Draft → Planning → Ready → Running on its own.
        self.plan_and_run(id).await
    }

    pub async fn approve_task(&self, task_id: TaskId) -> Result<(), MissionError> {
        self.execution.approve_task(task_id).await?;
        Ok(())
    }

    pub async fn list(&self) -> Result<Vec<MissionSummary>, MissionError> {
        let missions = self.missions.list().await?;
        let mut out = Vec::with_capacity(missions.len());
        for m in missions {
            let goals = self.goals.list_for_mission(m.id).await.unwrap_or_default();
            let completed = goals.iter().filter(|g| g.status == GoalStatus::Completed).count();
            out.push(MissionSummary {
                id: m.id,
                title: m.title.clone(),
                status: m.status.clone(),
                created_at: m.created_at,
                goal_count: goals.len(),
                completed_goal_count: completed,
            });
        }
        Ok(out)
    }

    pub async fn detail(&self, id: MissionId) -> Result<MissionDetail, MissionError> {
        let mission = self.missions.get(id).await?;
        let goals = self.goals.list_for_mission(id).await?;
        let mut tasks_by_goal = std::collections::HashMap::new();
        for g in &goals {
            let ts = self.tasks.list_for_goal(g.id).await?;
            tasks_by_goal.insert(g.id, ts);
        }
        Ok(MissionDetail { mission, goals, tasks_by_goal })
    }

    /// Return persisted reflections for a mission.
    pub async fn reflections(&self, id: MissionId) -> Result<Vec<forge_persistence::ReflectionRecord>, MissionError> {
        Ok(self.learning.reflections.list_for_mission(id).await?)
    }
}

fn truncate(s: &str, cap: usize) -> String {
    if s.len() <= cap { return s.to_string(); }
    let mut cut = cap;
    while cut > 0 && !s.is_char_boundary(cut) { cut -= 1; }
    format!("{}…", &s[..cut])
}

#[derive(Debug, serde::Serialize)]
pub struct MissionDetail {
    pub mission: Mission,
    pub goals: Vec<forge_domain::Goal>,
    pub tasks_by_goal: std::collections::HashMap<forge_domain::GoalId, Vec<forge_domain::Task>>,
}

// Suppress unused-warnings for types re-exported only for consumers.
#[allow(dead_code)]
fn _keep_goalid_taskid_visible(_: GoalId, _: TaskId) {}
