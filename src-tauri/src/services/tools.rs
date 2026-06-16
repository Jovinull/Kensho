//! Tool calling — extensible router.
//!
//! Small local models struggle with strict JSON, so Kensho uses a robust tag
//! protocol the model emits inline in its stream, e.g.:
//!
//!   `<CALL:ADD_TASK>Título|2026-06-20</CALL>`
//!   `<CALL:DELEGATE>Rafaela|Corrigir bug no login</CALL>`
//!   `<CALL:READ_FILE>/var/log/app/error.log</CALL>`
//!
//! Architecture (kept clean for a future MCP / open-orchestration backend):
//!   * the LLM emits *intent* (the tag),
//!   * the actor's stream filter intercepts it and hands the body here,
//!   * a [`ToolRouter`] dispatches to a registered [`Tool`] by name,
//!   * the tool executes and returns a [`ToolOutcome`] (a UI summary, plus an
//!     optional context blob to inject back into the conversation).
//!
//! Adding a capability = implement [`Tool`] and `register` it. Nothing else
//! (actor, parser, prompt plumbing) changes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{NaiveDate, TimeZone, Utc};

use crate::core::{AppError, AppResult};
use crate::domain::{DelegatedTask, Task};
use crate::infrastructure::{Database, Notifier};

/// Hardcoded dev team valid as delegation targets (MVP).
pub const TEAM: [&str; 3] = ["Waldston", "Joãozinho", "Rafaela"];

/// First/last N lines kept when reading a file (context-window safety).
const FILE_EDGE_LINES: usize = 100;
/// Hard cap on bytes read from a file.
const FILE_MAX_BYTES: u64 = 1_000_000;

/// Result of executing a tool.
#[derive(Debug, Clone)]
pub struct ToolOutcome {
    /// Short, user-facing confirmation shown as a toast (e.g. "Delegado para Rafaela").
    pub summary: String,
    /// Optional content to inject into the rolling window, forcing the model to
    /// respond over it (used by READ_FILE).
    pub follow_up: Option<String>,
}

impl ToolOutcome {
    pub fn summary(s: impl Into<String>) -> Self {
        Self {
            summary: s.into(),
            follow_up: None,
        }
    }

    pub fn with_follow_up(summary: impl Into<String>, follow_up: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            follow_up: Some(follow_up.into()),
        }
    }
}

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

    /// Execute with the raw argument string.
    async fn execute(&self, raw_args: &str) -> AppResult<ToolOutcome>;
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

    /// Build the default router with all built-in capabilities.
    pub fn with_defaults(db: Database, notifier: Notifier) -> Self {
        let mut router = Self::new();

        // Shared HTTP client for outbound webhooks (5s timeout, rustls).
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        let webhook_url = std::env::var("KENSHO_TEAM_WEBHOOK_URL")
            .ok()
            .filter(|s| !s.trim().is_empty());

        router.register(Arc::new(AddTaskTool {
            db: db.clone(),
            notifier: notifier.clone(),
        }));
        router.register(Arc::new(DelegateTaskTool {
            db,
            notifier,
            http,
            webhook_url,
        }));
        router.register(Arc::new(ReadLocalFileTool));
        router.register(Arc::new(ShellCommandTool));
        router
    }

    /// Intercept + execute. Unknown commands are logged, not fatal.
    pub async fn dispatch(&self, call: ToolCall) -> AppResult<ToolOutcome> {
        match self.tools.get(&call.name) {
            Some(tool) => {
                let outcome = tool.execute(&call.raw_args).await?;
                tracing::info!(command = %call.name, summary = %outcome.summary, "tool executed");
                Ok(outcome)
            }
            None => {
                tracing::warn!(command = %call.name, "unknown tool call ignored");
                Ok(ToolOutcome::summary(format!(
                    "Comando desconhecido: {}",
                    call.name
                )))
            }
        }
    }
}

impl Default for ToolRouter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Built-in tools
// ---------------------------------------------------------------------------

