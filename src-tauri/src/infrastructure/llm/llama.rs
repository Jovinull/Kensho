//! Real local GGUF backend — compiled only with `--features llama`.
//!
//! Binding: `llama-cpp-2` (safe-ish wrapper over llama.cpp, the reference GGUF
//! runtime). Ownership model chosen to satisfy the actor's `Send` bound and to
//! keep the (blocking, CPU-heavy) decode off the async runtime:
//!
//!   * `LlamaBackend` — empty, process-global init guard (`Send + Sync`).
//!   * `LlamaModel`   — loaded once, shared via `Arc` (`unsafe Send + Sync`).
//!   * `LlamaContext` — created fresh per request *inside* `spawn_blocking`
//!     (it borrows the model, so it must not outlive a single generation and
//!     must never cross an `.await`).

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

use super::engine::{InferenceEngine, LlmError, TokenSink};
use crate::core::SystemConfig;

/// GBNF for the tool-call body that follows the `<CALL:` trigger. Forces a
/// structurally valid `NAME>args</CALL>` once the model decides to open a call.
const CALL_GRAMMAR: &str = r#"
root ::= name ">" body "</CALL>"
name ::= [A-Za-z_]+
body ::= [^<]*
"#;

pub struct LlamaEngine {
    backend: Arc<LlamaBackend>,
    model: Arc<LlamaModel>,
    model_id: String,
    /// Context window size (bounds RAM/KV-cache usage).
    n_ctx: u32,
    /// Hard cap on generated tokens per request.
    max_tokens: usize,
}

impl LlamaEngine {
    /// Initialize the backend and load the GGUF model pointed to by the config
    /// (`KENSHO_MODEL_PATH` → `SystemConfig::model_path`).
    pub fn load(config: &SystemConfig) -> Result<Self, LlmError> {
        let model_path: PathBuf = config.model_path.clone();
        if !model_path.exists() {
            return Err(LlmError::NotLoaded(format!(
                "gguf model not found at {} (set KENSHO_MODEL_PATH)",
                model_path.display()
            )));
        }

        // The backend can only be initialized once per process.
        let backend = LlamaBackend::init()
            .map_err(|e| LlmError::NotLoaded(format!("backend init failed: {e}")))?;

        // CPU-friendly defaults; raise n_gpu_layers when a GPU build is used.
        let model_params = LlamaModelParams::default();
        let model = LlamaModel::load_from_file(&backend, &model_path, &model_params)
            .map_err(|e| LlmError::NotLoaded(format!("failed to load model: {e}")))?;

        tracing::info!(model = %model_path.display(), n_ctx = config.context_size, "gguf model loaded");

        Ok(Self {
            backend: Arc::new(backend),
            model: Arc::new(model),
            model_id: model_path.to_string_lossy().into_owned(),
            n_ctx: config.context_size,
            max_tokens: config.max_tokens,
        })
    }
}

#[async_trait]
impl InferenceEngine for LlamaEngine {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    async fn generate(&mut self, prompt: &str, sink: TokenSink) -> Result<(), LlmError> {
        // Clone cheap handles into the blocking thread; the model stays shared.
        let model = Arc::clone(&self.model);
        let backend = Arc::clone(&self.backend);
        let prompt = prompt.to_owned();
        let n_ctx = self.n_ctx;
        let max_tokens = self.max_tokens;

        // The decode loop is CPU-bound and blocking — keep it off the runtime.
        let join = tokio::task::spawn_blocking(move || {
            decode_loop(&model, &backend, &prompt, n_ctx, max_tokens, sink)
        })
        .await;

        match join {
            Ok(inner) => inner,
            Err(e) => Err(LlmError::Inference(format!("inference task panicked: {e}"))),
        }
    }
}

