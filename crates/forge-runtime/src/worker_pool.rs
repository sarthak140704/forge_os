//! Phase 4d — Persisted mission-execution worker pool.
//!
//! When `RuntimeConfig.workers >= 1`, the runtime replaces the classic
//! fire-and-forget `tokio::spawn(plan_and_run)` pattern with a proper pool:
//!
//! 1. `MissionService::enqueue(id)` inserts a `Queued` row into
//!    `mission_queue`.
//! 2. N background workers loop over `claim_next` → `plan_and_run_sync` →
//!    `finish`. A heartbeat timer keeps `heartbeat_at` fresh while a
//!    mission is running.
//! 3. On boot, `Runtime::boot` runs `requeue_stale(worker_stale_secs)` so
//!    any mission that was claimed by a crashed prior process gets moved
//!    back to `Queued` for another worker to pick up.
//!
//! ## Why not just tokio::spawn?
//!
//! - **Crash recovery** — a fire-and-forget spawn dies with the process
//!   and the mission is silently lost. The queue survives the process.
//! - **Backpressure** — a runaway user enqueuing 100 missions won't blow
//!   the LLM budget in parallel; only `workers` run at once.
//! - **Observability** — the queue depth + per-worker heartbeat is a real
//!   signal for the UI to render.
//!
//! ## Why not full distributed execution?
//!
//! This is a personal desktop project. In-process workers give the crash
//! recovery + backpressure benefits without a network dependency. Real
//! distributed execution (Postgres-backed queue + leader election) is on
//! the Phase 5 roadmap.

use forge_domain::MissionId;
use forge_events::EventBus;
use forge_mission::MissionService;
use forge_persistence::MissionQueueRepository;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

pub struct WorkerPool {
    queue:            Arc<dyn MissionQueueRepository>,
    missions:         MissionService,
    events:           EventBus,
    worker_count:     usize,
    stale_after_secs: u64,
}

impl WorkerPool {
    pub fn new(
        queue: Arc<dyn MissionQueueRepository>,
        missions: MissionService,
        events: EventBus,
        worker_count: usize,
        stale_after_secs: u64,
    ) -> Self {
        Self {
            queue,
            missions,
            events,
            worker_count: worker_count.max(1),
            stale_after_secs,
        }
    }

    /// Spawn `worker_count` tokio tasks + one janitor task that
    /// periodically requeues stale claims (in case a worker panics
    /// mid-mission after enqueueing and before finish).
    pub fn spawn(self: Arc<Self>) {
        for i in 0..self.worker_count {
            let this = self.clone();
            let worker_id = format!("w{i}");
            tokio::spawn(async move { this.worker_loop(worker_id).await; });
        }
        // Janitor — runs at half the stale threshold, so a worker crash
        // is recovered within one full window on average.
        let janitor = self.clone();
        let jan_interval = Duration::from_secs((self.stale_after_secs / 2).max(15));
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(jan_interval).await;
                match janitor.queue.requeue_stale(janitor.stale_after_secs as i64).await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(count = n, "janitor requeued stale mission-queue rows"),
                    Err(e) => tracing::warn!(err = %e, "janitor requeue failed"),
                }
            }
        });
    }

    async fn worker_loop(self: Arc<Self>, worker_id: String) {
        tracing::info!(worker = %worker_id, "worker starting");
        loop {
            match self.queue.claim_next(&worker_id).await {
                Ok(Some(row)) => {
                    tracing::info!(worker = %worker_id, queue_id = row.id, mission = %row.mission_id, "worker claimed mission");
                    self.run_claimed(&worker_id, row.id, &row.mission_id).await;
                }
                Ok(None) => {
                    // Empty queue — back off briefly. Poll cadence is a
                    // trade-off: shorter = snappier startup, more CPU;
                    // longer = calmer, higher latency. 500ms feels right
                    // for a desktop app.
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                Err(e) => {
                    tracing::warn!(worker = %worker_id, err = %e, "claim_next failed; sleeping");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }

    async fn run_claimed(&self, worker_id: &str, queue_id: i64, mission_id_str: &str) {
        // Start a heartbeat task alongside the actual execution so a
        // slow (multi-minute) mission doesn't get requeued by the janitor.
        let queue = self.queue.clone();
        let hb_interval = Duration::from_secs((self.stale_after_secs / 3).max(10));
        let hb_handle = tokio::spawn(async move {
            loop {
                tokio::time::sleep(hb_interval).await;
                if let Err(e) = queue.heartbeat(queue_id).await {
                    tracing::warn!(queue_id, err = %e, "heartbeat failed");
                    break;
                }
            }
        });

        let outcome = match MissionId::from_str(mission_id_str) {
            Ok(mid) => match self.missions.plan_and_run_sync(mid).await {
                Ok(()) => Ok(()),
                Err(e) => Err(format!("plan_and_run_sync: {e}")),
            },
            Err(e) => Err(format!("bad mission_id `{mission_id_str}`: {e}")),
        };
        hb_handle.abort();

        let (success, err) = match outcome.as_ref() {
            Ok(()) => (true, None),
            Err(msg) => (false, Some(msg.as_str())),
        };
        if let Err(e) = self.queue.finish(queue_id, success, err).await {
            tracing::warn!(worker = %worker_id, queue_id, err = %e, "queue.finish failed");
        }
        if !success {
            tracing::error!(worker = %worker_id, queue_id, err = ?outcome.err(), "mission failed via worker");
        } else {
            tracing::info!(worker = %worker_id, queue_id, "mission finished cleanly");
        }
        let _ = self.events; // reserved for a future "WorkerBusyChanged" event; keep the field wired.
    }
}
