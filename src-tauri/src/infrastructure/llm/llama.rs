//! Real local GGUF backend — compiled only with `--features llama`.
//!
//! Selected binding: `llama-cpp-2` (mature Rust bindings over llama.cpp, the
//! reference GGUF runtime). It is an optional dependency so the default build
//! never pays the C++ compile cost.
//!
//! Wiring the real token loop (left as the single integration TODO below)
//! follows this shape with llama-cpp-2 0.1.x:
//!
//! ```ignore
//! use llama_cpp_2::llama_backend::LlamaBackend;
//! use llama_cpp_2::model::{params::LlamaModelParams, LlamaModel, AddBos};
//! use llama_cpp_2::context::params::LlamaContextParams;
//! use llama_cpp_2::llama_batch::LlamaBatch;
//!
//! let backend = LlamaBackend::init()?;
//! let model = LlamaModel::load_from_file(&backend, &path, &LlamaModelParams::default())?;
//! let mut ctx = model.new_context(&backend, LlamaContextParams::default())?;
//! // tokenize prompt -> decode -> sample -> for each token: sink.send(piece).await
//! ```

use std::path::PathBuf;

use async_trait::async_trait;

use super::engine::{InferenceEngine, LlmError, TokenSink};
use crate::core::SystemConfig;

pub struct LlamaEngine {
    model_path: PathBuf,
    max_tokens: usize,
}

impl LlamaEngine {
    /// Validate the model file and prepare the engine.
    pub fn load(config: &SystemConfig) -> Result<Self, LlmError> {
        let model_path = config.model_path.clone();
        if !model_path.exists() {
            return Err(LlmError::NotLoaded(format!(
                "gguf model not found at {}",
                model_path.display()
            )));
        }
        Ok(Self {
            model_path,
            max_tokens: config.max_tokens,
        })
    }
}

#[async_trait]
impl InferenceEngine for LlamaEngine {
    fn model_id(&self) -> &str {
        self.model_path.to_str().unwrap_or("gguf")
    }

    async fn generate(&mut self, prompt: &str, sink: TokenSink) -> Result<(), LlmError> {
        // INTEGRATION POINT: run the blocking llama.cpp decode loop on a
        // dedicated thread (tokio::task::spawn_blocking) and forward each
        // decoded piece into `sink`. The actor already handles streaming.
        let notice = format!(
            "[llama backend ready] model={} max_tokens={} prompt={:?} \
             — wire the llama-cpp-2 decode loop in infrastructure/llm/llama.rs",
            self.model_path.display(),
            self.max_tokens,
            prompt
        );
        sink.send(notice)
            .await
            .map_err(|e| LlmError::Inference(e.to_string()))?;
        Ok(())
    }
}
