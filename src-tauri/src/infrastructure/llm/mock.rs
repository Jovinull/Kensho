//! Default zero-dependency engine used until a real `.gguf` backend is enabled.
//!
//! It fakes token-by-token streaming (with realistic pacing) so the entire
//! actor → event → frontend pipeline can be exercised end-to-end without
//! shipping a multi-GB model or compiling llama.cpp.

use std::time::Duration;

use async_trait::async_trait;

use super::engine::{InferenceEngine, LlmError, TokenSink};

pub struct MockEngine {
    id: String,
    token_delay: Duration,
}

impl MockEngine {
    pub fn new() -> Self {
        Self {
            id: "mock-qwen".to_string(),
            token_delay: Duration::from_millis(35),
        }
    }
}

impl Default for MockEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl InferenceEngine for MockEngine {
    fn model_id(&self) -> &str {
        &self.id
    }

    async fn generate(&mut self, prompt: &str, sink: TokenSink) -> Result<(), LlmError> {
        let reply = format!(
            "Entendi seu pedido: \"{}\". Este é o motor simulado do Kensho — \
             compile com --features llama e aponte KENSHO_MODEL_PATH para um \
             arquivo .gguf do Qwen para respostas reais.",
            prompt.trim()
        );

        // `split_inclusive` keeps the trailing space so the bubble reads naturally.
        for token in reply.split_inclusive(' ') {
            // If the receiver was dropped (frontend closed / cancelled), stop.
            if sink.send(token.to_string()).await.is_err() {
                break;
            }
            tokio::time::sleep(self.token_delay).await;
        }
        Ok(())
    }
}
