//! Idempotent schema migrations.
//!
//! On first launch the database file and all tables are created. The
//! `schema_version` table lets us evolve the schema additively over time.

use rusqlite::Connection;

use crate::core::AppResult;

/// Bump this when adding a new migration step below.
const TARGET_VERSION: i64 = 3;

/// Ordered list of migration SQL scripts. Index + 1 == version it produces.
const MIGRATIONS: &[&str] = &[
    // v1: initial schema
    r#"
    CREATE TABLE IF NOT EXISTS tasks (
        id          TEXT PRIMARY KEY NOT NULL,
        title       TEXT NOT NULL,
        description TEXT,
        status      TEXT NOT NULL DEFAULT 'pending',
        priority    INTEGER NOT NULL DEFAULT 1,
        due_at      TEXT,
        created_at  TEXT NOT NULL,
        updated_at  TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS events (
        id          TEXT PRIMARY KEY NOT NULL,
        title       TEXT NOT NULL,
        notes       TEXT,
        start_at    TEXT NOT NULL,
        end_at      TEXT,
        reminded    INTEGER NOT NULL DEFAULT 0,
        created_at  TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS memory_nodes (
        id          TEXT PRIMARY KEY NOT NULL,
        parent_id   TEXT,
        label       TEXT NOT NULL,
        content     TEXT NOT NULL,
        tags        TEXT NOT NULL DEFAULT '[]',
        weight      REAL NOT NULL DEFAULT 1.0,
        created_at  TEXT NOT NULL,
        FOREIGN KEY (parent_id) REFERENCES memory_nodes (id) ON DELETE SET NULL
    );

    CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks (status);
    CREATE INDEX IF NOT EXISTS idx_events_start ON events (start_at);
    "#,
    // v2: delegated tasks (agile-board tickets assigned to team members)
    r#"
    CREATE TABLE IF NOT EXISTS delegated_tasks (
        id          TEXT PRIMARY KEY NOT NULL,
        assignee    TEXT NOT NULL,
        description TEXT NOT NULL,
        status      TEXT NOT NULL DEFAULT 'open',
        payload     TEXT NOT NULL DEFAULT '{}',
        created_at  TEXT NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_delegated_assignee ON delegated_tasks (assignee);
    "#,
    // v3: permanent long-term memory via FTS5 full-text search.
    r#"
    CREATE VIRTUAL TABLE IF NOT EXISTS knowledge_base
        USING fts5(title, content, tags);
    "#,
];

/// Apply any pending migrations. Safe to call on every startup.
pub fn run(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);",
    )?;

    let current: i64 = conn
        .query_row("SELECT COALESCE(MAX(version), 0) FROM schema_version", [], |r| {
            r.get(0)
        })
        .unwrap_or(0);

    for (idx, script) in MIGRATIONS.iter().enumerate() {
        let version = idx as i64 + 1;
        if version > current {
            tracing::info!(version, "applying database migration");
            conn.execute_batch(script)?;
            conn.execute("INSERT INTO schema_version (version) VALUES (?1)", [version])?;
        }
    }

    debug_assert_eq!(MIGRATIONS.len() as i64, TARGET_VERSION);
    Ok(())
}
