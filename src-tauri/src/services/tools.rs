//! Tool calling — extensible router.
//!
//! Small local models struggle with strict JSON, so Kensho uses a robust tag
//! protocol the model emits inline in its stream:
//!
//!   `<CALL:ADD_TASK>Título da tarefa|2026-06-20</CALL>`
//!
//! Architecture (kept clean for a future MCP / open-orchestration backend):
//!   * the LLM emits *intent* (the tag),
//!   * the actor's stream filter intercepts it and hands the body here,
//!   * a [`ToolRouter`] dispatches to a registered [`Tool`] by name,
//!   * the tool executes against the system and returns a summary for the log.
//!
//! Adding a new capability = implement [`Tool`] and `register` it. No changes
//! to the actor, the parser, or the prompt plumbing.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{NaiveDate, TimeZone, Utc};

use crate::core::AppResult;
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

/// A single executable capability. `Send + Sync` so the router can be shared
/// across the async actor and run tools concurrently in the future.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Upper-case command name matched against `<CALL:NAME>`.
    fn name(&self) -> &str;

    /// Execute with the raw argument string; return a human-readable summary.
    async fn execute(&self, raw_args: &str) -> AppResult<String>;
}

/// Routes a [`ToolCall`] to the matching [`Tool`].
#[derive(Clone)]
pub struct ToolRouter {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRouter {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool under its `name()`.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Build the default router with the built-in capabilities.
    pub fn with_defaults(db: Database, notifier: Notifier) -> Self {
        let mut router = Self::new();
        router.register(Arc::new(AddTaskTool { db, notifier }));
        router
    }

    /// Intercept + execute. Unknown commands are logged, not fatal.
    pub async fn dispatch(&self, call: ToolCall) -> AppResult<String> {
        match self.tools.get(&call.name) {
            Some(tool) => {
                let summary = tool.execute(&call.raw_args).await?;
                tracing::info!(command = %call.name, %summary, "tool executed");
                Ok(summary)
            }
            None => {
                tracing::warn!(command = %call.name, "unknown tool call ignored");
                Ok(format!("Comando desconhecido: {}", call.name))
            }
        }
    }
}

impl Default for ToolRouter {
    fn default() -> Self {
        Self::new()
    }
}

/// Built-in tool: persist a task + fire a native notification.
struct AddTaskTool {
    db: Database,
    notifier: Notifier,
}

#[async_trait]
impl Tool for AddTaskTool {
    fn name(&self) -> &str {
        "ADD_TASK"
    }

    async fn execute(&self, raw_args: &str) -> AppResult<String> {
        let mut parts = raw_args.splitn(2, '|');
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
            .map_err(|e| crate::core::AppError::Other(anyhow::anyhow!("join error: {e}")))??;

        // Native Ubuntu notification (blocking D-Bus call → blocking thread).
        let notifier = self.notifier.clone();
        let summary_title = title.clone();
        let _ = tokio::task::spawn_blocking(move || {
            notifier.notify("Kensho", &format!("Tarefa adicionada: {summary_title}"))
        })
        .await;

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
    async fn router_dispatches_add_task() {
        let db = Database::open_in_memory().expect("db");
        let router = ToolRouter::with_defaults(db.clone(), Notifier::default());

        // Notification may no-op without a D-Bus daemon; the DB write must succeed.
        let call = ToolCall::parse("ADD_TASK>Comprar pão|2026-06-20");
        let summary = router.dispatch(call).await.expect("dispatch");

        assert!(summary.contains("Comprar pão"));
        assert_eq!(db.count_tasks().expect("count"), 1);
    }

    #[tokio::test]
    async fn unknown_command_is_non_fatal() {
        let db = Database::open_in_memory().expect("db");
        let router = ToolRouter::with_defaults(db, Notifier::default());
        let out = router.dispatch(ToolCall::parse("NOPE>x")).await.expect("ok");
        assert!(out.contains("desconhecido"));
    }
}
