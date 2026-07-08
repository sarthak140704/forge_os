//! SQLite implementation of the persistence traits.

use crate::{
    migrations, EventStore, GoalRepository, MissionRepository, PersistenceError, TaskRepository,
};
use async_trait::async_trait;
use forge_domain::{
    EventEnvelope, EventId, ForgeEvent, Goal, GoalId, GoalStatus, Mission, MissionId,
    MissionStatus, Task, TaskId, TaskStatus,
};
use sqlx::{sqlite::SqlitePoolOptions, Row};
use std::str::FromStr;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub type SqlitePool = sqlx::SqlitePool;

/// Open (or create) the SQLite DB at the given URL and apply migrations.
pub async fn connect(url: &str) -> Result<SqlitePool, PersistenceError> {
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect(url)
        .await?;
    // Enable WAL + foreign keys — safe defaults for a desktop app.
    sqlx::query("PRAGMA journal_mode = WAL;").execute(&pool).await?;
    sqlx::query("PRAGMA foreign_keys = ON;").execute(&pool).await?;
    // Apply init migration.
    for stmt in migrations::V001_INIT.split(';') {
        let s = stmt.trim();
        if s.is_empty() { continue; }
        sqlx::query(s).execute(&pool).await?;
    }
    Ok(pool)
}

fn ts_to_str(ts: OffsetDateTime) -> String {
    ts.format(&Rfc3339).unwrap_or_else(|_| String::new())
}
fn str_to_ts(s: &str) -> OffsetDateTime {
    OffsetDateTime::parse(s, &Rfc3339).unwrap_or_else(|_| OffsetDateTime::now_utc())
}

fn mission_status_str(s: &MissionStatus) -> &'static str {
    match s {
        MissionStatus::Draft => "draft",
        MissionStatus::Planning => "planning",
        MissionStatus::Ready => "ready",
        MissionStatus::Running => "running",
        MissionStatus::Paused => "paused",
        MissionStatus::Completed => "completed",
        MissionStatus::Failed => "failed",
        MissionStatus::Cancelled => "cancelled",
    }
}
fn parse_mission_status(s: &str) -> MissionStatus {
    match s {
        "planning" => MissionStatus::Planning,
        "ready" => MissionStatus::Ready,
        "running" => MissionStatus::Running,
        "paused" => MissionStatus::Paused,
        "completed" => MissionStatus::Completed,
        "failed" => MissionStatus::Failed,
        "cancelled" => MissionStatus::Cancelled,
        _ => MissionStatus::Draft,
    }
}

fn goal_status_str(s: &GoalStatus) -> &'static str {
    match s {
        GoalStatus::Pending => "pending",
        GoalStatus::Ready => "ready",
        GoalStatus::Running => "running",
        GoalStatus::Completed => "completed",
        GoalStatus::Failed => "failed",
        GoalStatus::Skipped => "skipped",
    }
}
fn parse_goal_status(s: &str) -> GoalStatus {
    match s {
        "ready" => GoalStatus::Ready,
        "running" => GoalStatus::Running,
        "completed" => GoalStatus::Completed,
        "failed" => GoalStatus::Failed,
        "skipped" => GoalStatus::Skipped,
        _ => GoalStatus::Pending,
    }
}

fn task_status_str(s: &TaskStatus) -> &'static str {
    match s {
        TaskStatus::Pending => "pending",
        TaskStatus::PendingApproval => "pending_approval",
        TaskStatus::Running => "running",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
        TaskStatus::Denied => "denied",
    }
}
fn parse_task_status(s: &str) -> TaskStatus {
    match s {
        "pending_approval" => TaskStatus::PendingApproval,
        "running" => TaskStatus::Running,
        "completed" => TaskStatus::Completed,
        "failed" => TaskStatus::Failed,
        "cancelled" => TaskStatus::Cancelled,
        "denied" => TaskStatus::Denied,
        _ => TaskStatus::Pending,
    }
}

