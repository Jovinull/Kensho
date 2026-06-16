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
use crate::domain::{DelegatedTask, KnowledgeNote, Task};
use crate::infrastructure::{Database, Notifier};
use crate::services::approval::ApprovalGate;

/// Hardcoded dev team valid as delegation targets (MVP).
pub const TEAM: [&str; 3] = ["Waldston", "Joãozinho", "Rafaela"];

/// First/last N lines kept when reading a file (context-window safety).
const FILE_EDGE_LINES: usize = 100;
/// Hard cap on bytes read from a file.
const FILE_MAX_BYTES: u64 = 1_000_000;

/// Command prefixes considered read-only/safe (run without approval).
const SAFE_PREFIXES: [&str; 14] = [
    "ls", "cat", "echo", "pwd", "whoami", "head", "tail", "grep", "find", "wc",
    "date", "git status", "git log", "git diff",
];
/// Shell metacharacters that force approval even with a safe prefix.
const DANGEROUS_CHARS: [char; 8] = [';', '&', '|', '>', '<', '`', '\n', '$'];

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
    /// Parse the captured body, e.g. `ADD_TASK>Comprar pão|2026-06-20` or the
    /// fuzzy bracket form `ADD_TASK]Comprar pão`. The command name is everything
    /// up to the first `>` or `]` separator.
    pub fn parse(body: &str) -> Self {
        match body.find(['>', ']']) {
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

    /// Human/agent-facing description (surfaced via the MCP `tools/list`).
    fn description(&self) -> &str {
        ""
    }

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

    /// Build the default router with all built-in capabilities. `gate` mediates
    /// approval of dangerous shell commands.
    pub fn with_defaults(db: Database, notifier: Notifier, gate: Arc<dyn ApprovalGate>) -> Self {
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
            db: db.clone(),
            notifier,
            http,
            webhook_url,
        }));
        router.register(Arc::new(MemorizeTool { db: db.clone() }));
        router.register(Arc::new(RecallTool { db }));
        router.register(Arc::new(ReadLocalFileTool));
        router.register(Arc::new(ShellCommandTool { gate }));
        router.register(Arc::new(ScanProjectTool));
        router
    }

    /// `(name, description)` for every registered tool, sorted by name.
    /// Consumed by the MCP bridge's `tools/list`.
    pub fn descriptors(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .tools
            .values()
            .map(|t| (t.name().to_string(), t.description().to_string()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
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

    fn description(&self) -> &str {
        "Cria e persiste uma tarefa pessoal (título e data opcional)."
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

    fn description(&self) -> &str {
        "Delega um ticket a um membro da equipe e notifica via webhook."
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
/// stdout/stderr as follow-up context for the model to analyze. Mutating
/// commands (anything not in `SAFE_PREFIXES`) require human approval first.
struct ShellCommandTool {
    gate: Arc<dyn ApprovalGate>,
}

const SHELL_TIMEOUT_SECS: u64 = 5;
const SHELL_OUTPUT_MAX_CHARS: usize = 2000;

/// Read-only/safe iff it starts with a safe prefix AND has no shell metachars
/// (so `ls; rm -rf` is NOT considered safe).
fn is_safe_command(cmd: &str) -> bool {
    let cmd = cmd.trim();
    if cmd.contains(DANGEROUS_CHARS) {
        return false;
    }
    SAFE_PREFIXES.iter().any(|p| {
        cmd == *p || cmd.starts_with(&format!("{p} ")) || cmd.starts_with(&format!("{p}\t"))
    })
}

#[async_trait]
impl Tool for ShellCommandTool {
    fn name(&self) -> &str {
        "CMD"
    }

    fn description(&self) -> &str {
        "Executa um comando de shell (mutações exigem aprovação)."
    }

    async fn execute(&self, raw_args: &str) -> AppResult<ToolOutcome> {
        let cmd = raw_args.trim().to_string();
        if cmd.is_empty() {
            return Ok(ToolOutcome::summary("Comando vazio — ignorado."));
        }

        // Human-in-the-loop: dangerous commands need explicit approval.
        if !is_safe_command(&cmd) && !self.gate.request(&cmd).await {
            tracing::info!(%cmd, "shell command denied by user");
            return Ok(ToolOutcome::with_follow_up(
                "Execução negada",
                format!("Usuário negou a execução do comando: `{cmd}`."),
            ));
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

    fn description(&self) -> &str {
        "Lê um arquivo local (limitado) e o injeta para análise."
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

/// Scan a directory, summarize each text file (RAG-lite), and inject a condensed
/// digest — for digesting dense docs (e.g. paper drafts) that exceed the 2048
/// context window. Summarization is rule-based per file type (headers, function
/// signatures, abstracts) so nothing pollutes the model's stack with raw bulk.
struct ScanProjectTool;

const SCAN_MAX_FILES: usize = 40;
const SCAN_MAX_DEPTH: usize = 3;
const SCAN_DIGEST_MAX_CHARS: usize = 6000;
const SCAN_PER_FILE_MAX_CHARS: usize = 600;
const SCAN_EXTS: [&str; 7] = ["md", "markdown", "tex", "txt", "rs", "toml", "py"];

#[async_trait]
impl Tool for ScanProjectTool {
    fn name(&self) -> &str {
        "SCAN_DIR"
    }

    fn description(&self) -> &str {
        "Varre e resume um diretório inteiro (RAG-lite)."
    }

    async fn execute(&self, raw_args: &str) -> AppResult<ToolOutcome> {
        let dir = raw_args.trim().to_string();
        if dir.is_empty() {
            return Ok(ToolOutcome::summary("Diretório vazio — ignorado."));
        }

        let dir_for_scan = dir.clone();
        let (digest, count) = tokio::task::spawn_blocking(move || scan_dir(&dir_for_scan))
            .await
            .map_err(join_err)??;

        if count == 0 {
            return Ok(ToolOutcome::summary(format!(
                "Nenhum arquivo de texto encontrado em {dir}."
            )));
        }

        let injected = format!(
            "Resumo condensado de {count} arquivo(s) em `{dir}` (cabeçalhos, \
             assinaturas e seções):\n```\n{digest}\n```\n\
             Use este panorama para responder à solicitação do usuário.",
        );
        Ok(ToolOutcome::with_follow_up(
            format!("Varrendo {dir} ({count} arquivos)"),
            injected,
        ))
    }
}

/// Permanently store a note in the FTS5 long-term memory.
struct MemorizeTool {
    db: Database,
}

#[async_trait]
impl Tool for MemorizeTool {
    fn name(&self) -> &str {
        "MEMORIZE"
    }

    fn description(&self) -> &str {
        "Salva uma anotação na memória permanente (FTS5)."
    }

    async fn execute(&self, raw_args: &str) -> AppResult<ToolOutcome> {
        // `Título|Conteúdo|tags-opcionais`
        let mut parts = raw_args.splitn(3, '|');
        let title = parts.next().unwrap_or("").trim().to_string();
        let content = parts.next().unwrap_or("").trim().to_string();
        let tags = parts.next().unwrap_or("").trim().to_string();

        if title.is_empty() || content.is_empty() {
            return Ok(ToolOutcome::summary(
                "Memória inválida (faltou título ou conteúdo).",
            ));
        }

        let note = KnowledgeNote::new(title.clone(), content, tags);
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || db.insert_knowledge(&note))
            .await
            .map_err(join_err)??;

        Ok(ToolOutcome::summary(format!("Memorizado: {title}")))
    }
}

/// Recall notes from long-term memory and inject them into the conversation.
struct RecallTool {
    db: Database,
}

#[async_trait]
impl Tool for RecallTool {
    fn name(&self) -> &str {
        "RECALL"
    }

    fn description(&self) -> &str {
        "Busca na memória permanente por palavras-chave."
    }

    async fn execute(&self, raw_args: &str) -> AppResult<ToolOutcome> {
        let query = raw_args.trim().to_string();
        if query.is_empty() {
            return Ok(ToolOutcome::summary("Busca de memória vazia — ignorada."));
        }

        let db = self.db.clone();
        let q = query.clone();
        let hits = tokio::task::spawn_blocking(move || db.search_knowledge(&q, 5))
            .await
            .map_err(join_err)??;

        if hits.is_empty() {
            return Ok(ToolOutcome::summary(format!(
                "Nada encontrado na memória sobre: {query}"
            )));
        }

        let mut digest = String::new();
        for note in &hits {
            digest.push_str(&format!("### {}", note.title));
            if !note.tags.is_empty() {
                digest.push_str(&format!(" [{}]", note.tags));
            }
            digest.push('\n');
            digest.push_str(&note.content);
            digest.push_str("\n\n");
        }

        let injected = format!(
            "Da sua memória permanente sobre `{query}`:\n```\n{}\n```\n\
             Use estas anotações para responder ao usuário.",
            digest.trim_end()
        );
        Ok(ToolOutcome::with_follow_up(
            format!("Lembrando: {query}"),
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

/// Keep only the first `max` characters (char-safe), appending `…` when cut.
fn head_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    }
}

/// Recursively collect text files (depth/file capped, skipping hidden + heavy dirs).
fn collect_files(dir: &std::path::Path, depth: usize, out: &mut Vec<std::path::PathBuf>) {
    if depth > SCAN_MAX_DEPTH || out.len() >= SCAN_MAX_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= SCAN_MAX_FILES {
            break;
        }
        let path = entry.path();
        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if name.starts_with('.') || name == "target" || name == "node_modules" {
                    continue;
                }
            }
            collect_files(&path, depth + 1, out);
        } else if path.is_file() {
            if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                if SCAN_EXTS.contains(&ext.to_lowercase().as_str()) {
                    out.push(path);
                }
            }
        }
    }
}

/// Rule-based per-file summary: pull structural lines, not raw bulk.
fn summarize_file(ext: &str, content: &str) -> String {
    let mut picked: Vec<&str> = Vec::new();
    match ext {
        "md" | "markdown" => {
            for l in content.lines() {
                if l.trim_start().starts_with('#') {
                    picked.push(l.trim_end());
                }
            }
            if let Some(first) = content
                .lines()
                .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
            {
                picked.push(first.trim_end());
            }
        }
        "tex" => {
            for l in content.lines() {
                let t = l.trim_start();
                if t.starts_with("\\section")
                    || t.starts_with("\\subsection")
                    || t.starts_with("\\chapter")
                    || t.starts_with("\\title")
                    || t.starts_with("\\paragraph")
                    || t.contains("\\begin{abstract}")
                {
                    picked.push(l.trim_end());
                }
            }
        }
        "rs" => {
            for l in content.lines() {
                let t = l.trim_start();
                if t.starts_with("pub fn ")
                    || t.starts_with("fn ")
                    || t.starts_with("pub struct ")
                    || t.starts_with("struct ")
                    || t.starts_with("pub enum ")
                    || t.starts_with("enum ")
                    || t.starts_with("pub trait ")
                    || t.starts_with("trait ")
                    || t.starts_with("impl ")
                {
                    picked.push(l.trim_end());
                }
            }
        }
        "py" => {
            for l in content.lines() {
                let t = l.trim_start();
                if t.starts_with("def ") || t.starts_with("class ") {
                    picked.push(l.trim_end());
                }
            }
        }
        "toml" => {
            for l in content.lines() {
                if l.trim_start().starts_with('[') {
                    picked.push(l.trim());
                }
            }
        }
        _ => {
            for l in content.lines().take(10) {
                picked.push(l.trim_end());
            }
        }
    }

    if picked.is_empty() {
        for l in content.lines().filter(|l| !l.trim().is_empty()).take(5) {
            picked.push(l.trim_end());
        }
    }

    head_chars(&picked.join("\n"), SCAN_PER_FILE_MAX_CHARS)
}

/// Walk `dir`, summarize each text file, and concatenate a bounded digest.
fn scan_dir(dir: &str) -> AppResult<(String, usize)> {
    let base = std::path::Path::new(dir);
    if !base.is_dir() {
        return Err(AppError::Other(anyhow::anyhow!(
            "{dir} não é um diretório"
        )));
    }

    let mut files = Vec::new();
    collect_files(base, 0, &mut files);
    files.sort();

    let mut digest = String::new();
    let mut count = 0usize;
    for path in &files {
        if digest.chars().count() >= SCAN_DIGEST_MAX_CHARS {
            break;
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let content = String::from_utf8_lossy(&bytes);
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        let summary = summarize_file(&ext, &content);
        if summary.trim().is_empty() {
            continue;
        }
        let rel = path.strip_prefix(base).unwrap_or(path);
        digest.push_str(&format!("## {}\n{}\n\n", rel.display(), summary));
        count += 1;
    }

    Ok((head_chars(&digest, SCAN_DIGEST_MAX_CHARS), count))
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
    use crate::services::approval::{AlwaysApprove, AlwaysDeny};

    fn approve() -> Arc<dyn ApprovalGate> {
        Arc::new(AlwaysApprove)
    }

    #[test]
    fn parses_command_and_args() {
        let c = ToolCall::parse("ADD_TASK>Comprar pão|2026-06-20");
        assert_eq!(c.name, "ADD_TASK");
        assert_eq!(c.raw_args, "Comprar pão|2026-06-20");
    }

    #[tokio::test]
    async fn router_dispatches_add_task() {
        let db = Database::open_in_memory().expect("db");
        let router = ToolRouter::with_defaults(db.clone(), Notifier::default(), approve());
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
        let router = ToolRouter::with_defaults(db.clone(), Notifier::default(), approve());
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
        let router = ToolRouter::with_defaults(db.clone(), Notifier::default(), approve());
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
        let router = ToolRouter::with_defaults(db, Notifier::default(), approve());

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
        let router = ToolRouter::with_defaults(db, Notifier::default(), approve());
        let out = router.dispatch(ToolCall::parse("NOPE>x")).await.expect("ok");
        assert!(out.summary.contains("desconhecido"));
    }

    #[tokio::test]
    async fn shell_runs_harmless_command() {
        let db = Database::open_in_memory().expect("db");
        let router = ToolRouter::with_defaults(db, Notifier::default(), approve());
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

    // --- Human-in-the-loop ---------------------------------------------------

    #[test]
    fn safe_command_classification() {
        assert!(is_safe_command("ls -la"));
        assert!(is_safe_command("git status -s"));
        assert!(is_safe_command("echo hi"));
        assert!(!is_safe_command("rm -rf /tmp/x"));
        assert!(!is_safe_command("git commit -m wip"));
        assert!(!is_safe_command("ls; rm -rf /")); // metachar defeats prefix
        assert!(!is_safe_command("cat a > b")); // redirection
    }

    #[tokio::test]
    async fn shell_safe_command_skips_gate_even_when_denied() {
        let db = Database::open_in_memory().expect("db");
        // Deny gate, but `echo` is safe → never asks, runs anyway.
        let router = ToolRouter::with_defaults(db, Notifier::default(), Arc::new(AlwaysDeny));
        let out = router
            .dispatch(ToolCall::parse("CMD>echo safe-path"))
            .await
            .expect("dispatch");
        assert!(out.follow_up.expect("output").contains("safe-path"));
    }

    #[tokio::test]
    async fn shell_unsafe_command_denied_returns_refusal() {
        let db = Database::open_in_memory().expect("db");
        let router = ToolRouter::with_defaults(db, Notifier::default(), Arc::new(AlwaysDeny));
        let out = router
            .dispatch(ToolCall::parse("CMD>rm -rf /tmp/should-not-run"))
            .await
            .expect("dispatch");
        assert_eq!(out.summary, "Execução negada");
        assert!(out.follow_up.expect("ctx").contains("negou"));
    }

    #[tokio::test]
    async fn shell_unsafe_command_approved_then_runs() {
        let db = Database::open_in_memory().expect("db");
        // `printf` is not in the safe list → needs approval → approved → runs.
        let router = ToolRouter::with_defaults(db, Notifier::default(), approve());
        let out = router
            .dispatch(ToolCall::parse("CMD>printf approved-run"))
            .await
            .expect("dispatch");
        assert!(out.follow_up.expect("output").contains("approved-run"));
    }

    // --- Long-term memory (FTS5) ---------------------------------------------

    #[tokio::test]
    async fn memorize_persists_to_fts5() {
        let db = Database::open_in_memory().expect("db");
        let router = ToolRouter::with_defaults(db.clone(), Notifier::default(), approve());
        let out = router
            .dispatch(ToolCall::parse(
                "MEMORIZE>ST-LibrasNet|arquitetura baseada em transformers espaço-temporais|paper",
            ))
            .await
            .expect("dispatch");
        assert!(out.summary.contains("Memorizado"));
        assert_eq!(db.count_knowledge().expect("count"), 1);
    }

    #[tokio::test]
    async fn recall_matches_word_fragments() {
        let db = Database::open_in_memory().expect("db");
        let router = ToolRouter::with_defaults(db, Notifier::default(), approve());
        router
            .dispatch(ToolCall::parse(
                "MEMORIZE>ST-LibrasNet|arquitetura baseada em transformers espaço-temporais",
            ))
            .await
            .expect("memorize");

        // Fragment of a title token.
        let by_title = router
            .dispatch(ToolCall::parse("RECALL>LibrasNet"))
            .await
            .expect("recall");
        assert!(by_title.follow_up.expect("ctx").contains("transformers"));

        // Prefix fragment of a content word ("arquit" → "arquitetura").
        let by_fragment = router
            .dispatch(ToolCall::parse("RECALL>arquit"))
            .await
            .expect("recall");
        assert!(by_fragment.follow_up.expect("ctx").contains("transformers"));
    }

    #[tokio::test]
    async fn recall_empty_when_no_match() {
        let db = Database::open_in_memory().expect("db");
        let router = ToolRouter::with_defaults(db, Notifier::default(), approve());
        let out = router
            .dispatch(ToolCall::parse("RECALL>inexistente"))
            .await
            .expect("recall");
        assert!(out.follow_up.is_none());
        assert!(out.summary.contains("Nada encontrado"));
    }

    // --- Workspace scanner ---------------------------------------------------

    #[tokio::test]
    async fn scan_summarizes_structure_not_bulk() {
        let dir = std::env::temp_dir().join("kensho_scan_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("doc.md"),
            "# Título Principal\nlinha de corpo qualquer\n## Seção A\nmais texto",
        )
        .expect("md");
        std::fs::write(
            dir.join("code.rs"),
            "fn main() {}\nlet ruido = 5; // nao e assinatura\npub struct Foo { a: i32 }",
        )
        .expect("rs");

        let db = Database::open_in_memory().expect("db");
        let router = ToolRouter::with_defaults(db, Notifier::default(), approve());
        let out = router
            .dispatch(ToolCall::parse(&format!("SCAN_DIR>{}", dir.display())))
            .await
            .expect("dispatch");

        assert!(out.summary.contains("Varrendo"));
        let digest = out.follow_up.expect("digest");
        assert!(digest.contains("# Título Principal"));
        assert!(digest.contains("## Seção A"));
        assert!(digest.contains("fn main"));
        assert!(digest.contains("struct Foo"));
        // Non-structural code line must be filtered out.
        assert!(!digest.contains("nao e assinatura"));
    }
}
