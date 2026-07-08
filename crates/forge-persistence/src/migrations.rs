//! Embedded SQL migrations. Applied at pool init.

pub const V001_INIT: &str = r#"
CREATE TABLE IF NOT EXISTS missions (
    id           TEXT    PRIMARY KEY,
    title        TEXT    NOT NULL,
    description  TEXT    NOT NULL,
    status       TEXT    NOT NULL,
    created_at   TEXT    NOT NULL,
    updated_at   TEXT    NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS goals (
    id                TEXT    PRIMARY KEY,
    mission_id        TEXT    NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
    title             TEXT    NOT NULL,
    description       TEXT    NOT NULL,
    status            TEXT    NOT NULL,
    depends_on_json   TEXT    NOT NULL DEFAULT '[]',
    confidence        REAL    NOT NULL DEFAULT 0.5,
    priority          INTEGER NOT NULL DEFAULT 0,
    retries_remaining INTEGER NOT NULL DEFAULT 2
) STRICT;
CREATE INDEX IF NOT EXISTS idx_goals_mission ON goals(mission_id);

CREATE TABLE IF NOT EXISTS tasks (
    id         TEXT    PRIMARY KEY,
    goal_id    TEXT    NOT NULL REFERENCES goals(id) ON DELETE CASCADE,
    tool       TEXT    NOT NULL,
    input      TEXT    NOT NULL,
    status     TEXT    NOT NULL,
    result     TEXT,
    error      TEXT,
    attempts   INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX IF NOT EXISTS idx_tasks_goal ON tasks(goal_id);

CREATE TABLE IF NOT EXISTS events (
    seq            INTEGER PRIMARY KEY AUTOINCREMENT,
    aggregate_id   TEXT    NOT NULL,
    aggregate_type TEXT    NOT NULL,
    event_type     TEXT    NOT NULL,
    payload        TEXT    NOT NULL,
    created_at     TEXT    NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS idx_events_agg  ON events(aggregate_id, seq);
CREATE INDEX IF NOT EXISTS idx_events_time ON events(created_at);

CREATE TABLE IF NOT EXISTS reflections (
    mission_id  TEXT    NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
    created_at  TEXT    NOT NULL,
    outcome     TEXT    NOT NULL,
    payload     TEXT    NOT NULL,
    PRIMARY KEY (mission_id, created_at)
) STRICT;
"#;

/// Phase 4a — Version-controlled skills.
///
/// Immutable history log: every skill promotion, rollback, or retirement
/// appends a row here. The active version is the most-recent row for a
/// given name whose `retired_at` IS NULL. Content is content-addressed by
/// `sha` — the byte SHA-256 of the SKILL.md file we snapshotted at
/// promotion time. Rollback = promote a prior sha (new row, same sha).
///
/// Nothing here overwrites: every state change appends. This mirrors the
/// event-sourcing discipline used elsewhere in the runtime and satisfies
/// agent.txt's mandate "Every learned improvement should be
/// version-controlled. Nothing should ever be overwritten."
pub const V002_SKILLS_HISTORY: &str = r#"
CREATE TABLE IF NOT EXISTS skills_history (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    name              TEXT    NOT NULL,
    sha               TEXT    NOT NULL,
    version           TEXT    NOT NULL,
    origin            TEXT    NOT NULL,
    origin_mission_id TEXT,
    parent_sha        TEXT,
    promoted_at       TEXT    NOT NULL,
    retired_at        TEXT,
    reason            TEXT
) STRICT;
CREATE INDEX IF NOT EXISTS idx_skills_history_name       ON skills_history(name, id);
CREATE INDEX IF NOT EXISTS idx_skills_history_sha        ON skills_history(sha);
CREATE INDEX IF NOT EXISTS idx_skills_history_promoted   ON skills_history(promoted_at);
"#;

/// Phase 4d — Persisted mission execution queue.
///
/// One row per plan_and_run invocation. Workers `claim` a queued row
/// atomically, `heartbeat` while running, and `finish` (or `fail`) on
/// completion. On boot, orphaned rows (status=claimed with stale
/// heartbeat) are requeued so a crash can't lose a mission.
pub const V003_MISSION_QUEUE: &str = r#"
CREATE TABLE IF NOT EXISTS mission_queue (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    mission_id    TEXT    NOT NULL,
    status        TEXT    NOT NULL,         -- queued | claimed | done | failed
    claimed_by    TEXT,
    claimed_at    TEXT,
    heartbeat_at  TEXT,
    finished_at   TEXT,
    error         TEXT,
    enqueued_at   TEXT    NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS idx_mission_queue_status   ON mission_queue(status, enqueued_at);
CREATE INDEX IF NOT EXISTS idx_mission_queue_mission  ON mission_queue(mission_id);
"#;

/// Phase 4f — Organizational memory.
///
/// Durable, cross-mission facts extracted from reflections and surfaced
/// back into the planner prompt on future missions. MVP uses LIKE-search
/// on `tags`; embedding-based recall is Phase 5. Each row is append-only;
/// `retired_at` is the only mutable column (soft-delete for UI).
pub const V004_ORG_MEMORY: &str = r#"
CREATE TABLE IF NOT EXISTS org_memory (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    key               TEXT    NOT NULL,
    value             TEXT    NOT NULL,
    tags              TEXT    NOT NULL DEFAULT '[]',
    source_mission_id TEXT,
    created_at        TEXT    NOT NULL,
    retired_at        TEXT
) STRICT;
CREATE INDEX IF NOT EXISTS idx_org_memory_key      ON org_memory(key);
CREATE INDEX IF NOT EXISTS idx_org_memory_active   ON org_memory(retired_at, created_at);
CREATE INDEX IF NOT EXISTS idx_org_memory_source   ON org_memory(source_mission_id);
"#;
