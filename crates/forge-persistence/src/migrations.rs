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