/// Persist a personal task + fire a native notification.
struct AddTaskTool {
    db: Database,
    notifier: Notifier,
}

#[async_trait]
impl Tool for AddTaskTool {
    fn name(&self) -> &str {
        "ADD_TASK"
    }

    async fn execute(&self, raw_args: &str) -> AppResult<ToolOutcome> {
        let mut parts = raw_args.splitn(2, '|');
        let title = parts.next().unwrap_or("").trim().to_string();
        let date = parts.next().map(str::trim).filter(|s| !s.is_empty());

        if title.is_empty() {
            return Ok(ToolOutcome::summary("Tarefa sem título — ignorada."));
        }

        let mut task = Task::new(title.clone());
        if let Some(d) = date {
            if let Ok(nd) = NaiveDate::parse_from_str(d, "%Y-%m-%d") {
                if let Some(naive) = nd.and_hms_opt(9, 0, 0) {
                    task.due_at = Some(Utc.from_utc_datetime(&naive));
                }
            }
        }

        let db = self.db.clone();
        let to_store = task.clone();
        tokio::task::spawn_blocking(move || db.insert_task(&to_store))
            .await
            .map_err(join_err)??;

        notify(&self.notifier, format!("Tarefa adicionada: {title}")).await;
        Ok(ToolOutcome::summary(format!("Tarefa adicionada: {title}")))
    }
}

/// Delegate a ticket to a known team member (agile-board style) and, if a team
/// webhook is configured, notify them externally over HTTP.
struct DelegateTaskTool {
    db: Database,
    notifier: Notifier,
    http: reqwest::Client,
    /// `KENSHO_TEAM_WEBHOOK_URL` (Slack/Discord-style endpoint), if set.
    webhook_url: Option<String>,
}

#[async_trait]
impl Tool for DelegateTaskTool {
    fn name(&self) -> &str {
        "DELEGATE"
    }

    async fn execute(&self, raw_args: &str) -> AppResult<ToolOutcome> {
        let mut parts = raw_args.splitn(2, '|');
        let assignee_raw = parts.next().unwrap_or("").trim();
        let description = parts.next().unwrap_or("").trim().to_string();

        if assignee_raw.is_empty() || description.is_empty() {
            return Ok(ToolOutcome::summary(
                "Delegação inválida (faltou alvo ou descrição).",
            ));
        }

        // Validate against the team, normalizing to the canonical name.
        let assignee = match TEAM.iter().find(|m| m.eq_ignore_ascii_case(assignee_raw)) {
            Some(m) => (*m).to_string(),
            None => {
                return Ok(ToolOutcome::summary(format!(
                    "Alvo desconhecido: {assignee_raw}. Equipe: {}.",
                    TEAM.join(", ")
                )));
            }
        };

        let ticket = DelegatedTask::new(assignee.clone(), description);
        let db = self.db.clone();
        let to_store = ticket.clone();
        tokio::task::spawn_blocking(move || db.insert_delegated_task(&to_store))
            .await
            .map_err(join_err)??;

        notify(&self.notifier, format!("Tarefa delegada para {assignee}")).await;

        let mut summary = format!("Delegado para {assignee}");

        // Best-effort external notification. Network failure is non-fatal: the
        // ticket is already persisted; we just annotate the summary.
        if let Some(url) = &self.webhook_url {
            let payload = serde_json::json!({
                "text": format!("📋 Nova tarefa para {}: {}", assignee, ticket.description),
                "assignee": assignee,
                "description": ticket.description,
            });
            match self.http.post(url).json(&payload).send().await {
                Ok(resp) if resp.status().is_success() => {
                    tracing::info!(%assignee, "team webhook notified");
                }
                Ok(resp) => {
                    tracing::warn!(status = %resp.status(), "team webhook non-2xx");
                    summary.push_str(" (notificação de rede falhou)");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "team webhook request failed");
                    summary.push_str(" (notificação de rede falhou)");
                }
            }
        }

        Ok(ToolOutcome::summary(summary))
    }
}

