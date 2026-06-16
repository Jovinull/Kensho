//! Transport layer: the only place Tauri types meet the backend.
//!
//! Commands stay thin — they validate/forward and return frontend-safe errors.
//! Anything potentially blocking (DB, inference) is pushed to the actor or a
//! blocking thread so the UI never stalls.

use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State, Window};

use crate::actor::LlmHandle;
use crate::core::CommandError;
use crate::domain::Task;
use crate::infrastructure::{Database, Notifier};
use crate::services::approval::{ApprovalGate, PendingApprovals};

/// Forward raw user input to the LLM actor. Returns immediately; the actor wraps
/// it in ChatML + rolling history and streams the answer back via the
/// `llm://token` / `llm://done` events.
#[tauri::command]
pub async fn ask_assistant(
    prompt: String,
    handle: State<'_, LlmHandle>,
) -> Result<(), CommandError> {
    handle.generate(prompt).await?;
    Ok(())
}

/// Create and persist a new task.
#[tauri::command]
pub async fn create_task(
    title: String,
    db: State<'_, Database>,
) -> Result<Task, CommandError> {
    let task = Task::new(title);
    let db = db.inner().clone();
    let to_store = task.clone();
    let join = tauri::async_runtime::spawn_blocking(move || db.insert_task(&to_store)).await;
    join.map_err(join_err)??;
    Ok::<_, CommandError>(task)
}

/// List all persisted tasks (highest priority first).
#[tauri::command]
pub async fn list_tasks(db: State<'_, Database>) -> Result<Vec<Task>, CommandError> {
    let db = db.inner().clone();
    let join = tauri::async_runtime::spawn_blocking(move || db.list_tasks()).await;
    let tasks = join.map_err(join_err)??;
    Ok(tasks)
}

/// Fire a native Ubuntu desktop notification.
#[tauri::command]
pub fn send_notification(
    summary: String,
    body: String,
    notifier: State<'_, Notifier>,
) -> Result<(), CommandError> {
    notifier.notify(&summary, &body)?;
    Ok(())
}

/// Toggle the floating character's always-on-top behavior at runtime.
#[tauri::command]
pub fn set_always_on_top(window: Window, on: bool) -> Result<(), CommandError> {
    window.set_always_on_top(on).map_err(|e| CommandError {
        message: e.to_string(),
        kind: "window".to_string(),
    })?;
    Ok(())
}

#[derive(Serialize)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
}

/// Basic app metadata for the frontend.
#[tauri::command]
pub fn app_info() -> AppInfo {
    AppInfo {
        name: "Kensho".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// Map a blocking-join failure into a frontend-safe error. Generic over the
/// runtime's join-error type so we don't depend on its exact path.
fn join_err<E: std::fmt::Display>(e: E) -> CommandError {
    CommandError {
        message: e.to_string(),
        kind: "join".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Human-in-the-loop approval (production gate + resolver command)
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
struct ApprovalRequest {
    id: String,
    command: String,
}

/// Production [`ApprovalGate`]: emits `ui://require-approval` and suspends the
/// tool until the frontend answers via `approve_action` (60s → auto-deny).
pub struct TauriApprovalGate {
    app: AppHandle,
    pending: PendingApprovals,
}

impl TauriApprovalGate {
    pub fn new(app: AppHandle, pending: PendingApprovals) -> Self {
        Self { app, pending }
    }
}

#[async_trait]
impl ApprovalGate for TauriApprovalGate {
    async fn request(&self, command: &str) -> bool {
        let id = uuid::Uuid::new_v4().to_string();
        let rx = self.pending.register(id.clone());
        let _ = self.app.emit(
            "ui://require-approval",
            ApprovalRequest {
                id: id.clone(),
                command: command.to_string(),
            },
        );
        match tokio::time::timeout(Duration::from_secs(60), rx).await {
            Ok(Ok(approved)) => approved,
            _ => {
                // Timeout or dropped channel → deny and clean up.
                self.pending.resolve(&id, false);
                false
            }
        }
    }
}

/// Frontend's answer to a pending `ui://require-approval` request.
#[tauri::command]
pub fn approve_action(
    id: String,
    approved: bool,
    pending: State<'_, PendingApprovals>,
) -> Result<(), CommandError> {
    pending.resolve(&id, approved);
    Ok(())
}
