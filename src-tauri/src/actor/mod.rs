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

mod sentence_buffer;
mod stream_filter;

pub use sentence_buffer::SentenceBuffer;
pub use stream_filter::StreamFilter;

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;

use crate::core::AppError;
use crate::domain::TaskId;
use crate::infrastructure::llm::InferenceEngine;
use crate::infrastructure::{Notifier, Speaker};
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
    Alert,
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

/// Dependencies handed to the actor worker.
pub struct ActorDeps {
    pub router: ToolRouter,
    pub assistant: AssistantService,
    pub notifier: Notifier,
    /// Voice output sink (no-op unless built with `--features tts`).
    pub speaker: Speaker,
    /// Proactive heartbeat interval.
    pub heartbeat: Duration,
    /// When true, the heartbeat suppresses proactive nudges (tray "Pausar Alertas").
    pub alerts_paused: Arc<AtomicBool>,
}

/// Spawn the persistent worker and return a handle to it.
pub fn spawn(app: AppHandle, mut engine: Box<dyn InferenceEngine>, deps: ActorDeps) -> LlmHandle {
    let (tx, mut rx) = mpsc::channel::<LlmCommand>(32);

    tauri::async_runtime::spawn(async move {
        // Short-term memory + which deadlines were already nudged this session.
        let mut history = History::new(HISTORY_TURNS);
        let mut nudged: HashSet<TaskId> = HashSet::new();

        let mut ticker = tokio::time::interval(deps.heartbeat);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // The first tick fires immediately; consume it so we don't nudge at boot.
        ticker.tick().await;

        tracing::info!(model = engine.model_id(), "llm actor started");

        loop {
            tokio::select! {
                cmd = rx.recv() => {
                    match cmd {
                        Some(LlmCommand::Generate { user_input }) => {
                            run_generation(
                                &app,
                                engine.as_mut(),
                                &deps.router,
                                &deps.assistant,
                                &deps.speaker,
                                &mut history,
                                user_input,
                            )
                            .await;
                        }
                        Some(LlmCommand::Shutdown) | None => break,
                    }
                }
                _ = ticker.tick() => {
                    heartbeat(&app, engine.as_mut(), &deps, &mut history, &mut nudged).await;
                }
            }
        }
        tracing::info!("llm actor stopped");
    });

    LlmHandle { tx }
}

/// Proactive tick: find tasks due today (not yet nudged), alert the user via a
/// native notification + the `alert` sprite, and have Kensho generate a nudge.
async fn heartbeat(
    app: &AppHandle,
    engine: &mut dyn InferenceEngine,
    deps: &ActorDeps,
    history: &mut History,
    nudged: &mut HashSet<TaskId>,
) {
    // Tray "Pausar Alertas" suppresses proactive nudges (actor keeps running).
    if deps.alerts_paused.load(Ordering::Relaxed) {
        return;
    }

    let assistant = deps.assistant.clone();
    let due = match tokio::task::spawn_blocking(move || assistant.pending_due_today()).await {
        Ok(Ok(tasks)) => tasks,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "heartbeat query failed");
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "heartbeat join failed");
            return;
        }
    };

    // Only nudge each task once per session.
    let fresh: Vec<_> = due.into_iter().filter(|t| !nudged.contains(&t.id)).collect();
    if fresh.is_empty() {
        return;
    }
    for t in &fresh {
        nudged.insert(t.id);
    }
    tracing::info!(count = fresh.len(), "heartbeat: deadlines due today");

    // Native Ubuntu notification + alert sprite.
    let titles: Vec<String> = fresh.iter().take(5).map(|t| t.title.clone()).collect();
    let body = format!("{} tarefa(s) vencendo hoje: {}", fresh.len(), titles.join("; "));
    let notifier = deps.notifier.clone();
    let _ = tokio::task::spawn_blocking(move || notifier.notify("Kensho — Lembrete", &body)).await;
    emit(app, EVENT_STATE, StatePayload { state: CharacterState::Alert });

    // Synthetic, invisible system turn → Kensho generates the reminder text.
    if let Some(prompt) = AssistantService::deadline_reminder_prompt(&fresh) {
        run_generation(
            app,
            engine,
            &deps.router,
            &deps.assistant,
            &deps.speaker,
            history,
            prompt,
        )
        .await;
    }
}