/// Run a shell command (`sh -c`) with a strict timeout, returning truncated
/// stdout/stderr as follow-up context for the model to analyze.
struct ShellCommandTool;

const SHELL_TIMEOUT_SECS: u64 = 5;
const SHELL_OUTPUT_MAX_CHARS: usize = 2000;

#[async_trait]
impl Tool for ShellCommandTool {
    fn name(&self) -> &str {
        "CMD"
    }

    async fn execute(&self, raw_args: &str) -> AppResult<ToolOutcome> {
        let cmd = raw_args.trim().to_string();
        if cmd.is_empty() {
            return Ok(ToolOutcome::summary("Comando vazio — ignorado."));
        }

        // `kill_on_drop` ensures the child dies if the timeout drops the future.
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg(&cmd).kill_on_drop(true);

        let combined = match tokio::time::timeout(
            Duration::from_secs(SHELL_TIMEOUT_SECS),
            command.output(),
        )
        .await
        {
            Ok(Ok(out)) => format!(
                "[exit {}]\n[stdout]\n{}\n[stderr]\n{}",
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stdout).trim_end(),
                String::from_utf8_lossy(&out.stderr).trim_end(),
            ),
            Ok(Err(e)) => format!("[erro ao executar: {e}]"),
            Err(_) => format!("[timeout após {SHELL_TIMEOUT_SECS}s — processo encerrado]"),
        };

        let truncated = tail_chars(&combined, SHELL_OUTPUT_MAX_CHARS);
        let injected = format!(
            "Saída do comando `{cmd}`:\n```\n{truncated}\n```\n\
             Analise o resultado e responda à solicitação do usuário.",
        );
        let short: String = cmd.chars().take(40).collect();

        Ok(ToolOutcome::with_follow_up(
            format!("Executando: {short}"),
            injected,
        ))
    }
}

/// Read a local file (clamped) and inject its content into the conversation,
/// forcing the model to answer over it.
struct ReadLocalFileTool;

#[async_trait]
impl Tool for ReadLocalFileTool {
    fn name(&self) -> &str {
        "READ_FILE"
    }

