use crate::client::ApiClient;
use crate::render;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use forge_domain::MissionStatus;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Everything you can do to a mission from the CLI.
#[derive(Subcommand)]
pub enum Op {
    /// List mission summaries.
    List {
        /// Only show missions with this status (draft, planning, running, …).
        #[arg(long)]
        status: Option<String>,
        /// Cap the number of rows.
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Fetch the full detail of a mission.
    Get { id: String },
    /// Create a new mission.
    Create {
        title: String,
        #[arg(long)]
        description: Option<String>,
        /// Create only — do not spawn plan_and_run.
        #[arg(long)]
        plan_only: bool,
    },
    /// Cancel a mission.
    Cancel { id: String },
    /// Extend a terminal mission with a follow-up prompt.
    Extend { id: String, prompt: String },
    /// Same as `create` but adds `--wait` semantics — block until terminal.
    Run {
        title: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        wait: bool,
        /// Print live event summaries while waiting.
        #[arg(long)]
        stream: bool,
    },
}

pub async fn dispatch(client: &ApiClient, json: bool, op: Op) -> Result<()> {
    match op {
        Op::List { status, limit } => list(client, json, status, limit).await,
        Op::Get  { id }            => get(client, json, id).await,
        Op::Create { title, description, plan_only } => {
            create(client, json, title, description, plan_only).await
        }
        Op::Cancel { id }          => cancel(client, json, id).await,
        Op::Extend { id, prompt }  => extend(client, json, id, prompt).await,
        Op::Run { title, description, wait, stream } => {
            run_shortcut(client, json, title, description, wait, stream).await
        }
    }
}

/// `forge run "..."` — same handler `Missions Op::Run` uses.
pub async fn run_shortcut(
    client: &ApiClient,
    json: bool,
    title: String,
    description: Option<String>,
    wait: bool,
    stream: bool,
) -> Result<()> {
    let id = create_inner(client, &title, description, false).await?;
    if json && !wait {
        render::json_line(&IdReply { id: id.clone() });
    } else if !wait {
        println!("mission created: {id}");
    }
    if wait {
        wait_for_terminal(client, &id, stream, json).await?;
    }
    Ok(())
}

// ---------- individual ops ----------

#[derive(Deserialize, Serialize)]
struct IdReply { id: String }

#[derive(Serialize)]
struct CreateBody<'a> {
    title:       &'a str,
    description: &'a str,
    plan_only:   bool,
}

async fn list(client: &ApiClient, json: bool, status: Option<String>, limit: usize) -> Result<()> {
    let mut rows: Vec<forge_domain::MissionSummary> =
        client.get_json("/missions").await?;
    if let Some(want) = status {
        let want = want.to_lowercase();
        rows.retain(|r| format!("{:?}", r.status).to_lowercase() == want);
    }
    rows.truncate(limit);

    if json {
        render::json_line(&rows);
        return Ok(());
    }
    if rows.is_empty() {
        println!("(no missions)");
        return Ok(());
    }
    println!("{:<10} {:<12} {}   TITLE", "ID", "STATUS", "CREATED");
    for r in &rows {
        let s = format!("{:?}", r.status).to_lowercase();
        println!(
            "{:<10} {} {:<10} {:<22}   {}",
            short_uuid(&r.id.as_uuid().to_string()),
            render::mission_status_glyph(&s),
            s,
            render::short_ts(&r.created_at.to_string()),
            truncate(&r.title, 60),
        );
    }
    Ok(())
}

async fn get(client: &ApiClient, json: bool, id: String) -> Result<()> {
    let detail: serde_json::Value = client.get_json(&format!("/missions/{id}")).await?;
    if json {
        render::json_pretty(&detail);
    } else {
        let m = &detail["mission"];
        println!("Mission {}", m["id"].as_str().unwrap_or("?"));
        render::kv("title:",   m["title"].as_str().unwrap_or(""));
        render::kv("status:",  &format!("{:?}", m["status"]));
        render::kv("created:", &render::short_ts(m["created_at"].as_str().unwrap_or("")));
        let goals = detail["goals"].as_array().map(|a| a.len()).unwrap_or(0);
        render::kv("goals:",   &goals.to_string());
    }
    Ok(())
}

