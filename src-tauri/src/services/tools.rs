//! Tool calling.
//!
//! Small local models struggle with strict JSON, so Kensho uses a robust tag
//! protocol the model emits inline in its stream:
//!
//!   `<CALL:ADD_TASK>Título da tarefa|2026-06-20</CALL>`
//!
//! The actor's stream filter extracts the body between `<CALL:` and `</CALL>`
//! and hands it here. The date segment (after `|`) is optional.

use chrono::{NaiveDate, TimeZone, Utc};

use crate::core::{AppError, AppResult};
use crate::domain::Task;
use crate::infrastructure::{Database, Notifier};

/// A parsed tool invocation: a command name plus its raw argument string.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub name: String,
    pub raw_args: String,
}

impl ToolCall {
    /// Parse the captured body, e.g. `ADD_TASK>Comprar pão|2026-06-20`.
    /// The command name is everything up to the first `>`.
    pub fn parse(body: &str) -> Self {
        match body.find('>') {
            Some(idx) => ToolCall {
                name: body[..idx].trim().to_uppercase(),
                raw_args: body[idx + 1..].to_string(),
            },
            None => ToolCall {
                name: body.trim().to_uppercase(),
                raw_args: String::new(),
            },
        }
    }
}

/// Executes tool calls against the system (DB + OS notifications).
#[derive(Clone)]
pub struct ToolExecutor {
    db: Database,
    notifier: Notifier,
}

impl ToolExecutor {
    pub fn new(db: Database, notifier: Notifier) -> Self {
        Self { db, notifier }
    }

    /// Dispatch a parsed call. Returns a short human-readable summary.
    pub async fn execute(&self, call: ToolCall) -> AppResult<String> {
        match call.name.as_str() {
            "ADD_TASK" => self.add_task(&call.raw_args).await,
            other => {
                tracing::warn!(command = other, "unknown tool call");
                Ok(format!("Comando desconhecido: {other}"))
            }
        }
    }

    async fn add_task(&self, args: &str) -> AppResult<String> {
        let mut parts = args.splitn(2, '|');
        let title = parts.next().unwrap_or("").trim().to_string();
        let date = parts.next().map(str::trim).filter(|s| !s.is_empty());

        if title.is_empty() {
            return Ok("Tarefa sem título — ignorada.".to_string());
        }

        let mut task = Task::new(title.clone());
        if let Some(d) = date {
            if let Ok(nd) = NaiveDate::parse_from_str(d, "%Y-%m-%d") {
                if let Some(naive) = nd.and_hms_opt(9, 0, 0) {
                    task.due_at = Some(Utc.from_utc_datetime(&naive));
                }
            }
        }

        // DB write off the async runtime.
        let db = self.db.clone();
        let to_store = task.clone();
        tokio::task::spawn_blocking(move || db.insert_task(&to_store))
            .await
            .map_err(|e| AppError::Other(anyhow::anyhow!("join error: {e}")))??;

        // Native Ubuntu notification (blocking D-Bus call → blocking thread).
        let notifier = self.notifier.clone();
        let summary_title = title.clone();
        let _ = tokio::task::spawn_blocking(move || {
            notifier.notify("Kensho", &format!("Tarefa adicionada: {summary_title}"))
        })
        .await;

        tracing::info!(task = %title, "tool ADD_TASK executed");
        Ok(format!("Tarefa \"{title}\" adicionada."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_command_and_args() {
        let c = ToolCall::parse("ADD_TASK>Comprar pão|2026-06-20");
        assert_eq!(c.name, "ADD_TASK");
        assert_eq!(c.raw_args, "Comprar pão|2026-06-20");
    }

    #[tokio::test]
    async fn add_task_inserts_row() {
        let db = Database::open_in_memory().expect("db");
        let exec = ToolExecutor::new(db.clone(), Notifier::default());

        // Notification may no-op without a D-Bus daemon; the DB write must succeed.
        let call = ToolCall::parse("ADD_TASK>Comprar pão|2026-06-20");
        let summary = exec.execute(call).await.expect("execute");

        assert!(summary.contains("Comprar pão"));
        assert_eq!(db.count_tasks().expect("count"), 1);
    }
}