    async fn execute(&self, raw_args: &str) -> AppResult<ToolOutcome> {
        let path = raw_args.trim().to_string();
        if path.is_empty() {
            return Ok(ToolOutcome::summary("Caminho de arquivo vazio — ignorado."));
        }

        let path_for_read = path.clone();
        let content = tokio::task::spawn_blocking(move || read_clamped(&path_for_read))
            .await
            .map_err(join_err)??;

        let filename = std::path::Path::new(&path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&path)
            .to_string();

        let injected = format!(
            "Conteúdo do arquivo `{path}` (até {FILE_EDGE_LINES} primeiras/últimas linhas):\n\
             ```\n{content}\n```\n\
             Analise esse conteúdo e responda à solicitação do usuário com base nele.",
        );

        Ok(ToolOutcome::with_follow_up(
            format!("Lendo {filename}"),
            injected,
        ))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn join_err(e: tokio::task::JoinError) -> AppError {
    AppError::Other(anyhow::anyhow!("join error: {e}"))
}

/// Keep only the last `max` characters (char-safe), prefixing `…` when cut.
fn tail_chars(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else {
        let tail: String = s.chars().skip(count - max).collect();
        format!("…{tail}")
    }
}

async fn notify(notifier: &Notifier, body: String) {
    let notifier = notifier.clone();
    let _ = tokio::task::spawn_blocking(move || notifier.notify("Kensho", &body)).await;
}

/// Read a file, returning at most the first + last `FILE_EDGE_LINES` lines and
/// never more than `FILE_MAX_BYTES` of input.
fn read_clamped(path: &str) -> AppResult<String> {
    use std::io::Read;

    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(AppError::Other(anyhow::anyhow!(
            "{path} não é um arquivo regular"
        )));
    }

    let mut buf = Vec::new();
    std::fs::File::open(path)?
        .take(FILE_MAX_BYTES)
        .read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let lines: Vec<&str> = text.lines().collect();

    if lines.len() <= FILE_EDGE_LINES * 2 {
        Ok(lines.join("\n"))
    } else {
        let head = lines[..FILE_EDGE_LINES].join("\n");
        let tail = lines[lines.len() - FILE_EDGE_LINES..].join("\n");
        let omitted = lines.len() - FILE_EDGE_LINES * 2;
        Ok(format!(
            "{head}\n... [{omitted} linhas omitidas] ...\n{tail}"
        ))
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
        let out = router
            .dispatch(ToolCall::parse("ADD_TASK>Comprar pão|2026-06-20"))
            .await
            .expect("dispatch");
        assert!(out.summary.contains("Comprar pão"));
        assert_eq!(db.count_tasks().expect("count"), 1);
    }

    #[tokio::test]
    async fn delegate_accepts_known_member_case_insensitive() {
        let db = Database::open_in_memory().expect("db");
        let router = ToolRouter::with_defaults(db.clone(), Notifier::default());
        let out = router
            .dispatch(ToolCall::parse("DELEGATE>rafaela|Corrigir bug no login"))
            .await
            .expect("dispatch");
        assert_eq!(out.summary, "Delegado para Rafaela"); // canonical-cased
        assert_eq!(db.count_delegated_tasks().expect("count"), 1);
    }

    #[tokio::test]
    async fn delegate_rejects_unknown_assignee() {
        let db = Database::open_in_memory().expect("db");
        let router = ToolRouter::with_defaults(db.clone(), Notifier::default());
        let out = router
            .dispatch(ToolCall::parse("DELEGATE>Fulano|qualquer coisa"))
            .await
            .expect("dispatch");
        assert!(out.summary.contains("desconhecido"));
        assert_eq!(db.count_delegated_tasks().expect("count"), 0);
    }

    #[tokio::test]
    async fn read_file_injects_follow_up_context() {
        let db = Database::open_in_memory().expect("db");
        let router = ToolRouter::with_defaults(db, Notifier::default());

        let path = std::env::temp_dir().join("kensho_read_test.txt");
        std::fs::write(&path, "linha1\nERRO: panic\nlinha3").expect("write");

        let out = router
            .dispatch(ToolCall::parse(&format!("READ_FILE>{}", path.display())))
            .await
            .expect("dispatch");

        assert!(out.summary.starts_with("Lendo"));
        let follow_up = out.follow_up.expect("follow-up context");
        assert!(follow_up.contains("ERRO: panic"));
    }

    #[tokio::test]
    async fn unknown_command_is_non_fatal() {
        let db = Database::open_in_memory().expect("db");
        let router = ToolRouter::with_defaults(db, Notifier::default());
        let out = router.dispatch(ToolCall::parse("NOPE>x")).await.expect("ok");
        assert!(out.summary.contains("desconhecido"));
    }

    #[tokio::test]
    async fn shell_runs_harmless_command() {
        let db = Database::open_in_memory().expect("db");
        let router = ToolRouter::with_defaults(db, Notifier::default());
        let out = router
            .dispatch(ToolCall::parse("CMD>echo kensho-test"))
            .await
            .expect("dispatch");
        assert!(out.summary.starts_with("Executando"));
        let follow_up = out.follow_up.expect("command output");
        assert!(follow_up.contains("kensho-test"));
    }

    #[tokio::test]
    async fn delegate_marks_network_failure_but_still_persists() {
        let db = Database::open_in_memory().expect("db");
        // Webhook pointed at a closed port → request fails fast (refused).
        let tool = DelegateTaskTool {
            db: db.clone(),
            notifier: Notifier::default(),
            http: reqwest::Client::new(),
            webhook_url: Some("http://127.0.0.1:9/webhook".to_string()),
        };
        let out = tool.execute("Rafaela|Corrigir bug").await.expect("execute");
        assert!(out.summary.contains("Rafaela"));
        assert!(out.summary.contains("rede")); // network-failure note
        assert_eq!(db.count_delegated_tasks().expect("count"), 1); // persisted anyway
    }
}
