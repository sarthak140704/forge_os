//! Mission facade — the API surface the runtime exposes to the outside world
//! (Tauri IPC, tests, an eventual HTTP server).

use forge_domain::{ForgeEvent, Goal, GoalId, GoalStatus, Mission, MissionId, MissionStatus, MissionSummary, Task, TaskId, TaskStatus};
use forge_events::EventBus;
use forge_execution::ExecutionEngine;
use forge_llm::{EmbeddingProvider, LlmRouter};
use forge_persistence::{
    GoalRepository, MissionQueueRepository, MissionRepository, NewOrgMemory,
    OrgMemoryRepository, ReflectionRepository, TaskRepository,
};
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

    /// Phase 4d — persisted mission execution queue. When set,
    /// `enqueue()` is available; the worker pool in `forge-runtime`
    /// pulls from it and calls `plan_and_run_sync`. IPC callers that
    /// prefer fire-and-forget still use `plan_and_run` which spawns a
    /// background task — set to `None` to disable queueing entirely.
    #[allow(clippy::type_complexity)]
    pub queue:      Option<Arc<dyn MissionQueueRepository>>,

    /// Phase 4f — organizational memory. When set, the reflector's
    /// insights are persisted as memory rows keyed on mission title
    /// keywords, and future missions' planners recall matching rows.
    #[allow(clippy::type_complexity)]
    pub org_memory: Option<Arc<dyn OrgMemoryRepository>>,

    /// Phase 6a — embedding provider for semantic recall over org_memory.
    /// When set, `fetch_org_memory_block` uses it to rank rows by
    /// cosine similarity instead of (well: in addition to) keyword LIKE
    /// search, and freshly-written memories get their embedding
    /// backfilled in a spawned task so recall degrades gracefully.
    /// `None` keeps the old keyword-only behaviour.
    #[allow(clippy::type_complexity)]
    pub embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
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
        let (selected_owned, ready_to_run) = self.plan_only(id).await?;
        if !ready_to_run {
            return Ok(());
        }
        // Kick off the replan loop + reflection in the background so IPC returns fast.
        let this = self.clone();
        tokio::spawn(async move {
            this.execute_and_reflect(id, selected_owned).await;
        });
        Ok(())
    }

    /// Blocking variant of `plan_and_run` — used by the WorkerPool
    /// (Phase 4d) so the worker's queue row stays `Claimed` until the
    /// mission is actually terminal. `plan_and_run` (async) returns as
    /// soon as planning is done, which is the wrong shape for a worker.
    pub async fn plan_and_run_sync(&self, id: MissionId) -> Result<(), MissionError> {
        let (selected_owned, ready_to_run) = self.plan_only(id).await?;
        if !ready_to_run {
            return Ok(());
        }
        self.execute_and_reflect(id, selected_owned).await;
        Ok(())
    }

    /// Insert a row into the persisted mission queue. Idempotent per
    /// mission (see `MissionQueueRepository::enqueue`). Errors if no
    /// queue is wired.
    pub async fn enqueue(&self, id: MissionId) -> Result<i64, MissionError> {
        let Some(q) = self.queue.as_ref() else {
            return Err(MissionError::State("mission queue not configured".into()));
        };
        let row_id = q.enqueue(id).await?;
        self.events.publish(ForgeEvent::MissionQueued { mission_id: id, queue_id: row_id }).await?;
        Ok(row_id)
    }

    /// Steps 1-3 of the old plan_and_run: transition to Planning,
    /// select skills, run initial plan, persist goals/tasks, transition
    /// to Ready → Running. Returns the selected skills and a flag
    /// indicating whether execution should proceed. When the mission
    /// can't transition to Planning (already terminal) returns
    /// `(vec![], false)` — the caller should short-circuit.
    async fn plan_only(&self, id: MissionId) -> Result<(Vec<forge_skills::Skill>, bool), MissionError> {
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
        // Phase 4f — org memory recall (best-effort).
        let org_mem_block: Option<String> = self.fetch_org_memory_block(&mission).await;
        if let Some(block) = org_mem_block.as_ref() {
            let preview: String = block.chars().take(240).collect();
            let _ = self.events.publish(ForgeEvent::OrgMemoryRecalled {
                mission_id: id,
                block_preview: preview,
            }).await;
        }
        let recall_block = recall.as_ref().map(|r| r.block.as_str());
        // Merge (org_memory ⨟ episodic ⨟ project) — all optional, joined with dividers.
        let combined_memory: Option<String> = {
            let mut parts: Vec<&str> = Vec::new();
            if let Some(m) = org_mem_block.as_deref() { parts.push(m); }
            if let Some(m) = recall_block            { parts.push(m); }
            if let Some(m) = self.project_memory.as_deref() { parts.push(m); }
            if parts.is_empty() { None } else { Some(parts.join("\n\n---\n\n")) }
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

        Ok((selected.into_iter().cloned().collect(), true))
    }

    /// Execute + replan + reflect + cost summary. Returns when the
    /// mission reaches a terminal state. Shared between the async
    /// `plan_and_run` (which wraps this in tokio::spawn) and
    /// `plan_and_run_sync` (which awaits it directly).
    async fn execute_and_reflect(&self, id: MissionId, selected_owned: Vec<forge_skills::Skill>) {
        if let Err(e) = self.run_with_replan_loop(id, selected_owned).await {
            tracing::error!(mission_id = %id, err = %e, "mission loop failed");
        }
        if let Some(router) = self.llm_router.as_ref() {
            if let Some(cost) = router.drain_mission_cost(&id.to_string()) {
                let _ = self.events.publish(ForgeEvent::MissionCostSummary {
                    mission_id: id,
                    llm_calls:  cost.calls,
                    prompt_tokens:     cost.prompt_tokens,
                    completion_tokens: cost.completion_tokens,
                    total_latency_ms:  cost.total_latency_ms,
                }).await;
            }
        }
        if let Err(e) = self.reflect_and_learn(id).await {
            tracing::warn!(mission_id = %id, err = %e, "reflection pass failed; continuing");
        }
    }

    /// Phase 4f + 6a — pull top-K matching memory rows for this mission's
    /// title + description as a planner block. Prefers semantic recall
    /// (cosine over embeddings) when an embedding provider is wired;
    /// falls back to keyword LIKE search when embeddings are unavailable
    /// or return no matches. Returns None when nothing at all was found.
    async fn fetch_org_memory_block(&self, mission: &Mission) -> Option<String> {
        let repo = self.org_memory.as_ref()?;
        let text = format!("{} {}", mission.title, mission.description);

        // ── 6a: try semantic first ──────────────────────────────────────
        let mut rows: Vec<(f32, forge_persistence::OrgMemoryRow)> = Vec::new();
        let mut used_semantic = false;
        if let Some(embedder) = self.embedding_provider.as_ref() {
            match embedder.embed(&text).await {
                Ok(qvec) => {
                    used_semantic = true;
                    match repo.semantic_search(&qvec, 5).await {
                        Ok(hits) => rows = hits.into_iter()
                            // Skip cosine below 0.20 — very likely noise.
                            .filter(|(s, _)| *s >= 0.20)
                            .collect(),
                        Err(e) => tracing::warn!(err = %e, "semantic_search failed; falling back"),
                    }
                }
                Err(e) => tracing::warn!(err = %e, "embedding for recall failed; falling back to keyword"),
            }
        }

        // ── 4f: keyword fallback (also runs when semantic yielded nothing) ─
        if rows.is_empty() {
            let keywords = keyword_extract(&text, 8);
            if keywords.is_empty() { return None; }
            match repo.search(&keywords, 5).await.ok() {
                Some(kw_rows) if !kw_rows.is_empty() => {
                    rows = kw_rows.into_iter().map(|r| (0.0_f32, r)).collect();
                }
                _ => return None,
            }
        }
        if rows.is_empty() { return None; }

        let header = if used_semantic {
            "## Prior learnings (semantic recall)\n"
        } else {
            "## Prior learnings\n"
        };
        let mut out = String::from(header);
        for (score, r) in &rows {
            if used_semantic {
                out.push_str(&format!("- **{}** _(sim {:.2})_ — {}\n", r.key, score, truncate(&r.value, 200)));
            } else {
                out.push_str(&format!("- **{}** — {}\n", r.key, truncate(&r.value, 200)));
            }
        }
        Some(out)
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

        // Phase 4f — persist reflection insights as durable org memory.
        // Skipped when memory isn't wired. Zero extra LLM cost: we reuse
        // the reflector's already-computed `insights` list. Tag with the
        // mission's own title keywords + selected skill names so future
        // planners can surface these rows via keyword recall.
        let mut memory_written = 0usize;
        if let Some(mem) = self.org_memory.as_ref() {
            let selected_skill_names: Vec<String> = self
                .skills.names().into_iter().collect();
            let title_keywords = keyword_extract(&format!("{} {}", mission.title, mission.description), 6);
            let mut tags = title_keywords;
            for n in selected_skill_names.iter().take(3) {
                if !tags.iter().any(|t| t == n) { tags.push(n.clone()); }
            }
            for insight in &reflection.insights {
                let trimmed = insight.trim();
                if trimmed.len() < 12 { continue; } // reject noise like "ok"
                let key = keyify(trimmed, 60);
                let new = NewOrgMemory {
                    key:               key.clone(),
                    value:             truncate(trimmed, 500).to_string(),
                    tags:              tags.clone(),
                    source_mission_id: Some(id),
                    embedding:         None, // Phase 6a: filled lazily by embedder task
                };
                match mem.insert(&new).await {
                    Ok(row_id) => {
                        memory_written += 1;
                        // Phase 6a — best-effort backfill: embed the
                        // memory value in a spawned task so future
                        // missions can recall it semantically. Failures
                        // are logged, never fatal; the row still exists
                        // and is reachable via keyword search.
                        if let Some(embedder) = self.embedding_provider.clone() {
                            let repo = mem.clone();
                            let val = new.value.clone();
                            tokio::spawn(async move {
                                match embedder.embed(&val).await {
                                    Ok(vec) => {
                                        if let Err(e) = repo.set_embedding(row_id, &vec).await {
                                            tracing::warn!(row_id, err = %e, "set_embedding failed");
                                        }
                                    }
                                    Err(e) => tracing::warn!(row_id, err = %e, "embed for memory row failed"),
                                }
                            });
                        }
                        let _ = self.events.publish(ForgeEvent::OrgMemoryLearned {
                            mission_id: id, memory_id: row_id, key: key.clone(),
                        }).await;
                    }
                    Err(e) => tracing::warn!(mission_id = %id, err = %e, "failed to persist org memory row"),
                }
            }
            tracing::info!(mission_id = %id, memory_written, "org memory extraction done");
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

/// Extract lowercase alphanumeric tokens of length ≥3 from `text`. Sorted
/// by first appearance, deduped, capped at `limit`. A very small stop-word
/// list is dropped to reduce false-positive keyword matches in org memory
/// search — kept intentionally short so we don't lose useful terms like
/// "test", "file", "http" that are keywords in a lot of missions.
pub(crate) fn keyword_extract(text: &str, limit: usize) -> Vec<String> {
    const STOP: &[&str] = &[
        "the","and","for","that","this","with","from","into","have","are","was",
        "will","use","using","how","what","when","where","which","should",
    ];
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::<String>::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        if raw.len() < 3 { continue; }
        let lower = raw.to_lowercase();
        if STOP.contains(&lower.as_str()) { continue; }
        if seen.insert(lower.clone()) {
            out.push(lower);
            if out.len() >= limit { break; }
        }
    }
    out
}

/// Turn a free-text insight into a snake_case memory key ≤ `max_len` chars.
/// Deterministic, so re-extracting the same insight collides (harmless — the
/// value column still stores the full text and older rows remain).
pub(crate) fn keyify(text: &str, max_len: usize) -> String {
    let mut s = String::with_capacity(max_len);
    let mut last_was_underscore = true;
    for c in text.chars() {
        if s.len() >= max_len { break; }
        if c.is_alphanumeric() {
            for lc in c.to_lowercase() { s.push(lc); }
            last_was_underscore = false;
        } else if !last_was_underscore {
            s.push('_');
            last_was_underscore = true;
        }
    }
    let out = s.trim_matches('_').to_string();
    if out.is_empty() { "memory".to_string() } else { out }
}

#[cfg(test)]
mod helper_tests {
    use super::*;

    #[test]
    fn keyword_extract_drops_short_and_stopwords() {
        let kws = keyword_extract("Use Python to run the pytest suite", 10);
        assert!(kws.contains(&"python".to_string()));
        assert!(kws.contains(&"pytest".to_string()));
        assert!(kws.contains(&"suite".to_string()));
        assert!(!kws.contains(&"the".to_string()), "should drop 'the'");
        assert!(!kws.contains(&"to".to_string()),  "should drop <3 char");
    }

    #[test]
    fn keyword_extract_dedups_preserving_order() {
        let kws = keyword_extract("Python python Python testing", 10);
        assert_eq!(kws, vec!["python".to_string(), "testing".to_string()]);
    }

    #[test]
    fn keyify_produces_snake_case() {
        assert_eq!(keyify("Prefer `pytest -q` in the project root!", 40), "prefer_pytest_q_in_the_project_root");
        assert_eq!(keyify("::::   ::::", 20), "memory");
        assert!(keyify(&"x".repeat(200), 20).len() <= 20);
    }
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
