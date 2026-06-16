//! The inference abstraction every backend implements.
//!
//! The rest of the system depends ONLY on this trait, never on a concrete
//! model runtime — so the MockEngine and a real GGUF backend are swappable
//! without touching the actor, services or commands.

use async_trait::async_trait;
use tokio::sync::mpsc;

/// Errors a backend can raise during loading or inference.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("model not loaded: {0}")]
    NotLoaded(String),

    #[error("inference failed: {0}")]
    Inference(String),
}

/// Channel the engine pushes generated tokens into. The consumer (the actor)
/// forwards them to the frontend. Dropping all senders ends the stream.
pub type TokenSink = mpsc::Sender<String>;

/// A loadable, streaming text-generation engine.
#[async_trait]
pub trait InferenceEngine: Send {
    /// Identifier of the loaded model (path or canonical name).
    fn model_id(&self) -> &str;

    /// Generate a completion for `prompt`, pushing each token into `sink`
    /// as it is produced. Returns when generation is complete.
    async fn generate(&mut self, prompt: &str, sink: TokenSink) -> Result<(), LlmError>;
}
