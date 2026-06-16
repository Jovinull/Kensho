//! The LLM worker actor — the heart of the concurrency model.
//!
//! A single long-lived task owns the (potentially blocking, multi-GB) inference
//! engine. Tauri commands never touch the engine directly; they only send a
//! message down an `mpsc` channel via [`LlmHandle`]. The actor streams tokens
//! back to the UI as Tauri events, so the main/UI thread is never blocked and
//! the character animation keeps running at 60fps.
//!
//! The reader half also runs a [`StreamFilter`] that strips machine tool-call
//! syntax (`<CALL:…>`) out of the visible stream, executes the tool, fires a
//! native notification, then lets the natural text resume.

mod stream_filter;

pub use stream_filter::StreamFilter;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;

use crate::core::AppError;
use crate::infrastructure::llm::InferenceEngine;
use crate::services::{conversation, AssistantService, History, ToolCall, ToolRouter};

// Event names shared with the frontend (`src/main.ts`).
pub const EVENT_STATE: &str = "character://state";
pub const EVENT_TOKEN: &str = "llm://token";
pub const EVENT_DONE: &str = "llm://done";
pub const EVENT_ERROR: &str = "llm://error";
pub const EVENT_TOOL: &str = "tool://executed";

/// Visual state of the character, mirrored 1:1 on the frontend.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CharacterState {
    Idle,
    Thinking,
    Speaking,
}

#[derive(Serialize, Clone)]
struct StatePayload {
    state: CharacterState,
}
#[derive(Serialize, Clone)]
struct TokenPayload {
    token: String,
}
#[derive(Serialize, Clone)]
struct DonePayload {
    full_text: String,
}
#[derive(Serialize, Clone)]
struct ErrorPayload {
    message: String,
}
#[derive(Serialize, Clone)]
struct ToolPayload {
    summary: String,
}

/// Messages accepted by the actor.
pub enum LlmCommand {
    /// Raw user input; the actor wraps it in ChatML + history before inference.
    Generate { user_input: String },
    Shutdown,
}

/// Cheap, cloneable handle used by commands/services to talk to the actor.
#[derive(Clone)]
pub struct LlmHandle {
    tx: mpsc::Sender<LlmCommand>,
}

impl LlmHandle {
    /// Queue a generation request. Returns immediately (non-blocking).
    pub async fn generate(&self, user_input: String) -> Result<(), AppError> {
        self.tx
            .send(LlmCommand::Generate { user_input })
            .await
            .map_err(|_| AppError::WorkerUnavailable)
    }

    /// Ask the actor to stop after draining in-flight work.
    pub async fn shutdown(&self) {
        let _ = self.tx.send(LlmCommand::Shutdown).await;
    }
}

/// Number of user/assistant turns retained in the rolling memory window.
const HISTORY_TURNS: usize = 6;

/// Spawn the persistent worker and return a handle to it.
pub fn spawn(
    app: AppHandle,
    mut engine: Box<dyn InferenceEngine>,
    router: ToolRouter,
    assistant: AssistantService,
) -> LlmHandle {
    let (tx, mut rx) = mpsc::channel::<LlmCommand>(32);

    tauri::async_runtime::spawn(async move {
        // Short-term memory lives with the worker for the session's lifetime.
        let mut history = History::new(HISTORY_TURNS);
        tracing::info!(model = engine.model_id(), "llm actor started");

        while let Some(cmd) = rx.recv().await {
            match cmd {
                LlmCommand::Generate { user_input } => {
                    run_generation(
                        &app,
                        engine.as_mut(),
                        &router,
                        &assistant,
                        &mut history,
                        user_input,
                    )
                    .await;
                }
                LlmCommand::Shutdown => break,
            }
        }
        tracing::info!("llm actor stopped");
    });

    LlmHandle { tx }
}

/// Drive one request: Thinking → stream tokens (Speaking) → done/error → Idle.
async fn run_generation(
    app: &AppHandle,
    engine: &mut dyn InferenceEngine,
    router: &ToolRouter,
    assistant: &AssistantService,
    history: &mut History,
    user_input: String,
) {
    // Record the user turn, then build the ChatML prompt: system (live DB
    // context + tools) + rolling history (which now ends with this user turn).
    history.push_user(user_input);
    let system = assistant.system_prompt().unwrap_or_default();
    let prompt = conversation::to_chatml(&system, history);

    emit(app, EVENT_STATE, StatePayload { state: CharacterState::Thinking });

    let (token_tx, mut token_rx) = mpsc::channel::<String>(64);

    // Reader task: filters tool calls out of the stream, forwards visible text
    // to the UI, executes detected tools, and accumulates the visible text.
    let app_reader = app.clone();
    let router_reader = router.clone();
    let reader = tauri::async_runtime::spawn(async move {
        let mut filter = StreamFilter::new();
        let mut spoke = false;

        while let Some(tok) = token_rx.recv().await {
            let (text, calls) = filter.push(&tok);

            if !text.is_empty() {
                if !spoke {
                    emit(
                        &app_reader,
                        EVENT_STATE,
                        StatePayload { state: CharacterState::Speaking },
                    );
                    spoke = true;
                }
                emit(&app_reader, EVENT_TOKEN, TokenPayload { token: text });
            }

            for body in calls {
                let call = ToolCall::parse(&body);
                match router_reader.dispatch(call).await {
                    Ok(summary) => {
                        emit(&app_reader, EVENT_TOOL, ToolPayload { summary });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "tool execution failed");
                        emit(
                            &app_reader,
                            EVENT_TOOL,
                            ToolPayload { summary: format!("Falha ao executar comando: {e}") },
                        );
                    }
                }
            }
        }

        // Flush any trailing visible text held back for partial-tag detection.
        let tail = filter.finish();
        if !tail.is_empty() {
            emit(&app_reader, EVENT_TOKEN, TokenPayload { token: tail });
        }
        filter.visible_text().to_owned()
    });

    // Engine owns `token_tx`; when it returns, the channel closes and the
    // reader loop terminates.
    let result = engine.generate(&prompt, token_tx).await;
    let full_text = reader.await.unwrap_or_default();

    match result {
        Ok(()) => {
            // Persist the assistant turn into short-term memory for continuity.
            history.push_assistant(full_text.clone());
            emit(app, EVENT_DONE, DonePayload { full_text });
        }
        Err(e) => emit(app, EVENT_ERROR, ErrorPayload { message: e.to_string() }),
    }

    emit(app, EVENT_STATE, StatePayload { state: CharacterState::Idle });
}

fn emit<P: Serialize + Clone>(app: &AppHandle, event: &str, payload: P) {
    if let Err(e) = app.emit(event, payload) {
        tracing::warn!(event, error = %e, "failed to emit tauri event");
    }
}