/// The synchronous llama.cpp decode loop. Streams each decoded piece into
/// `sink` as it is produced.
fn decode_loop(
    model: &LlamaModel,
    backend: &LlamaBackend,
    prompt: &str,
    n_ctx: u32,
    max_tokens: usize,
    sink: TokenSink,
) -> Result<(), LlmError> {
    let n_threads = std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4);

    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(n_ctx))
        .with_n_threads(n_threads)
        .with_n_threads_batch(n_threads);

    let mut ctx = model
        .new_context(backend, ctx_params)
        .map_err(|e| LlmError::Inference(format!("context creation failed: {e}")))?;

    // Tokenize the prompt (with BOS).
    let tokens = model
        .str_to_token(prompt, AddBos::Always)
        .map_err(|e| LlmError::Inference(format!("tokenize failed: {e}")))?;

    let ctx_window = ctx.n_ctx() as i32;
    if tokens.len() as i32 >= ctx_window {
        return Err(LlmError::Inference(format!(
            "prompt ({} tokens) exceeds context window ({})",
            tokens.len(),
            ctx_window
        )));
    }

    // Feed the prompt; only the last token needs logits for the first sample.
    let mut batch = LlamaBatch::new(ctx_window as usize, 1);
    let last = tokens.len() as i32 - 1;
    for (i, token) in tokens.iter().enumerate() {
        batch
            .add(*token, i as i32, &[0], i as i32 == last)
            .map_err(|e| LlmError::Inference(format!("batch add failed: {e}")))?;
    }
    ctx.decode(&mut batch)
        .map_err(|e| LlmError::Inference(format!("prompt decode failed: {e}")))?;

    // A light sampler chain: top-k → top-p → temperature → distribution.
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(1234);
    // Anti-loop sampler chain: a moderate repetition penalty stops runaway
    // repeats without breaking pt-BR, balanced temperature for an assistant.
    //   penalties(last_n, repeat=1.12, freq=0.0, present=0.0)
    let mut samplers = Vec::new();

    // Optional GBNF grammar (opt-in via KENSHO_GRAMMAR): a *lazy* grammar that
    // only kicks in once the model emits the `<CALL:` trigger, forcing the rest
    // of the tag to be structurally valid while leaving free text unconstrained.
    // Off by default; the deterministic fuzzy parser in stream_filter.rs is the
    // always-on safety net.
    if std::env::var_os("KENSHO_GRAMMAR").is_some() {
        match LlamaSampler::grammar_lazy(model, CALL_GRAMMAR, "root", ["<CALL:"], &[]) {
            Ok(g) => {
                tracing::info!("GBNF lazy grammar enabled for tool calls");
                samplers.push(g);
            }
            Err(e) => tracing::warn!(error = %e, "failed to build grammar; skipping"),
        }
    }

    samplers.push(LlamaSampler::penalties(64, 1.12, 0.0, 0.0));
    samplers.push(LlamaSampler::top_k(40));
    samplers.push(LlamaSampler::top_p(0.9, 1));
    samplers.push(LlamaSampler::temp(0.7));
    samplers.push(LlamaSampler::dist(seed));
    let mut sampler = LlamaSampler::chain_simple(samplers);

    // Explicit stop sequence: halt as soon as the model emits `<|im_end|>`.
    // Resolved to its token id when the tokenizer maps it to a single special
    // token; otherwise we still rely on `is_eog_token`.
    let im_end_id = model
        .str_to_token("<|im_end|>", AddBos::Never)
        .ok()
        .filter(|v| v.len() == 1)
        .map(|v| v[0]);

    let mut n_cur = batch.n_tokens();
    // Stop at the smaller of: prompt + max_tokens, or the context window.
    let n_len = (tokens.len() as i32 + max_tokens as i32).min(ctx_window - 1);

    // Buffer bytes so multi-byte UTF-8 (e.g. acentuação pt-BR) split across
    // tokens is never emitted as invalid UTF-8.
    let mut byte_buf: Vec<u8> = Vec::with_capacity(64);

    while n_cur < n_len {
        let token = sampler.sample(&ctx, batch.n_tokens() - 1);
        sampler.accept(token);

        // Stop sequence + end-of-generation.
        if model.is_eog_token(token) || Some(token) == im_end_id {
            break;
        }

        // Plaintext: special/control tokens render empty, never leaking machine
        // markup (e.g. `<|im_end|>`) into the user-facing stream.
        #[allow(deprecated)]
        let bytes = model
            .token_to_bytes(token, llama_cpp_2::model::Special::Plaintext)
            .map_err(|e| LlmError::Inference(format!("detokenize failed: {e}")))?;
        byte_buf.extend_from_slice(&bytes);

        // Flush only complete UTF-8; keep trailing partial bytes buffered.
        match std::str::from_utf8(&byte_buf) {
            Ok(piece) => {
                if !piece.is_empty() && sink.blocking_send(piece.to_owned()).is_err() {
                    break; // frontend/receiver dropped → cancel generation
                }
                byte_buf.clear();
            }
            Err(_) => { /* incomplete multibyte sequence: wait for more bytes */ }
        }

        // Decode the freshly sampled token to obtain the next logits.
        batch.clear();
        batch
            .add(token, n_cur, &[0], true)
            .map_err(|e| LlmError::Inference(format!("batch add failed: {e}")))?;
        n_cur += 1;
        ctx.decode(&mut batch)
            .map_err(|e| LlmError::Inference(format!("decode failed: {e}")))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    /// Real end-to-end inference against a local GGUF.
    /// Ignored by default; run explicitly once KENSHO_MODEL_PATH points at a model:
    ///   cargo test --features llama -- --ignored real_inference_streams_tokens
    #[tokio::test]
    #[ignore]
    async fn real_inference_streams_tokens() {
        let tmp = std::env::temp_dir().join("kensho-test");
        let config = SystemConfig::from_data_dir(&tmp);
        assert!(
            config.model_path.exists(),
            "set KENSHO_MODEL_PATH to a .gguf before running this test"
        );

        let mut engine = LlamaEngine::load(&config).expect("load model");
        let (tx, mut rx) = mpsc::channel::<String>(256);

        // Proper ChatML prompt — exercises the fixed path + `<|im_end|>` stop.
        let prompt = "<|im_start|>system\nVocê é Kensho, responda em pt-BR de forma breve.\
                      <|im_end|>\n<|im_start|>user\nDiga olá em uma frase curta.\
                      <|im_end|>\n<|im_start|>assistant\n"
            .to_string();

        let gen = tokio::spawn(async move {
            engine.generate(&prompt, tx).await.expect("generate");
        });

        let mut out = String::new();
        while let Some(tok) = rx.recv().await {
            out.push_str(&tok);
        }
        gen.await.expect("join");

        println!("model output: {out}");
        assert!(!out.trim().is_empty(), "expected non-empty generation");
    }
}
