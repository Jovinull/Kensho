//! The LLM worker actor — the heart of the concurrency model.
//!
//! A single long-lived task owns the (potentially blocking, multi-GB) inference
//! engine. Tauri commands never touch the engine directly; they only send a
//! message down an `mpsc` channel via [`LlmHandle`]. The actor streams tokens
//! back through an [`EventSink`] (Tauri events in production, a recorder in
//! tests), so the worker is fully decoupled from Tauri and headless-testable.
//!
//! The reader half runs a [`StreamFilter`] that strips machine tool-call syntax
//! (`<CALL:…>`) out of the visible stream, executes the tool, and — when a tool
//! injects follow-up context (file content, or an error to recover from) — runs
//! another silent inference cycle (autonomous multi-step), hard-capped.

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
use crate::domain::{ChatMessage, Role, TaskId};
use crate::infrastructure::llm::InferenceEngine;
use crate::infrastructure::{Notifier, Speaker};
use crate::services::{AssistantService, History, ToolCall, ToolRouter};

// Event names shared with the frontend (`src/main.ts`).
pub const EVENT_STATE: &str = "character://state";
pub const EVENT_TOKEN: &str = "llm://token";
pub const EVENT_DONE: &str = "llm://done";
pub const EVENT_ERROR: &str = "llm://error";
pub const EVENT_TOOL: &str = "tool://executed";

/// Visual state of the character, mirrored 1:1 on the frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

/// Sink for actor-emitted events. Decouples the worker from Tauri so the whole
/// generation loop can be exercised headless with a recording sink.
pub trait EventSink: Clone + Send + Sync + 'static {
    fn state(&self, state: CharacterState);
    fn token(&self, token: &str);
    fn tool(&self, summary: &str);
    fn done(&self, full_text: &str);
    fn error(&self, message: &str);
}

/// Production sink: forwards to Tauri events.
#[derive(Clone)]
pub struct TauriSink {
    app: AppHandle,
}

impl TauriSink {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }

    fn emit<P: Serialize + Clone>(&self, event: &str, payload: P) {
        if let Err(e) = self.app.emit(event, payload) {
            tracing::warn!(event, error = %e, "failed to emit tauri event");
        }
    }
}

impl EventSink for TauriSink {
    fn state(&self, state: CharacterState) {
        self.emit(EVENT_STATE, StatePayload { state });
    }
    fn token(&self, token: &str) {
        self.emit(EVENT_TOKEN, TokenPayload { token: token.to_string() });
    }
    fn tool(&self, summary: &str) {
        self.emit(EVENT_TOOL, ToolPayload { summary: summary.to_string() });
    }
    fn done(&self, full_text: &str) {
        self.emit(EVENT_DONE, DonePayload { full_text: full_text.to_string() });
    }
    fn error(&self, message: &str) {
        self.emit(EVENT_ERROR, ErrorPayload { message: message.to_string() });
    }
}

/// Messages accepted by the actor.
pub enum LlmCommand {
    /// Raw user input; the actor wraps it in history + chat template.
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

/// Hard ceiling on inference cycles per user turn (initial + tool-driven
/// follow-ups). Prevents runaway autonomous loops.
const MAX_TOOL_CYCLES: u8 = 3;

/// Whether another tool-driven cycle is allowed given the current pass index.
fn allow_another_cycle(pass: u8) -> bool {
    pass + 1 < MAX_TOOL_CYCLES
}

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
    let sink = TauriSink::new(app);