// ---------------------------------------------------------------------------
// Event store
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct SqliteEventStore {
    pool: SqlitePool,
}
impl SqliteEventStore {
    pub fn new(pool: SqlitePool) -> Self { Self { pool } }
}

#[async_trait]
impl EventStore for SqliteEventStore {
    async fn append(&self, event: &ForgeEvent, ts: OffsetDateTime) -> Result<EventId, PersistenceError> {
        let payload = serde_json::to_string(event)?;
        let rec = sqlx::query(
            r#"INSERT INTO events (aggregate_id, aggregate_type, event_type, payload, created_at)
               VALUES (?1, ?2, ?3, ?4, ?5)
               RETURNING seq"#,
        )
        .bind(event.aggregate_id())
        .bind(event.kind().as_str())
        .bind(event.event_type())
        .bind(payload)
        .bind(ts_to_str(ts))
        .fetch_one(&self.pool)
        .await?;
        let seq: i64 = rec.try_get("seq")?;
        Ok(EventId(seq))
    }

    async fn read_since(&self, since: Option<EventId>) -> Result<Vec<EventEnvelope>, PersistenceError> {
        let cutoff = since.map(|e| e.0).unwrap_or(0);
        let rows = sqlx::query(
            "SELECT seq, payload, created_at FROM events WHERE seq > ?1 ORDER BY seq ASC LIMIT 5000",
        )
        .bind(cutoff)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let seq: i64 = row.try_get("seq")?;
            let payload: String = row.try_get("payload")?;
            let created: String = row.try_get("created_at")?;
            let event: ForgeEvent = serde_json::from_str(&payload)?;
            out.push(EventEnvelope { seq: EventId(seq), ts: str_to_ts(&created), event });
        }
        Ok(out)
    }

    async fn read_for_aggregate(&self, aggregate_id: &str) -> Result<Vec<EventEnvelope>, PersistenceError> {
        let rows = sqlx::query(
            "SELECT seq, payload, created_at FROM events WHERE aggregate_id = ?1 ORDER BY seq ASC",
        )
        .bind(aggregate_id)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let seq: i64 = row.try_get("seq")?;
            let payload: String = row.try_get("payload")?;
            let created: String = row.try_get("created_at")?;
            let event: ForgeEvent = serde_json::from_str(&payload)?;
            out.push(EventEnvelope { seq: EventId(seq), ts: str_to_ts(&created), event });
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Mission repository
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct SqliteMissionRepository { pool: SqlitePool }
impl SqliteMissionRepository { pub fn new(pool: SqlitePool) -> Self { Self { pool } } }

#[async_trait]
impl MissionRepository for SqliteMissionRepository {
    async fn insert(&self, m: &Mission) -> Result<(), PersistenceError> {
        sqlx::query(
            "INSERT INTO missions (id, title, description, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(m.id.to_string())
        .bind(&m.title)
        .bind(&m.description)
        .bind(mission_status_str(&m.status))
        .bind(ts_to_str(m.created_at))
        .bind(ts_to_str(m.updated_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update(&self, m: &Mission) -> Result<(), PersistenceError> {
        sqlx::query(
            "UPDATE missions SET title=?2, description=?3, status=?4, updated_at=?5 WHERE id=?1",
        )
        .bind(m.id.to_string())
        .bind(&m.title)
        .bind(&m.description)
        .bind(mission_status_str(&m.status))
        .bind(ts_to_str(m.updated_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get(&self, id: MissionId) -> Result<Mission, PersistenceError> {
        let row = sqlx::query(
            "SELECT id, title, description, status, created_at, updated_at FROM missions WHERE id=?1",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| PersistenceError::NotFound { kind: "mission", id: id.to_string() })?;
        Ok(Mission {
            id,
            title: row.try_get("title")?,
            description: row.try_get("description")?,
            status: parse_mission_status(&row.try_get::<String, _>("status")?),
            created_at: str_to_ts(&row.try_get::<String, _>("created_at")?),
            updated_at: str_to_ts(&row.try_get::<String, _>("updated_at")?),
            goals: Vec::new(),
        })
    }

    async fn list(&self) -> Result<Vec<Mission>, PersistenceError> {
        let rows = sqlx::query(
            "SELECT id, title, description, status, created_at, updated_at FROM missions ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let id_str: String = row.try_get("id")?;
            let id = MissionId::from_str(&id_str)
                .map_err(|e| PersistenceError::Sql(sqlx::Error::Decode(Box::new(e))))?;
            out.push(Mission {
                id,
                title: row.try_get("title")?,
                description: row.try_get("description")?,
                status: parse_mission_status(&row.try_get::<String, _>("status")?),
                created_at: str_to_ts(&row.try_get::<String, _>("created_at")?),
                updated_at: str_to_ts(&row.try_get::<String, _>("updated_at")?),
                goals: Vec::new(),
            });
        }
        Ok(out)
    }

    async fn search_similar(
        &self,
        keywords: &[String],
        limit: usize,
    ) -> Result<Vec<Mission>, PersistenceError> {
        if keywords.is_empty() || limit == 0 { return Ok(Vec::new()); }

        // Build a dynamic score expression:
        //   score = SUM(CASE WHEN lower(title||' '||description) LIKE '%kw%' THEN 1 ELSE 0 END)
        // Only keep terminal missions with score > 0.
        let mut score_parts: Vec<String> = Vec::with_capacity(keywords.len());
        for _ in 0..keywords.len() {
            score_parts.push(
                "CASE WHEN (lower(title) LIKE ?) OR (lower(description) LIKE ?) THEN 1 ELSE 0 END".into(),
            );
        }
        let sql = format!(
            "SELECT id, title, description, status, created_at, updated_at,
                    ({score}) AS match_score
             FROM missions
             WHERE status IN ('completed','failed','cancelled')
               AND ({any_match})
             ORDER BY match_score DESC, created_at DESC
             LIMIT ?",
            score = score_parts.join(" + "),
            any_match = score_parts.iter()
                .map(|p| format!("({})", p))
                .collect::<Vec<_>>()
                .join(" OR "),
        );

        let mut q = sqlx::query(&sql);
        // Bind twice per keyword for the score, then twice per keyword again
        // for the WHERE-clause guard (same pattern, sqlx wants distinct binds).
        for kw in keywords {
            let pat = format!("%{}%", kw.to_lowercase());
            q = q.bind(pat.clone()).bind(pat);
        }
        for kw in keywords {
            let pat = format!("%{}%", kw.to_lowercase());
            q = q.bind(pat.clone()).bind(pat);
        }
        q = q.bind(limit as i64);

        let rows = q.fetch_all(&self.pool).await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let id_str: String = row.try_get("id")?;
            let id = MissionId::from_str(&id_str)
                .map_err(|e| PersistenceError::Sql(sqlx::Error::Decode(Box::new(e))))?;
            out.push(Mission {
                id,
                title: row.try_get("title")?,
                description: row.try_get("description")?,
                status: parse_mission_status(&row.try_get::<String, _>("status")?),
                created_at: str_to_ts(&row.try_get::<String, _>("created_at")?),
                updated_at: str_to_ts(&row.try_get::<String, _>("updated_at")?),
                goals: Vec::new(),
            });
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Goal repository
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct SqliteGoalRepository { pool: SqlitePool }
impl SqliteGoalRepository { pub fn new(pool: SqlitePool) -> Self { Self { pool } } }

fn parse_goal_row(row: sqlx::sqlite::SqliteRow) -> Result<Goal, PersistenceError> {
    let id_str: String = row.try_get("id")?;
    let id = GoalId::from_str(&id_str)
        .map_err(|e| PersistenceError::Sql(sqlx::Error::Decode(Box::new(e))))?;
    let mid_str: String = row.try_get("mission_id")?;
    let mission_id = MissionId::from_str(&mid_str)
        .map_err(|e| PersistenceError::Sql(sqlx::Error::Decode(Box::new(e))))?;
    let deps_json: String = row.try_get("depends_on_json")?;
    let depends_on: Vec<GoalId> = serde_json::from_str(&deps_json)?;
    Ok(Goal {
        id,
        mission_id,
        title: row.try_get("title")?,
        description: row.try_get("description")?,
        status: parse_goal_status(&row.try_get::<String, _>("status")?),
        depends_on,
        confidence: row.try_get::<f64, _>("confidence")? as f32,
        priority: row.try_get::<i64, _>("priority")? as i32,
        retries_remaining: row.try_get::<i64, _>("retries_remaining")? as u8,
        tasks: Vec::new(),
    })
}

#[async_trait]
impl GoalRepository for SqliteGoalRepository {
    async fn insert(&self, g: &Goal) -> Result<(), PersistenceError> {
        let deps_json = serde_json::to_string(&g.depends_on)?;
        sqlx::query(
            r#"INSERT INTO goals
               (id, mission_id, title, description, status, depends_on_json,
                confidence, priority, retries_remaining)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
        )
        .bind(g.id.to_string())
        .bind(g.mission_id.to_string())
        .bind(&g.title)
        .bind(&g.description)
        .bind(goal_status_str(&g.status))
        .bind(deps_json)
        .bind(g.confidence as f64)
        .bind(g.priority as i64)
        .bind(g.retries_remaining as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update(&self, g: &Goal) -> Result<(), PersistenceError> {
        let deps_json = serde_json::to_string(&g.depends_on)?;
        sqlx::query(
            r#"UPDATE goals SET title=?2, description=?3, status=?4, depends_on_json=?5,
                confidence=?6, priority=?7, retries_remaining=?8 WHERE id=?1"#,
        )
        .bind(g.id.to_string())
        .bind(&g.title)
        .bind(&g.description)
        .bind(goal_status_str(&g.status))
        .bind(deps_json)
        .bind(g.confidence as f64)
        .bind(g.priority as i64)
        .bind(g.retries_remaining as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get(&self, id: GoalId) -> Result<Goal, PersistenceError> {
        let row = sqlx::query(
            "SELECT id, mission_id, title, description, status, depends_on_json, confidence, priority, retries_remaining FROM goals WHERE id=?1",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| PersistenceError::NotFound { kind: "goal", id: id.to_string() })?;
        parse_goal_row(row)
    }

    async fn list_for_mission(&self, mission_id: MissionId) -> Result<Vec<Goal>, PersistenceError> {
        let rows = sqlx::query(
            "SELECT id, mission_id, title, description, status, depends_on_json, confidence, priority, retries_remaining FROM goals WHERE mission_id=?1",
        )
        .bind(mission_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(parse_goal_row).collect()
    }
}

// ---------------------------------------------------------------------------
// Task repository
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct SqliteTaskRepository { pool: SqlitePool }
impl SqliteTaskRepository { pub fn new(pool: SqlitePool) -> Self { Self { pool } } }

fn parse_task_row(row: sqlx::sqlite::SqliteRow) -> Result<Task, PersistenceError> {
    let id_str: String = row.try_get("id")?;
    let id = TaskId::from_str(&id_str)
        .map_err(|e| PersistenceError::Sql(sqlx::Error::Decode(Box::new(e))))?;
    let gid_str: String = row.try_get("goal_id")?;
    let goal_id = GoalId::from_str(&gid_str)
        .map_err(|e| PersistenceError::Sql(sqlx::Error::Decode(Box::new(e))))?;
    let input_json: String = row.try_get("input")?;
    let input: serde_json::Value = serde_json::from_str(&input_json)?;
    let result_json: Option<String> = row.try_get("result")?;
    let result = match result_json {
        Some(s) => Some(serde_json::from_str(&s)?),
        None => None,
    };
    Ok(Task {
        id,
        goal_id,
        tool: row.try_get("tool")?,
        input,
        status: parse_task_status(&row.try_get::<String, _>("status")?),
        result,
        error: row.try_get("error")?,
        attempts: row.try_get::<i64, _>("attempts")? as u8,
    })
}

#[async_trait]
impl TaskRepository for SqliteTaskRepository {
    async fn insert(&self, t: &Task) -> Result<(), PersistenceError> {
        let input_json = serde_json::to_string(&t.input)?;
        let result_json = t.result.as_ref().map(|v| serde_json::to_string(v)).transpose()?;
        sqlx::query(
            r#"INSERT INTO tasks (id, goal_id, tool, input, status, result, error, attempts)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"#,
        )
        .bind(t.id.to_string())
        .bind(t.goal_id.to_string())
        .bind(&t.tool)
        .bind(input_json)
        .bind(task_status_str(&t.status))
        .bind(result_json)
        .bind(&t.error)
        .bind(t.attempts as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update(&self, t: &Task) -> Result<(), PersistenceError> {
        let input_json = serde_json::to_string(&t.input)?;
        let result_json = t.result.as_ref().map(|v| serde_json::to_string(v)).transpose()?;
        sqlx::query(
            r#"UPDATE tasks SET tool=?2, input=?3, status=?4, result=?5, error=?6, attempts=?7 WHERE id=?1"#,
        )
        .bind(t.id.to_string())
        .bind(&t.tool)
        .bind(input_json)
        .bind(task_status_str(&t.status))
        .bind(result_json)
        .bind(&t.error)
        .bind(t.attempts as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get(&self, id: TaskId) -> Result<Task, PersistenceError> {
        let row = sqlx::query(
            "SELECT id, goal_id, tool, input, status, result, error, attempts FROM tasks WHERE id=?1",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| PersistenceError::NotFound { kind: "task", id: id.to_string() })?;
        parse_task_row(row)
    }

    async fn list_for_goal(&self, goal_id: GoalId) -> Result<Vec<Task>, PersistenceError> {
        let rows = sqlx::query(
            "SELECT id, goal_id, tool, input, status, result, error, attempts FROM tasks WHERE goal_id=?1",
        )
        .bind(goal_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(parse_task_row).collect()
    }
}


// ────────────────────────────────────────────────────────────────────────────
// Reflections — persisted post-mission analyses.
// ────────────────────────────────────────────────────────────────────────────

pub struct SqliteReflectionRepository { pool: SqlitePool }
impl SqliteReflectionRepository {
    pub fn new(pool: SqlitePool) -> Self { Self { pool } }
}

#[async_trait]
impl crate::ReflectionRepository for SqliteReflectionRepository {
    async fn insert(
        &self,
        mission_id: MissionId,
        outcome: &str,
        payload_json: &str,
    ) -> Result<(), PersistenceError> {
        sqlx::query(
            "INSERT INTO reflections (mission_id, created_at, outcome, payload) VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(mission_id.to_string())
        .bind(ts_to_str(OffsetDateTime::now_utc()))
        .bind(outcome)
        .bind(payload_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_for_mission(
        &self,
        mission_id: MissionId,
    ) -> Result<Vec<crate::ReflectionRecord>, PersistenceError> {
        let rows = sqlx::query(
            "SELECT created_at, outcome, payload FROM reflections WHERE mission_id=?1 ORDER BY created_at",
        )
        .bind(mission_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| crate::ReflectionRecord {
            mission_id,
            created_at: r.try_get::<String, _>("created_at").unwrap_or_default(),
            outcome:    r.try_get::<String, _>("outcome").unwrap_or_default(),
            payload:    r.try_get::<String, _>("payload").unwrap_or_default(),
        }).collect())
    }
}
