//! Audit export — dump the full mission / event history to a portable JSON
//! bundle for compliance, offline analysis, or backup.
//!
//! Not compliant with any specific standard (SOC 2, HIPAA, …), but the shape
//! is deterministic + versioned so downstream converters can produce those
//! formats. Everything currently in the SQLite store is exported: missions,
//! goals, tasks, events, reflections.

use serde::Serialize;
use sqlx::{sqlite::SqlitePool, Column, Row};
use std::path::Path;

const SCHEMA_VERSION: u32 = 1;

#[derive(Serialize)]
pub struct AuditBundle {
    pub schema_version: u32,
    pub exported_at:    String,
    pub counts:         Counts,
    pub missions:       Vec<serde_json::Value>,
    pub goals:          Vec<serde_json::Value>,
    pub tasks:          Vec<serde_json::Value>,
    pub events:         Vec<serde_json::Value>,
    pub reflections:    Vec<serde_json::Value>,
}

#[derive(Serialize)]
pub struct Counts {
    pub missions:    usize,
    pub goals:       usize,
    pub tasks:       usize,
    pub events:      usize,
    pub reflections: usize,
}

/// Build the audit bundle by querying every table in the SQLite store.
///
/// The queries are deliberately minimal — we `SELECT *` and let the row
/// serializer decide the column layout so the export doesn't lag behind
/// schema evolution.
pub async fn build(pool: &SqlitePool) -> Result<AuditBundle, String> {
    let missions = query_all(pool, "missions").await?;
    let goals = query_all(pool, "goals").await?;
    let tasks = query_all(pool, "tasks").await?;
    let events = query_all(pool, "events").await?;
    let reflections = query_all(pool, "reflections").await?;
    let counts = Counts {
        missions:    missions.len(),
        goals:       goals.len(),
        tasks:       tasks.len(),
        events:      events.len(),
        reflections: reflections.len(),
    };
    Ok(AuditBundle {
        schema_version: SCHEMA_VERSION,
        exported_at: chrono_now_iso(),
        counts,
        missions, goals, tasks, events, reflections,
    })
}

/// Serialize and write the bundle to `dest` as pretty-printed JSON.
pub async fn write_to(pool: &SqlitePool, dest: &Path) -> Result<Counts, String> {
    let bundle = build(pool).await?;
    let counts = Counts { ..bundle.counts };
    let json = serde_json::to_string_pretty(&bundle).map_err(|e| format!("serialize: {e}"))?;
    tokio::fs::write(dest, json).await.map_err(|e| format!("write: {e}"))?;
    Ok(counts)
}

impl Clone for Counts {
    fn clone(&self) -> Self {
        Self {
            missions: self.missions, goals: self.goals, tasks: self.tasks,
            events: self.events, reflections: self.reflections,
        }
    }
}

async fn query_all(pool: &SqlitePool, table: &str) -> Result<Vec<serde_json::Value>, String> {
    // `sqlx::query` doesn't support unbounded table names via `bind` — so we
    // build the SQL directly. Table names come from this file's constants,
    // not user input, so injection isn't a concern.
    let sql = format!("SELECT * FROM {table}");
    let rows = sqlx::query(&sql).fetch_all(pool).await
        .map_err(|e| format!("query {table}: {e}"))?;
    let mut out: Vec<serde_json::Value> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut obj = serde_json::Map::new();
        for (idx, col) in row.columns().iter().enumerate() {
            let name = col.name().to_string();
            // Best-effort typed fetch. SQLite is dynamically typed so we
            // try i64, f64, then String, then Vec<u8>, then treat as null.
            let value: serde_json::Value =
                if let Ok(v) = row.try_get::<i64, _>(idx) { serde_json::Value::from(v) }
                else if let Ok(v) = row.try_get::<f64, _>(idx) { serde_json::json!(v) }
                else if let Ok(v) = row.try_get::<String, _>(idx) { serde_json::Value::from(v) }
                else if let Ok(v) = row.try_get::<Vec<u8>, _>(idx) {
                    match String::from_utf8(v.clone()) {
                        Ok(s) => serde_json::Value::from(s),
                        Err(_) => serde_json::Value::from(base16(&v)),
                    }
                }
                else { serde_json::Value::Null };
            obj.insert(name, value);
        }
        out.push(serde_json::Value::Object(obj));
    }
    Ok(out)
}

fn chrono_now_iso() -> String {
    // Avoid pulling in chrono for this — use `time` (already in workspace).
    use ::time::OffsetDateTime;
    OffsetDateTime::now_utc()
        .format(&::time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| String::from(""))
}

fn base16(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