async fn create(
    client: &ApiClient,
    json: bool,
    title: String,
    description: Option<String>,
    plan_only: bool,
) -> Result<()> {
    let id = create_inner(client, &title, description, plan_only).await?;
    if json {
        render::json_line(&IdReply { id });
    } else {
        println!("mission created: {id}");
    }
    Ok(())
}

async fn create_inner(
    client: &ApiClient,
    title: &str,
    description: Option<String>,
    plan_only: bool,
) -> Result<String> {
    let body = CreateBody {
        title,
        description: description.as_deref().unwrap_or(title),
        plan_only,
    };
    let reply: IdReply = client.post_json("/missions", &body).await?;
    Ok(reply.id)
}

async fn cancel(client: &ApiClient, json: bool, id: String) -> Result<()> {
    client.post_empty::<serde_json::Value>(&format!("/missions/{id}/cancel"), &serde_json::json!({})).await?;
    if json {
        render::json_line(&serde_json::json!({ "cancelled": id }));
    } else {
        println!("cancelled: {id}");
    }
    Ok(())
}

#[derive(Serialize)]
struct ExtendBody<'a> { prompt: &'a str }

async fn extend(client: &ApiClient, json: bool, id: String, prompt: String) -> Result<()> {
    client
        .post_empty(
            &format!("/missions/{id}/extend"),
            &ExtendBody { prompt: &prompt },
        )
        .await?;
    if json {
        render::json_line(&serde_json::json!({ "extended": id }));
    } else {
        println!("extended: {id}");
    }
    Ok(())
}

// ---------- --wait ----------

const WAIT_POLL: Duration    = Duration::from_millis(750);
const WAIT_TIMEOUT: Duration = Duration::from_secs(600);

async fn wait_for_terminal(client: &ApiClient, id: &str, stream: bool, json: bool) -> Result<()> {
    let start = Instant::now();
    let mut last_status: Option<String> = None;

    // Optional live tail — fire-and-forget task that echoes summaries.
    let stream_task = if stream {
        let c    = client.clone();
        let mid  = id.to_string();
        Some(tokio::spawn(async move {
            let _ = crate::cmd::events::run(&c, false, 0, Some(mid), true).await;
        }))
    } else {
        None
    };

    loop {
        let detail: serde_json::Value = client
            .get_json(&format!("/missions/{id}"))
            .await
            .with_context(|| format!("polling mission {id}"))?;
        let s = detail["mission"]["status"].as_str().unwrap_or("?").to_string();

        if last_status.as_deref() != Some(&s) {
            if !json && !stream {
                println!("  status → {s}");
            }
            last_status = Some(s.clone());
        }

        if is_terminal(&s) {
            if let Some(h) = stream_task { h.abort(); }
            if json {
                render::json_pretty(&detail);
            } else {
                println!();
                println!("Mission {id} terminated: {s}");
                if let Some(goals) = detail["goals"].as_array() {
                    for g in goals {
                        println!("  • [{}] {}",
                            g["status"].as_str().unwrap_or("?"),
                            g["title"].as_str().unwrap_or(""));
                    }
                }
            }
            return Ok(());
        }

        if start.elapsed() > WAIT_TIMEOUT {
            if let Some(h) = stream_task { h.abort(); }
            bail!("timed out waiting for mission {id} (last status {s})");
        }
        tokio::time::sleep(WAIT_POLL).await;
    }
}

fn is_terminal(s: &str) -> bool {
    matches!(s, "completed" | "failed" | "cancelled")
        || {
            // Snake-case is the wire format; MissionStatus::Debug gives us
            // PascalCase — defensive branch for either.
            let ms: Result<MissionStatus, _> = serde_json::from_value(serde_json::json!(s));
            matches!(ms.ok(), Some(MissionStatus::Completed | MissionStatus::Failed | MissionStatus::Cancelled))
        }
}

// ---------- utility ----------

fn short_uuid(u: &str) -> String {
    if u.len() >= 8 { u[..8].to_string() } else { u.to_string() }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() }
    else {
        let mut end = max;
        while !s.is_char_boundary(end) && end > 0 { end -= 1; }
        format!("{}…", &s[..end])
    }
}
