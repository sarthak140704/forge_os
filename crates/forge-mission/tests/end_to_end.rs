//! End-to-end integration test.
//!
//! Boots persistence + event bus + LLM (Mock) + tools + policy + planner +
//! execution + mission service against a scratch SQLite file. Fires a
//! mission and asserts that it goes through the full pipeline: planning →
//! goals created → tasks executed → mission completed. No external services.

use forge_domain::{GoalStatus, MissionStatus};
use forge_events::EventBus;
use forge_execution::{ExecutionDeps, ExecutionEngine};
use forge_llm::{mock::MockProvider, LlmRouter, RoutingStrategy};
use forge_mission::MissionService;
use forge_persistence::{
    connect, GoalRepository, MissionRepository, ReflectionRepository, SqliteEventStore,
    SqliteGoalRepository, SqliteMissionRepository, SqliteReflectionRepository,
    SqliteTaskRepository,
};
use forge_planner::Planner;
use forge_policy::PolicyEngine;
use forge_skills::SkillRegistry;
use forge_tools::{builtins, ToolRegistry};
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn end_to_end_mock_llm_writes_file() {
    let workspace = tempdir();
    let db_url = format!(
        "sqlite://{}/forge.sqlite?mode=rwc",
        workspace.display().to_string().replace('\\', "/")
    );
    let pool = connect(&db_url).await.expect("connect");
    let event_store = Arc::new(SqliteEventStore::new(pool.clone()));
    let events = EventBus::new(event_store, 128);

    let missions_repo: Arc<dyn MissionRepository> =
        Arc::new(SqliteMissionRepository::new(pool.clone()));
    let goals_repo: Arc<dyn GoalRepository> =
        Arc::new(SqliteGoalRepository::new(pool.clone()));
    let tasks_repo = Arc::new(SqliteTaskRepository::new(pool.clone()));

    let policy = Arc::new(PolicyEngine::empty());

    let mut reg = ToolRegistry::new();
    for t in builtins::all(workspace.clone()) {
        reg.register(t);
    }
    let tools = Arc::new(reg);

    // Deterministic mock plan: write a file, then read it back.
    let plan_json = serde_json::json!({
        "goals": [
            {
                "id": "g1",
                "title": "Write hello file",
                "description": "Create hello.txt",
                "depends_on": [],
                "tasks": [
                    { "tool": "fs.write", "input": { "path": "hello.txt", "content": "hi forge" } }
                ]
            },
            {
                "id": "g2",
                "title": "Read hello file",
                "description": "Verify the file",
                "depends_on": ["g1"],
                "tasks": [
                    { "tool": "fs.read", "input": { "path": "hello.txt" } }
                ]
            }
        ]
    })
    .to_string();
    let mock: Arc<dyn forge_llm::LlmProvider> =
        Arc::new(MockProvider::new("mock", vec![Ok(plan_json)]));
    let router = Arc::new(LlmRouter::new(vec![mock], RoutingStrategy::FailoverInOrder));

    let planner = Arc::new(Planner::new(router.clone(), "mock-model".to_string()));
    let exec_deps = ExecutionDeps {
        missions: missions_repo.clone(),
        goals: goals_repo.clone(),
        tasks: tasks_repo.clone(),
        events: events.clone(),
        policy,
        tools: tools.clone(),
        workspace: workspace.clone(),
        max_parallel_goals: 2,
        materializer: None,
    };
    let execution = ExecutionEngine::new(exec_deps);

    let svc = MissionService {
        missions: missions_repo.clone(),
        goals: goals_repo.clone(),
        tasks: tasks_repo.clone(),
        events: events.clone(),
        planner,
        execution: execution.clone(),
        tools,
        skills: Arc::new(SkillRegistry::new(Vec::new())),
        learning: forge_mission::LearningDeps {
            reflector: None,
            proposal_writer: None,
            reflections: Arc::new(SqliteReflectionRepository::new(pool.clone())) as Arc<dyn ReflectionRepository>,
        },
        project_memory: None,
        llm_router: None,
        episodic_recall: None,
        queue: None,
        org_memory: None,
        embedding_provider: None,
    };

    let mid = svc
        .create("greet".into(), "write and read a hello file".into())
        .await
        .expect("create");

    // Subscribe to events BEFORE kicking off so we can print them on failure.
    let mut event_rx = events.subscribe();
    let events_log = Arc::new(tokio::sync::Mutex::new(Vec::<forge_domain::EventEnvelope>::new()));
    let log_writer = events_log.clone();
    tokio::spawn(async move {
        while let Ok(env) = event_rx.recv().await {
            log_writer.lock().await.push(env);
        }
    });

    svc.plan_and_run(mid).await.expect("plan_and_run");

    // Poll for terminal state; the executor runs on a spawned task.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let last_status = loop {
        let m = missions_repo.get(mid).await.expect("get");
        if m.status.is_terminal() {
            break m.status;
        }
        if std::time::Instant::now() > deadline {
            panic!("mission did not reach terminal state in time; last = {:?}", m.status);
        }
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    };
    if last_status != MissionStatus::Completed {
        let log = events_log.lock().await;
        panic!(
            "expected Completed, got {:?}\n---- event log ({} events) ----\n{}",
            last_status,
            log.len(),
            log.iter()
                .map(|e| format!("{:?}", e))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    let goals = goals_repo.list_for_mission(mid).await.expect("goals");
    assert_eq!(goals.len(), 2);
    assert!(goals.iter().all(|g| g.status == GoalStatus::Completed));

    let file_path = workspace.join("hello.txt");
    let content = std::fs::read_to_string(&file_path).expect("file exists");
    assert_eq!(content, "hi forge");
}

fn tempdir() -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!("forge-os-test-{}", nanos_suffix()));
    std::fs::create_dir_all(&base).expect("create tempdir");
    base
}

fn nanos_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}")
}
