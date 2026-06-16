//! SQLite connection manager + thin repositories.
//!
//! `rusqlite` is synchronous, so the shared `Connection` is guarded by a
//! `Mutex`. Callers in async contexts should invoke these methods from inside
//! `tokio::task::spawn_blocking` to avoid stalling the runtime.

pub mod migrations;

use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension};

use crate::core::{AppError, AppResult};
use crate::domain::{EventId, ScheduleEvent, Task, TaskPriority, TaskStatus};

/// Cloneable handle to the embedded SQLite database.
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    /// Open (creating if needed) the database at `path` and auto-migrate.
    pub fn open(path: impl AsRef<Path>) -> AppResult<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        migrations::run(&conn)?;
        tracing::info!(path = %path.display(), "database ready");
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// In-memory database, primarily for tests.
    pub fn open_in_memory() -> AppResult<Self> {
        let conn = Connection::open_in_memory()?;
        migrations::run(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn lock(&self) -> AppResult<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| AppError::Other(anyhow::anyhow!("database mutex poisoned")))
    }

    // -- tasks ---------------------------------------------------------------

    pub fn insert_task(&self, task: &Task) -> AppResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO tasks (id, title, description, status, priority, due_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                task.id.to_string(),
                task.title,
                task.description,
                task.status.as_str(),
                task.priority.as_i64(),
                task.due_at.map(|d| d.to_rfc3339()),
                task.created_at.to_rfc3339(),
                task.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn list_tasks(&self) -> AppResult<Vec<Task>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, title, description, status, priority, due_at, created_at, updated_at
             FROM tasks ORDER BY priority DESC, created_at ASC",
        )?;
        let rows = stmt.query_map([], row_to_task)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Open, not-yet-done tasks that are due by `when` (or have no due date).
    /// Used by the assistant to inject "today's pending work" into the prompt.
    pub fn pending_tasks_due_by(&self, when: DateTime<Utc>) -> AppResult<Vec<Task>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, title, description, status, priority, due_at, created_at, updated_at
             FROM tasks
             WHERE status IN ('pending', 'in_progress')
               AND (due_at IS NULL OR due_at <= ?1)
             ORDER BY priority DESC, due_at ASC",
        )?;
        let rows = stmt.query_map([when.to_rfc3339()], row_to_task)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Agenda events starting within `[start, end]`.
    pub fn events_between(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> AppResult<Vec<ScheduleEvent>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, title, notes, start_at, end_at, reminded, created_at
             FROM events
             WHERE start_at >= ?1 AND start_at <= ?2
             ORDER BY start_at ASC",
        )?;
        let rows = stmt.query_map([start.to_rfc3339(), end.to_rfc3339()], row_to_event)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn count_tasks(&self) -> AppResult<i64> {
        let conn = self.lock()?;
        let n = conn
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
            .optional()?
            .unwrap_or(0);
        Ok(n)
    }

    // -- events --------------------------------------------------------------

    pub fn insert_event(&self, ev: &ScheduleEvent) -> AppResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO events (id, title, notes, start_at, end_at, reminded, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                ev.id.to_string(),
                ev.title,
                ev.notes,
                ev.start_at.to_rfc3339(),
                ev.end_at.map(|d| d.to_rfc3339()),
                ev.reminded as i64,
                ev.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }
}

// -- row parsing helpers -----------------------------------------------------

fn parse_dt(s: String) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&s)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

fn parse_dt_opt(s: Option<String>) -> Option<DateTime<Utc>> {
    s.map(parse_dt)
}

fn parse_id<T: std::str::FromStr + Default>(s: String) -> T {
    s.parse().unwrap_or_default()
}

fn row_to_task(row: &rusqlite::Row) -> rusqlite::Result<Task> {
    Ok(Task {
        id: parse_id(row.get::<_, String>(0)?),
        title: row.get(1)?,
        description: row.get(2)?,
        status: TaskStatus::from_str_lossy(&row.get::<_, String>(3)?),
        priority: TaskPriority::from_i64(row.get(4)?),
        due_at: parse_dt_opt(row.get::<_, Option<String>>(5)?),
        created_at: parse_dt(row.get::<_, String>(6)?),
        updated_at: parse_dt(row.get::<_, String>(7)?),
    })
}

fn row_to_event(row: &rusqlite::Row) -> rusqlite::Result<ScheduleEvent> {
    Ok(ScheduleEvent {
        id: parse_id::<EventId>(row.get::<_, String>(0)?),
        title: row.get(1)?,
        notes: row.get(2)?,
        start_at: parse_dt(row.get::<_, String>(3)?),
        end_at: parse_dt_opt(row.get::<_, Option<String>>(4)?),
        reminded: row.get::<_, i64>(5)? != 0,
        created_at: parse_dt(row.get::<_, String>(6)?),
    })
}
