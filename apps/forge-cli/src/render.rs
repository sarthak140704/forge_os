//! Formatting helpers for human-friendly output.
//!
//! Colours are avoided intentionally — the CLI runs in CI and inside PowerShell
//! pipes; ANSI escapes cause more grief than they save. Where we want to draw
//! attention we use plain-text sigils (✓ ✗ …) that render everywhere.

use serde::Serialize;
use serde_json::Value;

pub fn json_line<T: Serialize>(v: &T) {
    match serde_json::to_string(v) {
        Ok(s)  => println!("{s}"),
        Err(e) => eprintln!("!! failed to serialize: {e}"),
    }
}

pub fn json_pretty<T: Serialize>(v: &T) {
    match serde_json::to_string_pretty(v) {
        Ok(s)  => println!("{s}"),
        Err(e) => eprintln!("!! failed to serialize: {e}"),
    }
}

pub fn kv(k: &str, v: &str) {
    println!("  {k:<14} {v}");
}

/// Very small ISO-ish formatter — strips fractional seconds and the `+00:00`
/// suffix so the output is scannable at a glance.
pub fn short_ts(ts: &str) -> String {
    let mut out = ts.to_string();
    if let Some(dot) = out.find('.') {
        // 2026-01-01T09:00:00.123456789Z → 2026-01-01T09:00:00
        if let Some(end) = out[dot..].find(|c: char| c == 'Z' || c == '+' || c == '-') {
            let cut = dot + end;
            out.truncate(cut);
        } else {
            out.truncate(dot);
        }
    } else if let Some(z) = out.find('Z') {
        out.truncate(z);
    }
    out
}

/// Given an internally-tagged event body (as raw JSON), produce a one-line
/// human summary. Falls back to the raw `type` key if we don't have a
/// specialised formatter.
pub fn summarize_event(env: &Value) -> String {
    let seq   = env.get("seq").and_then(|v| v.as_i64()).unwrap_or(-1);
    let ts    = env.get("ts").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let ev    = env.get("event").cloned().unwrap_or_default();
    let ty    = ev.get("type").and_then(|v| v.as_str()).unwrap_or("?").to_string();

    let detail: String = match ty.as_str() {
        "mission_created" => format!(
            "mission {} \"{}\"",
            short_id(&ev, "id"),
            ev.get("title").and_then(|v| v.as_str()).unwrap_or("")
        ),
        "mission_planning_started"  => format!("planning {}",  short_id(&ev, "id")),
        "mission_planning_completed" => format!(
            "planned {} · {} goals",
            short_id(&ev, "id"),
            ev.get("goal_count").and_then(|v| v.as_u64()).unwrap_or(0)
        ),
        "mission_planning_failed" => format!(
            "planning FAILED {} · {}",
            short_id(&ev, "id"),
            ev.get("error").and_then(|v| v.as_str()).unwrap_or("")
        ),
        "mission_status_changed" => format!(
            "mission {} · {} → {}",
            short_id(&ev, "id"),
            ev.get("from").and_then(|v| v.as_str()).unwrap_or("?"),
            ev.get("to").and_then(|v| v.as_str()).unwrap_or("?")
        ),
        "goal_created" => format!(
            "goal {} \"{}\"",
            short_id(&ev, "id"),
            ev.get("title").and_then(|v| v.as_str()).unwrap_or("")
        ),
        "task_created" => format!(
            "task {} tool={}",
            short_id(&ev, "id"),
            ev.get("tool").and_then(|v| v.as_str()).unwrap_or("")
        ),
        "task_completed" => format!(
            "task ✓ {} · {}",
            short_id(&ev, "id"),
            ev.get("result_summary").and_then(|v| v.as_str()).unwrap_or("")
        ),
        "task_failed" => format!(
            "task ✗ {} · {}",
            short_id(&ev, "id"),
            ev.get("error").and_then(|v| v.as_str()).unwrap_or("")
        ),
        "llm_requested" => format!(
            "llm → {} {}",
            ev.get("provider").and_then(|v| v.as_str()).unwrap_or("?"),
            ev.get("model").and_then(|v| v.as_str()).unwrap_or("?")
        ),
        "llm_responded" => format!(
            "llm ← {} tok(p={},c={}) {}ms",
            ev.get("provider").and_then(|v| v.as_str()).unwrap_or("?"),
            ev.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            ev.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            ev.get("latency_ms").and_then(|v| v.as_u64()).unwrap_or(0)
        ),
        "llm_failed" => format!(
            "llm ✗ {} · {}",
            ev.get("provider").and_then(|v| v.as_str()).unwrap_or("?"),
            ev.get("error").and_then(|v| v.as_str()).unwrap_or("")
        ),
        _ => ty.clone(),
    };

    format!("[#{seq} {}] {}: {}", short_ts(&ts), ty, detail)
}

fn short_id(v: &Value, key: &str) -> String {
    let s = v.get(key).and_then(|v| v.as_str()).unwrap_or("?");
    if s.len() >= 8 { s[..8].to_string() } else { s.to_string() }
}

pub fn mission_status_glyph(status: &str) -> &'static str {
    match status {
        "completed" => "✓",
        "failed"    => "✗",
        "cancelled" => "⨯",
        "running"   => "→",
        "planning"  => "…",
        "ready"     => "◦",
        "paused"    => "‖",
        _           => "·",
    }
}
