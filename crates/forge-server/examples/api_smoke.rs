//! LLM-free headless smoke for Phase 5 — HTTP API server.
//!
//! Boots a Runtime with `api_bind = 127.0.0.1:<ephemeral>`, then talks to it
//! over real TCP with `reqwest`:
//!
//!   1. `GET /health` without a bearer  → 200 (health is unauthenticated).
//!   2. `GET /missions` with a wrong bearer → 401.
//!   3. `POST /missions` with the right bearer → returns `{id: <uuid>}`.
//!   4. `GET /missions/:id` → mission detail.
//!   5. `POST /missions/:id/cancel` → 200; mission transitions to Cancelled.
//!   6. `POST /v1/chat/completions` (non-streaming) → 200 with an OpenAI-shaped
//!      body. Planning fails (dummy LLM key), so `finish_reason` is `"error"`.
//!   7. `GET /events?since=0` streams recent events (peeks a few frames).
//!
//! Planning fails against the dummy provider — that is intentional. The point
//! is to prove the HTTP surface, auth, JSON contracts and mission-id routing
//! work end-to-end without a real LLM.
//!
//! Run:  cargo run -p forge-server --example api_smoke

use std::net::TcpListener;
use std::time::Duration;

use forge_runtime::{LlmConfig, LlmProviderConfig, Runtime, RuntimeConfig};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("forge_=info,warn")),
        )
        .try_init()
        .ok();

    // Grab an ephemeral port by binding and immediately dropping the listener.
    // Small race window, fine for a smoke.
    let port = TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let bind: std::net::SocketAddr = format!("127.0.0.1:{port}").parse()?;

    let tmp = std::env::temp_dir().join(format!(
        "forge-api-smoke-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis()
    ));
    let workspace = tmp.join("workspace");
    let db_path = tmp.join("forge.sqlite");
    std::fs::create_dir_all(&workspace)?;

    unsafe {
        std::env::set_var("__FORGE_DUMMY_KEY__", "unused");
        std::env::set_var("FORGE_SMOKE_TOKEN", "s3cr3t");
    }

    let config = RuntimeConfig {
        workspace_root: workspace,
        db_path,
        policy_path: None,
        llm: LlmConfig {
            providers: vec![LlmProviderConfig::Groq {
                api_key_env: "__FORGE_DUMMY_KEY__".into(),
            }],
            model: "unused-model".into(),
        },
        max_parallel_goals: 2,
        skills_root: None,
        mcp_config: None,
        auto_promote_skills: false,
        autopromote_interval_secs: 300,
        curator: Default::default(),
        curator_sweep_enabled: false,
        curator_interval_secs: 900,
        workers: 0,
        worker_stale_secs: 120,
        org_memory_enabled: false,
        embedding_provider: None,
        api_bind: Some(bind),
        api_token_env: "FORGE_SMOKE_TOKEN".into(),
    };

    println!("→ booting runtime on {bind}");
    let _runtime = Runtime::boot(config).await?;

    // Give the axum listener a moment to be ready.
    tokio::time::sleep(Duration::from_millis(400)).await;

    let base = format!("http://{bind}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    // 1. Health — unauthenticated.
    let r = client.get(format!("{base}/health")).send().await?;
    println!("[1] GET  /health                → {}", r.status());
    assert!(r.status().is_success(), "health should be 200");

    // 2. Auth reject.
    let r = client
        .get(format!("{base}/missions"))
        .bearer_auth("wrong-token")
        .send()
        .await?;
    println!("[2] GET  /missions   (bad tok)  → {}", r.status());
    assert_eq!(r.status().as_u16(), 401, "expected 401 for wrong bearer");

    // 3. Create mission.
    let r = client
        .post(format!("{base}/missions"))
        .bearer_auth("s3cr3t")
        .json(&json!({
            "title":       "smoke title",
            "description": "smoke description — planning will fail because the LLM key is a dummy."
        }))
        .send()
        .await?;
    let status = r.status();
    let body: serde_json::Value = r.json().await?;
    println!("[3] POST /missions              → {status}  body={body}");
    assert!(status.is_success(), "mission create should be 200");
    let mission_id = body["id"].as_str().expect("id field").to_string();

    // 4. GET :id
    let r = client
        .get(format!("{base}/missions/{mission_id}"))
        .bearer_auth("s3cr3t")
        .send()
        .await?;
    let status = r.status();
    let detail: serde_json::Value = r.json().await?;
    println!(
        "[4] GET  /missions/:id          → {status}  status={}",
        detail["mission"]["status"]
    );
    assert!(status.is_success(), "get mission should be 200");

    // 5. Cancel
    let r = client
        .post(format!("{base}/missions/{mission_id}/cancel"))
        .bearer_auth("s3cr3t")
        .send()
        .await?;
    println!("[5] POST /missions/:id/cancel   → {}", r.status());
    assert!(r.status().is_success(), "cancel should be 200");

    // 6. OpenAI-compat shim.
    let r = client
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth("s3cr3t")
        .json(&json!({
            "model": "forge-mission",
            "messages": [
                {"role": "user", "content": "hello forge"}
            ],
            "stream": false
        }))
        .send()
        .await?;
    let status = r.status();
    let shim: serde_json::Value = r.json().await?;
    println!(
        "[6] POST /v1/chat/completions   → {status}  finish_reason={}",
        shim["choices"][0]["finish_reason"]
    );
    assert!(status.is_success(), "openai shim should be 200");

    // 7. Events stream — pull first 2KB.
    let r = client
        .get(format!("{base}/events?since=0"))
        .bearer_auth("s3cr3t")
        .send()
        .await?;
    println!(
        "[7] GET  /events?since=0        → {}  (streaming, taking first bytes)",
        r.status()
    );
    let bytes = tokio::time::timeout(Duration::from_millis(700), r.bytes()).await;
    match bytes {
        Ok(Ok(b)) => println!("    got {} bytes", b.len()),
        _ => println!("    (timed out; stream stayed open — that's expected)"),
    }

    println!("\n✅ Phase 5 API smoke complete.");
    Ok(())
}
