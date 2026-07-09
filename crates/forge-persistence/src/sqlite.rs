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
    // Phase 4a: skills_history for version-controlled learning.
    for stmt in migrations::V002_SKILLS_HISTORY.split(';') {
        let s = stmt.trim();
        if s.is_empty() { continue; }
        sqlx::query(s).execute(&pool).await?;
    }
    // Phase 4d: persisted mission-execution queue.
    for stmt in migrations::V003_MISSION_QUEUE.split(';') {
        let s = stmt.trim();
        if s.is_empty() { continue; }
        sqlx::query(s).execute(&pool).await?;
    }
    // Phase 4f: organizational memory.
    for stmt in migrations::V004_ORG_MEMORY.split(';') {
        let s = stmt.trim();
        if s.is_empty() { continue; }
        sqlx::query(s).execute(&pool).await?;
    }
    // Phase 6a: semantic-memory embedding columns. `ALTER TABLE ADD COLUMN`
    // isn't idempotent in SQLite, so we tolerate "duplicate column name"
    // errors (i.e. the migration already ran on a previous boot).
    for stmt in migrations::V005_SEMANTIC_MEMORY.split(';') {
        let s = stmt.trim();
        if s.is_empty() { continue; }
        if let Err(e) = sqlx::query(s).execute(&pool).await {
            let msg = e.to_string();
            if !msg.contains("duplicate column") {
                return Err(e.into());
            }
        }
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

// ────────────────────────────────────────────────────────────────────────────
// Phase 4a — skills_history
// ────────────────────────────────────────────────────────────────────────────
//
// Append-only log. See `crate::SkillHistoryRepository` doc for semantics.
// The "currently active" version of a skill is the newest row for that name
// with `retired_at IS NULL`. Rollback = insert a new row referencing an
// older sha. Nothing here mutates prior rows.

pub struct SqliteSkillHistoryRepository { pool: SqlitePool }
impl SqliteSkillHistoryRepository {
    pub fn new(pool: SqlitePool) -> Self { Self { pool } }
}

fn parse_history_row(r: sqlx::sqlite::SqliteRow) -> crate::SkillVersionRecord {
    crate::SkillVersionRecord {
        id:                r.try_get::<i64, _>("id").unwrap_or_default(),
        name:              r.try_get::<String, _>("name").unwrap_or_default(),
        sha:               r.try_get::<String, _>("sha").unwrap_or_default(),
        version:           r.try_get::<String, _>("version").unwrap_or_default(),
        origin:            crate::SkillOrigin::parse(&r.try_get::<String, _>("origin").unwrap_or_default()),
        origin_mission_id: r.try_get::<Option<String>, _>("origin_mission_id").unwrap_or(None),
        parent_sha:        r.try_get::<Option<String>, _>("parent_sha").unwrap_or(None),
        promoted_at:       r.try_get::<String, _>("promoted_at").unwrap_or_default(),
        retired_at:        r.try_get::<Option<String>, _>("retired_at").unwrap_or(None),
        reason:            r.try_get::<Option<String>, _>("reason").unwrap_or(None),
    }
}

#[async_trait]
impl crate::SkillHistoryRepository for SqliteSkillHistoryRepository {
    async fn promote(&self, v: &crate::NewSkillVersion) -> Result<i64, PersistenceError> {
        let res = sqlx::query(
            "INSERT INTO skills_history (name, sha, version, origin, origin_mission_id, parent_sha, promoted_at, retired_at, reason) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8)",
        )
        .bind(&v.name)
        .bind(&v.sha)
        .bind(&v.version)
        .bind(v.origin.as_str())
        .bind(&v.origin_mission_id)
        .bind(&v.parent_sha)
        .bind(ts_to_str(OffsetDateTime::now_utc()))
        .bind(&v.reason)
        .execute(&self.pool)
        .await?;
        Ok(res.last_insert_rowid())
    }

    async fn retire_active(&self, name: &str, reason: &str) -> Result<bool, PersistenceError> {
        // Find the currently-active row (newest, retired_at IS NULL).
        let row = sqlx::query(
            "SELECT id FROM skills_history WHERE name=?1 AND retired_at IS NULL ORDER BY id DESC LIMIT 1",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(false); };
        let id: i64 = row.try_get("id")?;
        sqlx::query(
            "UPDATE skills_history SET retired_at=?1, reason=COALESCE(reason || '\n' || ?2, ?2) WHERE id=?3",
        )
        .bind(ts_to_str(OffsetDateTime::now_utc()))
        .bind(reason)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(true)
    }

    async fn active(&self, name: &str) -> Result<Option<crate::SkillVersionRecord>, PersistenceError> {
        let row = sqlx::query(
            "SELECT id, name, sha, version, origin, origin_mission_id, parent_sha, promoted_at, retired_at, reason \
             FROM skills_history WHERE name=?1 AND retired_at IS NULL ORDER BY id DESC LIMIT 1",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(parse_history_row))
    }

    async fn history(&self, name: &str) -> Result<Vec<crate::SkillVersionRecord>, PersistenceError> {
        let rows = sqlx::query(
            "SELECT id, name, sha, version, origin, origin_mission_id, parent_sha, promoted_at, retired_at, reason \
             FROM skills_history WHERE name=?1 ORDER BY id DESC",
        )
        .bind(name)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(parse_history_row).collect())
    }

    async fn list_active(&self) -> Result<Vec<crate::SkillVersionRecord>, PersistenceError> {
        // Newest non-retired row per name. Uses a correlated subquery for
        // clarity — the table is small (bounded by unique skill count).
        let rows = sqlx::query(
            "SELECT id, name, sha, version, origin, origin_mission_id, parent_sha, promoted_at, retired_at, reason \
             FROM skills_history AS a \
             WHERE retired_at IS NULL \
               AND id = (SELECT MAX(id) FROM skills_history WHERE name = a.name AND retired_at IS NULL) \
             ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(parse_history_row).collect())
    }
}


// ────────────────────────────────────────────────────────────────────────────
// Phase 4d — mission execution queue
// ────────────────────────────────────────────────────────────────────────────

pub struct SqliteMissionQueueRepository { pool: SqlitePool }
impl SqliteMissionQueueRepository {
    pub fn new(pool: SqlitePool) -> Self { Self { pool } }
}

fn parse_queue_row(r: sqlx::sqlite::SqliteRow) -> crate::MissionQueueRow {
    crate::MissionQueueRow {
        id:           r.try_get::<i64, _>("id").unwrap_or_default(),
        mission_id:   r.try_get::<String, _>("mission_id").unwrap_or_default(),
        status:       crate::QueueStatus::parse(&r.try_get::<String, _>("status").unwrap_or_default()),
        claimed_by:   r.try_get::<Option<String>, _>("claimed_by").unwrap_or(None),
        claimed_at:   r.try_get::<Option<String>, _>("claimed_at").unwrap_or(None),
        heartbeat_at: r.try_get::<Option<String>, _>("heartbeat_at").unwrap_or(None),
        finished_at:  r.try_get::<Option<String>, _>("finished_at").unwrap_or(None),
        error:        r.try_get::<Option<String>, _>("error").unwrap_or(None),
        enqueued_at:  r.try_get::<String, _>("enqueued_at").unwrap_or_default(),
    }
}

#[async_trait]
impl crate::MissionQueueRepository for SqliteMissionQueueRepository {
    async fn enqueue(&self, mission_id: MissionId) -> Result<i64, PersistenceError> {
        let mid = mission_id.to_string();
        // If an active (Queued|Claimed) row exists for this mission, reuse it.
        if let Some(row) = sqlx::query(
            "SELECT id FROM mission_queue WHERE mission_id=?1 AND status IN ('queued','claimed') ORDER BY id ASC LIMIT 1",
        )
        .bind(&mid)
        .fetch_optional(&self.pool)
        .await? {
            return Ok(row.try_get::<i64, _>("id")?);
        }
        let now = ts_to_str(OffsetDateTime::now_utc());
        let res = sqlx::query(
            "INSERT INTO mission_queue (mission_id, status, enqueued_at) VALUES (?1, 'queued', ?2)",
        )
        .bind(&mid)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(res.last_insert_rowid())
    }

    async fn claim_next(&self, worker_id: &str) -> Result<Option<crate::MissionQueueRow>, PersistenceError> {
        // SQLite doesn't support UPDATE...RETURNING in every distribution,
        // so we do the claim in a transaction: SELECT the oldest queued row,
        // then UPDATE it if its status is still 'queued'.
        let mut tx = self.pool.begin().await?;
        let row_opt = sqlx::query(
            "SELECT id FROM mission_queue WHERE status='queued' ORDER BY id ASC LIMIT 1",
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row_opt else {
            tx.commit().await?;
            return Ok(None);
        };
        let id: i64 = row.try_get("id")?;
        let now = ts_to_str(OffsetDateTime::now_utc());
        let n = sqlx::query(
            "UPDATE mission_queue SET status='claimed', claimed_by=?1, claimed_at=?2, heartbeat_at=?2 \
             WHERE id=?3 AND status='queued'",
        )
        .bind(worker_id)
        .bind(&now)
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if n == 0 {
            // Lost the race — someone else claimed it. Return None; the
            // worker will loop and try again.
            tx.commit().await?;
            return Ok(None);
        }
        let full = sqlx::query(
            "SELECT id, mission_id, status, claimed_by, claimed_at, heartbeat_at, finished_at, error, enqueued_at \
             FROM mission_queue WHERE id=?1",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(parse_queue_row(full)))
    }

    async fn heartbeat(&self, id: i64) -> Result<(), PersistenceError> {
        let now = ts_to_str(OffsetDateTime::now_utc());
        sqlx::query(
            "UPDATE mission_queue SET heartbeat_at=?1 WHERE id=?2 AND status='claimed'",
        )
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn finish(&self, id: i64, success: bool, error: Option<&str>) -> Result<(), PersistenceError> {
        let now = ts_to_str(OffsetDateTime::now_utc());
        let status = if success { "done" } else { "failed" };
        sqlx::query(
            "UPDATE mission_queue SET status=?1, finished_at=?2, error=?3 WHERE id=?4",
        )
        .bind(status)
        .bind(now)
        .bind(error)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn requeue_stale(&self, stale_after_secs: i64) -> Result<usize, PersistenceError> {
        // SQLite stores our timestamps as ISO-8601 strings; we compare
        // string-wise against a cutoff timestamp we compute in Rust.
        let cutoff = ts_to_str(OffsetDateTime::now_utc() - time::Duration::seconds(stale_after_secs));
        let res = sqlx::query(
            "UPDATE mission_queue \
             SET status='queued', claimed_by=NULL, claimed_at=NULL, heartbeat_at=NULL \
             WHERE status='claimed' AND (heartbeat_at IS NULL OR heartbeat_at < ?1)",
        )
        .bind(cutoff)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() as usize)
    }

    async fn depth(&self) -> Result<(usize, usize), PersistenceError> {
        let row = sqlx::query(
            "SELECT \
               SUM(CASE WHEN status='queued'  THEN 1 ELSE 0 END) AS q, \
               SUM(CASE WHEN status='claimed' THEN 1 ELSE 0 END) AS c \
             FROM mission_queue",
        )
        .fetch_one(&self.pool)
        .await?;
        let q: Option<i64> = row.try_get("q").ok();
        let c: Option<i64> = row.try_get("c").ok();
        Ok((q.unwrap_or(0) as usize, c.unwrap_or(0) as usize))
    }

    async fn recent(&self, limit: usize) -> Result<Vec<crate::MissionQueueRow>, PersistenceError> {
        let rows = sqlx::query(
            "SELECT id, mission_id, status, claimed_by, claimed_at, heartbeat_at, finished_at, error, enqueued_at \
             FROM mission_queue ORDER BY id DESC LIMIT ?1",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(parse_queue_row).collect())
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Phase 4f — organizational memory
// ────────────────────────────────────────────────────────────────────────────

pub struct SqliteOrgMemoryRepository { pool: SqlitePool }
impl SqliteOrgMemoryRepository {
    pub fn new(pool: SqlitePool) -> Self { Self { pool } }
}

fn parse_memory_row(r: sqlx::sqlite::SqliteRow) -> crate::OrgMemoryRow {
    let tags_json: String = r.try_get::<String, _>("tags").unwrap_or_else(|_| "[]".into());
    let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
    let embedding_bytes: Option<Vec<u8>> = r.try_get::<Option<Vec<u8>>, _>("embedding").unwrap_or(None);
    let embedding = embedding_bytes.and_then(bytes_to_vec_f32);
    crate::OrgMemoryRow {
        id:                r.try_get::<i64, _>("id").unwrap_or_default(),
        key:               r.try_get::<String, _>("key").unwrap_or_default(),
        value:             r.try_get::<String, _>("value").unwrap_or_default(),
        tags,
        source_mission_id: r.try_get::<Option<String>, _>("source_mission_id").unwrap_or(None),
        created_at:        r.try_get::<String, _>("created_at").unwrap_or_default(),
        retired_at:        r.try_get::<Option<String>, _>("retired_at").unwrap_or(None),
        embedding,
    }
}

/// Phase 6a — encode a Vec<f32> as raw little-endian bytes for BLOB storage.
fn vec_f32_to_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Phase 6a — inverse of `vec_f32_to_bytes`. Returns None on a truncated
/// blob (length not divisible by 4) so callers can silently skip corrupt
/// rows instead of aborting the search.
fn bytes_to_vec_f32(b: Vec<u8>) -> Option<Vec<f32>> {
    if b.len() % 4 != 0 { return None; }
    let mut out = Vec::with_capacity(b.len() / 4);
    for chunk in b.chunks_exact(4) {
        out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Some(out)
}

/// Phase 6a — cosine similarity, `1.0` = identical direction, `0.0` =
/// orthogonal. Returns 0.0 if either vector is zero-length.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() { return 0.0; }
    let mut dot = 0.0f32;
    let mut na  = 0.0f32;
    let mut nb  = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na  += a[i] * a[i];
        nb  += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 { return 0.0; }
    dot / (na.sqrt() * nb.sqrt())
}

#[async_trait]
impl crate::OrgMemoryRepository for SqliteOrgMemoryRepository {
    async fn insert(&self, m: &crate::NewOrgMemory) -> Result<i64, PersistenceError> {
        let tags_json = serde_json::to_string(&m.tags)?;
        let now = ts_to_str(OffsetDateTime::now_utc());
        let src = m.source_mission_id.map(|id| id.to_string());
        let (emb_blob, emb_dim): (Option<Vec<u8>>, Option<i64>) = match m.embedding.as_ref() {
            Some(v) if !v.is_empty() => (Some(vec_f32_to_bytes(v)), Some(v.len() as i64)),
            _ => (None, None),
        };
        let res = sqlx::query(
            "INSERT INTO org_memory (key, value, tags, source_mission_id, created_at, embedding, embedding_dim) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(&m.key)
        .bind(&m.value)
        .bind(tags_json)
        .bind(src)
        .bind(now)
        .bind(emb_blob)
        .bind(emb_dim)
        .execute(&self.pool)
        .await?;
        Ok(res.last_insert_rowid())
    }

    async fn retire(&self, id: i64) -> Result<bool, PersistenceError> {
        let now = ts_to_str(OffsetDateTime::now_utc());
        let res = sqlx::query(
            "UPDATE org_memory SET retired_at=?1 WHERE id=?2 AND retired_at IS NULL",
        )
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    async fn list_active(&self, limit: usize) -> Result<Vec<crate::OrgMemoryRow>, PersistenceError> {
        let rows = sqlx::query(
            "SELECT id, key, value, tags, source_mission_id, created_at, retired_at, embedding \
             FROM org_memory WHERE retired_at IS NULL ORDER BY id DESC LIMIT ?1",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(parse_memory_row).collect())
    }

    async fn search(&self, keywords: &[String], limit: usize) -> Result<Vec<crate::OrgMemoryRow>, PersistenceError> {
        if keywords.is_empty() {
            return Ok(vec![]);
        }
        // Build an OR of LIKE clauses over (key, value, tags). Score by
        // sum-of-matches computed in Rust after fetch. `LIKE` is
        // case-insensitive for ASCII in SQLite; we lowercase the keyword
        // and rely on that being close enough for a personal-project MVP.
        let mut where_clauses = Vec::with_capacity(keywords.len() * 3);
        let mut binds: Vec<String> = Vec::with_capacity(keywords.len() * 3);
        for kw in keywords {
            let pat = format!("%{}%", kw.to_lowercase());
            where_clauses.push("(LOWER(key) LIKE ? OR LOWER(value) LIKE ? OR LOWER(tags) LIKE ?)".to_string());
            binds.push(pat.clone());
            binds.push(pat.clone());
            binds.push(pat);
        }
        let sql = format!(
            "SELECT id, key, value, tags, source_mission_id, created_at, retired_at, embedding \
             FROM org_memory WHERE retired_at IS NULL AND ({}) ORDER BY id DESC LIMIT ?",
            where_clauses.join(" OR "),
        );
        let mut q = sqlx::query(&sql);
        for b in &binds { q = q.bind(b); }
        q = q.bind(limit as i64);
        let rows = q.fetch_all(&self.pool).await?;
        // Score client-side: count of keyword hits across (key+value+tags).
        let mut scored: Vec<(usize, crate::OrgMemoryRow)> = rows.into_iter().map(|r| {
            let row = parse_memory_row(r);
            let hay = format!("{} {} {}", row.key.to_lowercase(), row.value.to_lowercase(), row.tags.join(" ").to_lowercase());
            let score = keywords.iter().filter(|k| hay.contains(&k.to_lowercase())).count();
            (score, row)
        }).collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.id.cmp(&a.1.id)));
        Ok(scored.into_iter().map(|(_, r)| r).collect())
    }

    async fn semantic_search(
        &self,
        query: &[f32],
        limit: usize,
    ) -> Result<Vec<(f32, crate::OrgMemoryRow)>, PersistenceError> {
        if query.is_empty() || limit == 0 {
            return Ok(vec![]);
        }
        // We only care about rows whose embedding dimension matches the
        // query — mixing providers with different dims would produce
        // meaningless cosine values. SQLite can filter this cheaply.
        let rows = sqlx::query(
            "SELECT id, key, value, tags, source_mission_id, created_at, retired_at, embedding \
             FROM org_memory \
             WHERE retired_at IS NULL AND embedding IS NOT NULL AND embedding_dim = ?1",
        )
        .bind(query.len() as i64)
        .fetch_all(&self.pool)
        .await?;
        let mut scored: Vec<(f32, crate::OrgMemoryRow)> = rows.into_iter().filter_map(|r| {
            let row = parse_memory_row(r);
            let emb = row.embedding.as_ref()?;
            Some((cosine(query, emb), row))
        }).collect();
        // Descending by score; NaN pushed to the bottom.
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored)
    }

    async fn set_embedding(&self, id: i64, embedding: &[f32]) -> Result<bool, PersistenceError> {
        if embedding.is_empty() {
            return Ok(false);
        }
        let blob = vec_f32_to_bytes(embedding);
        let res = sqlx::query(
            "UPDATE org_memory SET embedding=?1, embedding_dim=?2 WHERE id=?3",
        )
        .bind(blob)
        .bind(embedding.len() as i64)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }
}

#[cfg(test)]
mod queue_and_memory_tests {
    use super::*;
    use crate::{MissionQueueRepository, OrgMemoryRepository, NewOrgMemory};
    use forge_domain::MissionId;

    async fn fresh_pool() -> SqlitePool {
        connect("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn queue_enqueue_is_idempotent_on_active_mission() {
        let pool = fresh_pool().await;
        let repo = SqliteMissionQueueRepository::new(pool);
        let mid = MissionId::new();
        let a = repo.enqueue(mid).await.unwrap();
        let b = repo.enqueue(mid).await.unwrap();
        assert_eq!(a, b, "second enqueue of the same active mission returned different id");
    }

    #[tokio::test]
    async fn queue_claim_and_finish_flow() {
        let pool = fresh_pool().await;
        let repo = SqliteMissionQueueRepository::new(pool);
        let m1 = MissionId::new();
        let m2 = MissionId::new();
        repo.enqueue(m1).await.unwrap();
        repo.enqueue(m2).await.unwrap();
        // Two workers claim in order.
        let c1 = repo.claim_next("w1").await.unwrap().unwrap();
        let c2 = repo.claim_next("w2").await.unwrap().unwrap();
        assert_ne!(c1.id, c2.id);
        assert_eq!(c1.mission_id, m1.to_string());
        // Empty now.
        assert!(repo.claim_next("w3").await.unwrap().is_none());
        // Heartbeat + finish.
        repo.heartbeat(c1.id).await.unwrap();
        repo.finish(c1.id, true, None).await.unwrap();
        repo.finish(c2.id, false, Some("boom")).await.unwrap();
        let (q, c) = repo.depth().await.unwrap();
        assert_eq!((q, c), (0, 0));
    }

    #[tokio::test]
    async fn queue_requeues_stale_claims() {
        let pool = fresh_pool().await;
        let repo = SqliteMissionQueueRepository::new(pool);
        let m = MissionId::new();
        repo.enqueue(m).await.unwrap();
        let c = repo.claim_next("w1").await.unwrap().unwrap();
        assert_eq!(c.status, crate::QueueStatus::Claimed);
        // stale_after=0 → any claimed row (heartbeat is exactly `now`, so
        // string-wise < now-INTERVAL is only guaranteed for very small
        // negative intervals). Use -1 to force requeue.
        let n = repo.requeue_stale(-1).await.unwrap();
        assert_eq!(n, 1);
        let (q, cc) = repo.depth().await.unwrap();
        assert_eq!((q, cc), (1, 0));
    }

    #[tokio::test]
    async fn memory_insert_search_retire() {
        let pool = fresh_pool().await;
        let repo = SqliteOrgMemoryRepository::new(pool);
        let id = repo.insert(&NewOrgMemory {
            key:               "python_test_runner".into(),
            value:             "This repo uses pytest -q via a venv at .venv/".into(),
            tags:              vec!["python".into(), "testing".into()],
            source_mission_id: None,
            embedding:         None,
        }).await.unwrap();
        let _ = repo.insert(&NewOrgMemory {
            key:               "unrelated".into(),
            value:             "totally different subject matter".into(),
            tags:              vec!["misc".into()],
            source_mission_id: None,
            embedding:         None,
        }).await.unwrap();
        let hits = repo.search(&["python".into()], 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].key, "python_test_runner");
        // Empty keywords → empty
        assert!(repo.search(&[], 10).await.unwrap().is_empty());
        // Retire hides from list_active
        assert!(repo.retire(id).await.unwrap());
        let active = repo.list_active(10).await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].key, "unrelated");
    }

    // ── Phase 6a — semantic memory ──────────────────────────────────────

    #[tokio::test]
    async fn memory_semantic_search_ranks_by_cosine() {
        let pool = fresh_pool().await;
        let repo = SqliteOrgMemoryRepository::new(pool);

        // Insert three rows with hand-picked orthogonal-ish 4-dim vectors
        // so we can predict cosine scores precisely.
        let mostly_x = vec![1.0_f32, 0.0, 0.0, 0.0];
        let some_x   = vec![0.6_f32, 0.4, 0.4, 0.4];
        let mostly_y = vec![0.0_f32, 1.0, 0.0, 0.0];

        let id_x = repo.insert(&NewOrgMemory {
            key: "x".into(), value: "along x axis".into(),
            tags: vec![], source_mission_id: None,
            embedding: Some(mostly_x.clone()),
        }).await.unwrap();
        let id_mid = repo.insert(&NewOrgMemory {
            key: "mid".into(), value: "biased toward x with some y/z".into(),
            tags: vec![], source_mission_id: None,
            embedding: Some(some_x.clone()),
        }).await.unwrap();
        let _id_y = repo.insert(&NewOrgMemory {
            key: "y".into(), value: "along y axis".into(),
            tags: vec![], source_mission_id: None,
            embedding: Some(mostly_y.clone()),
        }).await.unwrap();

        // Query along x — the pure-x row should rank first, mid second.
        let hits = repo.semantic_search(&mostly_x, 3).await.unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].1.id, id_x);
        assert_eq!(hits[1].1.id, id_mid);
        // cosine of x with y is 0, so worst hit should be near 0.
        assert!(hits[2].0.abs() < 0.001);
        // Best hit's cosine is 1.
        assert!((hits[0].0 - 1.0).abs() < 0.001);
    }

    #[tokio::test]
    async fn memory_semantic_search_ignores_mismatched_dims() {
        let pool = fresh_pool().await;
        let repo = SqliteOrgMemoryRepository::new(pool);
        // A dim-4 row and a dim-8 row; a dim-4 query should only see the dim-4 row.
        let _id_a = repo.insert(&NewOrgMemory {
            key: "a".into(), value: "".into(), tags: vec![],
            source_mission_id: None,
            embedding: Some(vec![1.0, 0.0, 0.0, 0.0]),
        }).await.unwrap();
        let _id_b = repo.insert(&NewOrgMemory {
            key: "b".into(), value: "".into(), tags: vec![],
            source_mission_id: None,
            embedding: Some(vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
        }).await.unwrap();
        let hits = repo.semantic_search(&[1.0_f32, 0.0, 0.0, 0.0], 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1.key, "a");
    }

    #[tokio::test]
    async fn memory_set_embedding_backfills_existing_row() {
        let pool = fresh_pool().await;
        let repo = SqliteOrgMemoryRepository::new(pool);
        let id = repo.insert(&NewOrgMemory {
            key: "k".into(), value: "v".into(), tags: vec![],
            source_mission_id: None, embedding: None,
        }).await.unwrap();
        // Before backfill: semantic search returns nothing.
        let before = repo.semantic_search(&[1.0_f32, 0.0], 5).await.unwrap();
        assert!(before.is_empty());
        // Backfill and re-query.
        assert!(repo.set_embedding(id, &[1.0_f32, 0.0]).await.unwrap());
        let after = repo.semantic_search(&[1.0_f32, 0.0], 5).await.unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].1.id, id);
        // Retired rows are excluded from semantic search too.
        assert!(repo.retire(id).await.unwrap());
        let post_retire = repo.semantic_search(&[1.0_f32, 0.0], 5).await.unwrap();
        assert!(post_retire.is_empty());
    }
}