/// Drive one request: Thinking → stream tokens (Speaking) → done/error → Idle.
async fn run_generation(
    app: &AppHandle,
    engine: &mut dyn InferenceEngine,
    router: &ToolRouter,
    assistant: &AssistantService,
    speaker: &Speaker,
    history: &mut History,
    user_input: String,
) {
    // Record the user turn.
    history.push_user(user_input);

    // Accumulated visible text across all passes. Tools (READ_FILE success, or a
    // failed CMD) inject follow-up context that drives another silent inference
    // cycle — autonomous multi-step "chain of thought" — hard-capped.
    let mut transcript = String::new();
    let mut pass: u8 = 0;

    loop {
        emit(app, EVENT_STATE, StatePayload { state: CharacterState::Thinking });

        let system = assistant.system_prompt().unwrap_or_default();
        let prompt = conversation::to_chatml(&system, history);

        let (token_tx, mut token_rx) = mpsc::channel::<String>(64);

        // Reader task: filters tool calls out of the stream, forwards visible
        // text to the UI, executes detected tools, and collects follow-up
        // context any tool wants injected back into the conversation.
        let app_reader = app.clone();
        let router_reader = router.clone();
        let speaker_reader = speaker.clone();
        let reader = tauri::async_runtime::spawn(async move {
            let mut filter = StreamFilter::new();
            let mut sentences = SentenceBuffer::new();
            let mut spoke = false;
            let mut follow_ups: Vec<String> = Vec::new();

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
                    emit(&app_reader, EVENT_TOKEN, TokenPayload { token: text.clone() });
                    // Voice complete sentences as they form (parallel with UI).
                    for sentence in sentences.push(&text) {
                        speaker_reader.speak(sentence);
                    }
                }

                for body in calls {
                    let call = ToolCall::parse(&body);
                    match router_reader.dispatch(call).await {
                        Ok(outcome) => {
                            emit(
                                &app_reader,
                                EVENT_TOOL,
                                ToolPayload { summary: outcome.summary },
                            );
                            if let Some(ctx) = outcome.follow_up {
                                follow_ups.push(ctx);
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "tool execution failed");
                            emit(
                                &app_reader,
                                EVENT_TOOL,
                                ToolPayload { summary: format!("Falha ao executar comando: {e}") },
                            );
                            // Feed the failure back so the model can self-correct
                            // (chain-of-thought) on the next cycle.
                            follow_ups.push(format!(
                                "A última ferramenta falhou: {e}. \
                                 Analise o erro e tente uma abordagem alternativa, \
                                 ou explique ao usuário se não for possível."
                            ));
                        }
                    }
                }
            }

            // Flush trailing visible text held back for partial-tag detection.
            let tail = filter.finish();
            if !tail.is_empty() {
                emit(&app_reader, EVENT_TOKEN, TokenPayload { token: tail.clone() });
                for sentence in sentences.push(&tail) {
                    speaker_reader.speak(sentence);
                }
            }
            // Speak any trailing unterminated text.
            if let Some(rest) = sentences.flush() {
                speaker_reader.speak(rest);
            }
            (filter.visible_text().to_owned(), follow_ups)
        });

        // Engine owns `token_tx`; when it returns, the channel closes and the
        // reader loop terminates.
        let result = engine.generate(&prompt, token_tx).await;
        let (full_text, follow_ups) = reader.await.unwrap_or_default();

        if let Err(e) = result {
            emit(app, EVENT_ERROR, ErrorPayload { message: e.to_string() });
            break;
        }

        // Persist the assistant turn into short-term memory for continuity.
        history.push_assistant(full_text.clone());
        transcript.push_str(&full_text);

        // A tool injected context (file content, or an error to recover from):
        // push it and run another silent cycle — until resolved or capped.
        if !follow_ups.is_empty() && allow_another_cycle(pass) {
            pass += 1;
            for ctx in follow_ups {
                history.push_user(ctx);
            }
            continue;
        }

        emit(app, EVENT_DONE, DonePayload { full_text: transcript.clone() });
        break;
    }

    emit(app, EVENT_STATE, StatePayload { state: CharacterState::Idle });
}

/// Hard ceiling on inference cycles per user turn (initial + tool-driven
/// follow-ups). Prevents runaway autonomous loops.
const MAX_TOOL_CYCLES: u8 = 3;

/// Whether another tool-driven cycle is allowed given the current pass index.
fn allow_another_cycle(pass: u8) -> bool {
    pass + 1 < MAX_TOOL_CYCLES
}

fn emit<P: Serialize + Clone>(app: &AppHandle, event: &str, payload: P) {
    if let Err(e) = app.emit(event, payload) {
        tracing::warn!(event, error = %e, "failed to emit tauri event");
    }
}

#[cfg(test)]
mod tests {
    use super::{allow_another_cycle, MAX_TOOL_CYCLES};

    #[test]
    fn multi_step_loop_is_hard_capped() {
        assert_eq!(MAX_TOOL_CYCLES, 3);
        // Passes 0 and 1 may spawn another cycle; pass 2 is the last → stop.
        assert!(allow_another_cycle(0));
        assert!(allow_another_cycle(1));
        assert!(!allow_another_cycle(2));
        assert!(!allow_another_cycle(3));
    }
}
