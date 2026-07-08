//! Execution engine.
//!
//! The engine walks a mission's goal DAG. It:
//!   1. Loads all goals + tasks for the mission from persistence.
//!   2. Runs goals whose dependencies are all `Completed`, up to a concurrency
//!      cap enforced by a semaphore.
//!   3. For each goal, runs its tasks sequentially. Each task:
//!        - passes through the policy engine → Allow / RequireApproval / Deny
//!        - if Allow → invoke tool
//!        - if RequireApproval → publish `PolicyApprovalRequested`, park until
//!          `approve_task` is called on the engine (or mission is cancelled)
//!        - if Deny → mark task and its goal as `Failed`
//!   4. On terminal goal state, re-computes the ready set.
//!   5. Emits a stream of `ForgeEvent`s at every state change.
//!
//! Cancellation is cooperative via `CancellationToken`.

use dashmap::DashMap;
use forge_domain::{
    ForgeEvent, Goal, GoalId, GoalStatus, MissionId, MissionStatus, PolicyDecision, Task, TaskId,
    TaskStatus,
};
use forge_events::EventBus;
use forge_persistence::{GoalRepository, MissionRepository, PersistenceError, TaskRepository};
use forge_policy::{EvalCtx, PolicyEngine};
use forge_tools::{ToolCtx, ToolRegistry};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{oneshot, Semaphore};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("persistence: {0}")]
    Persist(#[from] PersistenceError),
    #[error("event bus: {0}")]
    Events(#[from] forge_events::EventBusError),
    #[error("tool `{tool}` failed: {source}")]
    Tool { tool: String, #[source] source: forge_tools::ToolError },
    #[error("cancelled")]
    Cancelled,
}

/// A completed upstream task, feeding context into a downstream materialization.
#[derive(Clone, Debug)]
pub struct UpstreamResult {
    pub goal_title: String,
    pub tool: String,
    /// Truncated JSON summary of the result payload (already size-limited).
    pub result_summary: String,
}

/// Rewrites a goal's pending task inputs at execution time, given the
/// completed results of that goal's upstream dependencies. The initial
/// planner runs *before* upstream goals execute, so it can only emit
/// placeholder args ("[insert directories here]") for tasks that need to
/// consume upstream output. The materializer closes that gap.
///
/// Implementations must be side-effect free with respect to persistence and
/// events — the executor persists any changed inputs and emits
/// `TaskInputRefreshed` events itself. Returning the inputs unchanged is a
/// valid no-op.
#[async_trait::async_trait]
pub trait TaskInputMaterializer: Send + Sync {
    /// `tasks[i]` corresponds to the returned `Vec[i]`. Length MUST match.
    /// If the materializer cannot make a decision, it should return the
    /// original inputs verbatim.
    async fn materialize(
        &self,
        mission_id: MissionId,
        goal: &Goal,
        tasks: &[Task],
        upstream: &[UpstreamResult],
    ) -> Result<Vec<serde_json::Value>, MaterializeError>;
}

#[derive(Debug, Error)]
pub enum MaterializeError {
    #[error("materializer llm error: {0}")]
    Llm(String),
    #[error("materializer produced malformed output: {0}")]
    Malformed(String),
}

/// Injected dependencies. Cheap to clone (all Arcs).
#[derive(Clone)]
pub struct ExecutionDeps {
    pub missions:  Arc<dyn MissionRepository>,
    pub goals:     Arc<dyn GoalRepository>,
    pub tasks:     Arc<dyn TaskRepository>,
    pub events:    EventBus,
    pub policy:    Arc<PolicyEngine>,
    pub tools:     Arc<ToolRegistry>,
    pub workspace: PathBuf,
    pub max_parallel_goals: usize,
    /// Optional just-in-time task input rewriter. When set, and a goal has
    /// upstream completed dependencies, tasks whose inputs may reference
    /// upstream outputs get their `input` refreshed before execution. When
    /// `None`, tasks execute with plan-time inputs verbatim.
    #[allow(clippy::type_complexity)]
    pub materializer: Option<Arc<dyn TaskInputMaterializer>>,
}

/// Outstanding approval requests per task.
type ApprovalMap = Arc<DashMap<TaskId, oneshot::Sender<()>>>;

#[derive(Clone)]
pub struct ExecutionEngine {
    deps: ExecutionDeps,
    approvals: ApprovalMap,
    /// Cancellation tokens keyed by mission id.
    cancels: Arc<DashMap<MissionId, CancellationToken>>,
}

impl ExecutionEngine {
    pub fn new(deps: ExecutionDeps) -> Self {
        Self {
            deps,
            approvals: Arc::new(DashMap::new()),
            cancels: Arc::new(DashMap::new()),
        }
    }

    /// Approve a task currently parked in the `PendingApproval` state.
    /// No-op if the task isn't waiting.
    pub async fn approve_task(&self, task_id: TaskId) -> Result<(), ExecutionError> {
        if let Some((_, tx)) = self.approvals.remove(&task_id) {
            let _ = tx.send(());
            self.deps.events.publish(ForgeEvent::PolicyApprovalGranted { task_id }).await?;
        }
        Ok(())
    }

    /// Cancel a running mission.
    pub fn cancel(&self, mission_id: MissionId) {
        if let Some(tok) = self.cancels.get(&mission_id) {
            tok.cancel();
        }
    }

    /// Run all goals for a mission until quiescent (all terminal), then flip
    /// the mission status accordingly.
    pub async fn run_mission(&self, mission_id: MissionId) -> Result<(), ExecutionError> {
        let cancel = CancellationToken::new();
        self.cancels.insert(mission_id, cancel.clone());

        // Load fresh state from persistence — the engine is stateless between runs.
        let mut goals: Vec<Goal> = self.deps.goals.list_for_mission(mission_id).await?;
        let goal_index: HashMap<GoalId, usize> = goals.iter().enumerate().map(|(i, g)| (g.id, i)).collect();

        let sem = Arc::new(Semaphore::new(self.deps.max_parallel_goals.max(1)));
        let mut running: HashMap<GoalId, tokio::task::JoinHandle<Result<GoalId, ExecutionError>>> = HashMap::new();
        let mut completed: HashSet<GoalId> = goals.iter()
            .filter(|g| g.status == GoalStatus::Completed)
            .map(|g| g.id).collect();
        let mut failed: HashSet<GoalId> = goals.iter()
            .filter(|g| g.status == GoalStatus::Failed || g.status == GoalStatus::Skipped)
            .map(|g| g.id).collect();

        loop {
            if cancel.is_cancelled() {
                break;
            }
            // Any newly ready goals?
            for i in 0..goals.len() {
                let g = &goals[i];
                if g.status != GoalStatus::Pending && g.status != GoalStatus::Ready { continue; }
                if running.contains_key(&g.id) { continue; }
                // Are all deps satisfied?
                if g.depends_on.iter().any(|d| !completed.contains(d)) {
                    // If any dep failed, skip this goal.
                    if g.depends_on.iter().any(|d| failed.contains(d)) {
                        self.transition_goal(&mut goals[i], GoalStatus::Skipped).await?;
                        failed.insert(goals[i].id);
                    }
                    continue;
                }
                // ready → mark, launch
                self.transition_goal(&mut goals[i], GoalStatus::Ready).await?;
                let deps = self.deps.clone();
                let goal = goals[i].clone();
                let approvals = self.approvals.clone();
                let cancel = cancel.clone();
                let sem = sem.clone();
                let engine = self.clone();
                let handle = tokio::spawn(async move {
                    let _permit = sem.acquire_owned().await.map_err(|_| ExecutionError::Cancelled)?;
                    engine.run_goal(goal, approvals, cancel, &deps).await
                });
                running.insert(goals[i].id, handle);
            }

            // Nothing running and nothing left to launch → done.
            if running.is_empty() {
                break;
            }

            // Wait for any running goal to finish.
            let ids: Vec<GoalId> = running.keys().copied().collect();
            let mut finished_any = false;
            for gid in ids {
                // Check without blocking to keep scheduling reactive.
                let handle = running.get_mut(&gid).unwrap();
                if handle.is_finished() {
                    let handle = running.remove(&gid).unwrap();
                    let result = handle.await.unwrap_or_else(|e| Err(ExecutionError::Tool {
                        tool: "runtime".into(),
                        source: forge_tools::ToolError::Exec(format!("join: {e}")),
                    }));
                    let idx = goal_index[&gid];
                    match result {
                        Ok(_) => {
                            self.transition_goal(&mut goals[idx], GoalStatus::Completed).await?;
                            completed.insert(gid);
                        }
                        Err(e) => {
                            // Deterministic errors (InvalidInput, PolicyDenied)
                            // will fail identically on retry — bail immediately
                            // so the mission can either re-plan or fail fast
                            // instead of burning attempts on a known-bad call.
                            let deterministic = matches!(
                                &e,
                                ExecutionError::Tool { source: forge_tools::ToolError::InvalidInput { .. }, .. }
                                    | ExecutionError::Tool { source: forge_tools::ToolError::PolicyDenied(_), .. }
                            );
                            if !deterministic && goals[idx].retries_remaining > 0 {
                                goals[idx].retries_remaining -= 1;
                                self.deps.goals.update(&goals[idx]).await?;
                                self.transition_goal(&mut goals[idx], GoalStatus::Pending).await?;
                                tracing::warn!(goal = %gid, err = %e, retries_left = goals[idx].retries_remaining, "goal failed, will retry");
                            } else {
                                self.transition_goal(&mut goals[idx], GoalStatus::Failed).await?;
                                failed.insert(gid);
                                if deterministic {
                                    tracing::error!(goal = %gid, err = %e, "goal failed deterministically; skipping retries");
                                } else {
                                    tracing::error!(goal = %gid, err = %e, "goal terminally failed");
                                }
                            }
                        }
                    }
                    finished_any = true;
                }
            }
            if !finished_any {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }

        // Flip mission status.
        //
        // We deliberately only transition to Cancelled or Completed here.
        // Failure is left to the mission service so its replan loop still
        // has a chance to add corrective goals before we declare defeat.
        // (Mission stays in Running with failed goals until the service
        // gives up.)
        let mut mission = self.deps.missions.get(mission_id).await?;
        let next = if cancel.is_cancelled() {
            Some(MissionStatus::Cancelled)
        } else if failed.is_empty() && completed.len() == goals.len() {
            Some(MissionStatus::Completed)
        } else {
            None
        };
        if let Some(next) = next {
            if mission.status.can_transition(&next) {
                let from = mission.status.clone();
                mission.transition_to(next.clone()).ok();
                self.deps.missions.update(&mission).await?;
                self.deps.events.publish(ForgeEvent::MissionStatusChanged {
                    id: mission_id, from, to: next,
                }).await?;
            }
        }

        self.cancels.remove(&mission_id);
        Ok(())
    }

    async fn transition_goal(&self, goal: &mut Goal, next: GoalStatus) -> Result<(), ExecutionError> {
        if goal.status == next { return Ok(()); }
        let from = goal.status.clone();
        goal.status = next.clone();
        self.deps.goals.update(goal).await?;
        self.deps.events.publish(ForgeEvent::GoalStatusChanged {
            id: goal.id, from, to: next,
        }).await?;
        Ok(())
    }

    async fn run_goal(
        &self,
        goal: Goal,
        approvals: ApprovalMap,
        cancel: CancellationToken,
        deps: &ExecutionDeps,
    ) -> Result<GoalId, ExecutionError> {
        // Load tasks for this goal.
        let mut tasks: Vec<Task> = deps.tasks.list_for_goal(goal.id).await?;
        deps.events.publish(ForgeEvent::GoalStatusChanged {
            id: goal.id, from: GoalStatus::Ready, to: GoalStatus::Running,
        }).await?;

        // Just-in-time task input materialization. If the goal has upstream
        // dependencies with completed tasks, ask the materializer to rewrite
        // any placeholder args using those results as context. This closes
        // the plan-time / execute-time gap that leaves tasks holding tokens
        // like "[insert directories here]".
        if let Some(mat) = deps.materializer.as_ref() {
            if !goal.depends_on.is_empty()
                && tasks.iter().any(|t| matches!(t.status, TaskStatus::Pending))
            {
                let upstream = self.gather_upstream_results(&goal, deps).await?;
                if !upstream.is_empty() {
                    match mat.materialize(goal.mission_id, &goal, &tasks, &upstream).await {
                        Ok(new_inputs) if new_inputs.len() == tasks.len() => {
                            for (task, new_input) in tasks.iter_mut().zip(new_inputs.into_iter()) {
                                if task.status == TaskStatus::Pending && task.input != new_input {
                                    task.input = new_input;
                                    deps.tasks.update(task).await?;
                                    deps.events.publish(ForgeEvent::TaskInputRefreshed {
                                        task_id: task.id, tool: task.tool.clone(),
                                    }).await?;
                                }
                            }
                        }
                        Ok(_) => {
                            tracing::warn!(
                                goal = %goal.id,
                                "materializer returned wrong number of inputs; ignoring",
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                goal = %goal.id, err = %e,
                                "task input materialization failed; using plan-time inputs",
                            );
                        }
                    }
                }
            }
        }

        for task in tasks.iter_mut() {
            if cancel.is_cancelled() { return Err(ExecutionError::Cancelled); }
            if task.status == TaskStatus::Completed { continue; }

            // Policy check.
            let decision = deps.policy.evaluate(&EvalCtx {
                tool: &task.tool,
                input: &task.input,
                workspace_root: &deps.workspace,
            });
            match decision {
                PolicyDecision::Deny { rule, reason } => {
                    task.status = TaskStatus::Denied;
                    task.error = Some(format!("{rule}: {reason}"));
                    deps.tasks.update(task).await?;
                    deps.events.publish(ForgeEvent::PolicyDenied {
                        task_id: task.id, rule, reason,
                    }).await?;
                    return Err(ExecutionError::Tool {
                        tool: task.tool.clone(),
                        source: forge_tools::ToolError::PolicyDenied(task.tool.clone()),
                    });
                }
                PolicyDecision::RequireApproval { rule, reason } => {
                    task.status = TaskStatus::PendingApproval;
                    deps.tasks.update(task).await?;
                    deps.events.publish(ForgeEvent::PolicyApprovalRequested {
                        task_id: task.id, rule, reason,
                    }).await?;
                    let (tx, rx) = oneshot::channel();
                    approvals.insert(task.id, tx);
                    // Wait for either approval or cancellation.
                    tokio::select! {
                        _ = rx => {},
                        _ = cancel.cancelled() => {
                            approvals.remove(&task.id);
                            task.status = TaskStatus::Cancelled;
                            deps.tasks.update(task).await?;
                            return Err(ExecutionError::Cancelled);
                        }
                    }
                }
                PolicyDecision::Allow => {}
            }

            // Execute.
            task.status = TaskStatus::Running;
            task.attempts = task.attempts.saturating_add(1);
            deps.tasks.update(task).await?;
            deps.events.publish(ForgeEvent::TaskStarted { id: task.id }).await?;
            deps.events.publish(ForgeEvent::ToolInvoked {
                task_id: task.id, tool: task.tool.clone(),
            }).await?;

            let tool = match deps.tools.get(&task.tool) {
                Some(t) => t,
                None => {
                    let err = format!("unknown tool: {}", task.tool);
                    task.status = TaskStatus::Failed;
                    task.error = Some(err.clone());
                    deps.tasks.update(task).await?;
                    deps.events.publish(ForgeEvent::TaskFailed { id: task.id, error: err.clone() }).await?;
                    return Err(ExecutionError::Tool {
                        tool: task.tool.clone(),
                        source: forge_tools::ToolError::Exec(err),
                    });
                }
            };
            let ctx = ToolCtx { workspace_root: deps.workspace.clone() };
            match tool.invoke(&ctx, task.input.clone()).await {
                Ok(v) => {
                    let summary = summarize(&v);
                    task.status = TaskStatus::Completed;
                    task.result = Some(v);
                    deps.tasks.update(task).await?;
                    deps.events.publish(ForgeEvent::TaskCompleted {
                        id: task.id, result_summary: summary,
                    }).await?;
                }
                Err(e) => {
                    let msg = e.to_string();
                    task.status = TaskStatus::Failed;
                    task.error = Some(msg.clone());
                    deps.tasks.update(task).await?;
                    deps.events.publish(ForgeEvent::TaskFailed { id: task.id, error: msg }).await?;
                    return Err(ExecutionError::Tool { tool: task.tool.clone(), source: e });
                }
            }
        }

        Ok(goal.id)
    }

    /// Collect (goal_title, tool, result_summary) for every completed task on
    /// every direct upstream dependency of `goal`. Truncated to keep prompts
    /// small — individual result summaries are already size-capped by
    /// `summarize()` at emission time, and we take at most 6 upstream tasks
    /// total (most recent by upstream goal order).
    async fn gather_upstream_results(
        &self,
        goal: &Goal,
        deps: &ExecutionDeps,
    ) -> Result<Vec<UpstreamResult>, ExecutionError> {
        let mut out: Vec<UpstreamResult> = Vec::new();
        for dep_id in &goal.depends_on {
            let dep_goal = match deps.goals.get(*dep_id).await {
                Ok(g) => g,
                Err(_) => continue,
            };
            let dep_tasks = deps.tasks.list_for_goal(*dep_id).await.unwrap_or_default();
            for t in dep_tasks {
                if t.status == TaskStatus::Completed {
                    let summary = t.result
                        .as_ref()
                        .map(summarize)
                        .unwrap_or_default();
                    out.push(UpstreamResult {
                        goal_title: dep_goal.title.clone(),
                        tool: t.tool.clone(),
                        result_summary: summary,
                    });
                    if out.len() >= 6 { return Ok(out); }
                }
            }
        }
        Ok(out)
    }
}

fn summarize(v: &serde_json::Value) -> String {
    let s = serde_json::to_string(v).unwrap_or_default();
    if s.len() > 240 { format!("{}…({}b)", &s[..240], s.len()) } else { s }
}