    tauri::async_runtime::spawn(async move {
        let mut history = History::new(HISTORY_TURNS);
        let mut nudged: HashSet<TaskId> = HashSet::new();

        let mut ticker = tokio::time::interval(deps.heartbeat);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await; // consume the immediate first tick

        tracing::info!(model = engine.model_id(), "llm actor started");

        loop {
            tokio::select! {
                cmd = rx.recv() => {
                    match cmd {
                        Some(LlmCommand::Generate { user_input }) => {
                            run_generation(
                                &sink,
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
                    heartbeat(&sink, engine.as_mut(), &deps, &mut history, &mut nudged).await;
                }
            }
        }
        tracing::info!("llm actor stopped");
    });

    LlmHandle { tx }
}

/// Proactive tick: find tasks due today (not yet nudged), alert via a native
/// notification + the `alert` sprite, and have Kensho generate a nudge.
async fn heartbeat<S: EventSink>(
    sink: &S,
    engine: &mut dyn InferenceEngine,
    deps: &ActorDeps,
    history: &mut History,
    nudged: &mut HashSet<TaskId>,
) {
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

    let fresh: Vec<_> = due.into_iter().filter(|t| !nudged.contains(&t.id)).collect();
    if fresh.is_empty() {
        return;
    }
    for t in &fresh {
        nudged.insert(t.id);
    }
    tracing::info!(count = fresh.len(), "heartbeat: deadlines due today");

    let titles: Vec<String> = fresh.iter().take(5).map(|t| t.title.clone()).collect();
    let body = format!("{} tarefa(s) vencendo hoje: {}", fresh.len(), titles.join("; "));
    let notifier = deps.notifier.clone();
    let _ = tokio::task::spawn_blocking(move || notifier.notify("Kensho — Lembrete", &body)).await;
    sink.state(CharacterState::Alert);

    if let Some(prompt) = AssistantService::deadline_reminder_prompt(&fresh) {
        run_generation(
            sink,
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

/// Drive one request: Thinking → stream tokens (Speaking) → done/error → Idle,
/// with autonomous multi-step cycles when tools inject follow-up context.
async fn run_generation<S: EventSink>(
    sink: &S,
    engine: &mut dyn InferenceEngine,
    router: &ToolRouter,
    assistant: &AssistantService,
    speaker: &Speaker,
    history: &mut History,
    user_input: String,
) {
    history.push_user(user_input);

    let mut transcript = String::new();
    let mut pass: u8 = 0;

    loop {
        sink.state(CharacterState::Thinking);

        // Build the conversation: live system prompt + rolling history. The
        // engine applies the model's native chat template (model-agnostic).
        let system = assistant.system_prompt().unwrap_or_default();
        let mut messages: Vec<ChatMessage> = Vec::with_capacity(history.len() + 1);
        messages.push(ChatMessage::new(Role::System, system));
        messages.extend(history.iter().cloned());

        let (token_tx, mut token_rx) = mpsc::channel::<String>(64);

        // Reader: filter tool calls out of the visible stream, voice sentences,
        // execute tools, and collect follow-up context.
        let sink_reader = sink.clone();
        let router_reader = router.clone();
        let speaker_reader = speaker.clone();
        let reader = tokio::spawn(async move {
            let mut filter = StreamFilter::new();
            let mut sentences = SentenceBuffer::new();
            let mut spoke = false;
            let mut follow_ups: Vec<String> = Vec::new();

            while let Some(tok) = token_rx.recv().await {
                let (text, calls) = filter.push(&tok);

                if !text.is_empty() {
                    if !spoke {
                        sink_reader.state(CharacterState::Speaking);
                        spoke = true;
                    }
                    sink_reader.token(&text);
                    for sentence in sentences.push(&text) {
                        speaker_reader.speak(sentence);
                    }
                }

                for body in calls {
                    let call = ToolCall::parse(&body);
                    match router_reader.dispatch(call).await {
                        Ok(outcome) => {
                            sink_reader.tool(&outcome.summary);
                            if let Some(ctx) = outcome.follow_up {
                                follow_ups.push(ctx);
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "tool execution failed");
                            sink_reader.tool(&format!("Falha ao executar comando: {e}"));
                            // Feed the failure back so the model can self-correct.
                            follow_ups.push(format!(
                                "A última ferramenta falhou: {e}. \
                                 Analise o erro e tente uma abordagem alternativa, \
                                 ou explique ao usuário se não for possível."
                            ));
                        }
                    }
                }
            }

            let tail = filter.finish();
            if !tail.is_empty() {
                sink_reader.token(&tail);
                for sentence in sentences.push(&tail) {
                    speaker_reader.speak(sentence);
                }
            }
            if let Some(rest) = sentences.flush() {
                speaker_reader.speak(rest);
            }
            (filter.visible_text().to_owned(), follow_ups)
        });

        // Engine owns `token_tx`; when it returns the channel closes and the
        // reader loop terminates.
        let result = engine.generate(&messages, token_tx).await;
        let (full_text, follow_ups) = reader.await.unwrap_or_default();

        if let Err(e) = result {
            sink.error(&e.to_string());
            break;
        }

        history.push_assistant(full_text.clone());
        transcript.push_str(&full_text);

        if !follow_ups.is_empty() && allow_another_cycle(pass) {
            pass += 1;
            for ctx in follow_ups {
                history.push_user(ctx);
            }
            continue;
        }

        sink.done(&transcript);
        break;
    }

    sink.state(CharacterState::Idle);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::llm::{LlmError, TokenSink};
    use crate::infrastructure::{Database, Notifier};
    use crate::services::approval::AlwaysApprove;
    use crate::services::clipboard::ClipboardContext;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[test]
    fn multi_step_loop_is_hard_capped() {
        assert_eq!(MAX_TOOL_CYCLES, 3);
        assert!(allow_another_cycle(0));
        assert!(allow_another_cycle(1));
        assert!(!allow_another_cycle(2));
        assert!(!allow_another_cycle(3));
    }

    /// Engine that replays scripted outputs, one per `generate` call.
    struct ScriptedEngine {
        scripts: VecDeque<String>,
        calls: usize,
        id: String,
    }

    impl ScriptedEngine {
        fn new(scripts: Vec<&str>) -> Self {
            Self {
                scripts: scripts.into_iter().map(String::from).collect(),
                calls: 0,
                id: "scripted".to_string(),
            }
        }
    }

    #[async_trait::async_trait]
    impl InferenceEngine for ScriptedEngine {
        fn model_id(&self) -> &str {
            &self.id
        }
        async fn generate(
            &mut self,
            _messages: &[ChatMessage],
            sink: TokenSink,
        ) -> Result<(), LlmError> {
            self.calls += 1;
            let script = self
                .scripts
                .pop_front()
                .unwrap_or_else(|| "Não sei mais o que fazer.".to_string());
            for tok in script.split_inclusive(' ') {
                if sink.send(tok.to_string()).await.is_err() {
                    break;
                }
            }
            Ok(())
        }
    }

    /// Sink that records everything the actor emits.
    #[derive(Clone, Default)]
    struct RecordingSink {
        inner: Arc<Mutex<Recorded>>,
    }
    #[derive(Default)]
    struct Recorded {
        tokens: String,
        done: Option<String>,
        tools: Vec<String>,
        states: Vec<CharacterState>,
    }

    impl EventSink for RecordingSink {
        fn state(&self, state: CharacterState) {
            self.inner.lock().unwrap().states.push(state);
        }
        fn token(&self, token: &str) {
            self.inner.lock().unwrap().tokens.push_str(token);
        }
        fn tool(&self, summary: &str) {
            self.inner.lock().unwrap().tools.push(summary.to_string());
        }
        fn done(&self, full_text: &str) {
            self.inner.lock().unwrap().done = Some(full_text.to_string());
        }
        fn error(&self, _message: &str) {}
    }

    fn harness(
        scripts: Vec<&str>,
    ) -> (RecordingSink, ScriptedEngine, ToolRouter, AssistantService, Speaker, History) {
        let db = Database::open_in_memory().expect("db");
        let router =
            ToolRouter::with_defaults(db.clone(), Notifier::default(), Arc::new(AlwaysApprove));
        let assistant = AssistantService::new(db, ClipboardContext::new());
        (
            RecordingSink::default(),
            ScriptedEngine::new(scripts),
            router,
            assistant,
            Speaker::disabled(),
            History::new(HISTORY_TURNS),
        )
    }

    #[tokio::test]
    async fn multi_step_recovers_from_tool_failure() {
        // Cycle 1 calls a missing file (fails → re-injected); cycle 2 answers.
        let (sink, mut engine, router, assistant, speaker, mut history) = harness(vec![
            "Vou verificar. <CALL:READ_FILE>/nonexistent/kensho_xyz.log</CALL>",
            "Pronto, resolvido.",
        ]);

        run_generation(
            &sink,
            &mut engine,
            &router,
            &assistant,
            &speaker,
            &mut history,
            "Leia o log e me explique.".to_string(),
        )
        .await;

        assert_eq!(engine.calls, 2, "should self-correct over 2 cycles");
        let rec = sink.inner.lock().unwrap();
        assert_eq!(rec.done.as_deref(), Some("Vou verificar. Pronto, resolvido."));
        assert!(rec.states.contains(&CharacterState::Idle));
    }

    #[tokio::test]
    async fn multi_step_stops_at_hard_cap() {
        // Every cycle fails → must stop at MAX_TOOL_CYCLES, never loop forever.
        let failing = "<CALL:READ_FILE>/nope/missing.txt</CALL>";
        let (sink, mut engine, router, assistant, speaker, mut history) =
            harness(vec![failing, failing, failing, failing, failing]);

        run_generation(
            &sink,
            &mut engine,
            &router,
            &assistant,
            &speaker,
            &mut history,
            "tente ler".to_string(),
        )
        .await;

        assert_eq!(engine.calls, MAX_TOOL_CYCLES as usize, "capped at 3 cycles");
        assert!(sink.inner.lock().unwrap().done.is_some(), "still emits a final answer");
    }
}
