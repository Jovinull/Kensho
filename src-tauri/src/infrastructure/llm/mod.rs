//! Local inference subsystem.

pub mod engine;
#[cfg(feature = "llama")]
pub mod llama;
pub mod mock;

#[allow(unused_imports)]
pub use engine::{InferenceEngine, LlmError, TokenSink};
pub use mock::MockEngine;

use crate::core::SystemConfig;

/// Construct the best inference engine available for this build & config.
///
/// With `--features llama` and a present `.gguf`, returns the real backend;
/// otherwise falls back to the always-available `MockEngine`.
pub fn build_engine(config: &SystemConfig) -> Box<dyn InferenceEngine> {
    #[cfg(feature = "llama")]
    {
        if config.model_path.exists() {
            match llama::LlamaEngine::load(config) {
                Ok(engine) => {
                    tracing::info!(model = %config.model_path.display(), "loaded gguf backend");
                    return Box::new(engine);
                }
                Err(e) => {
                    tracing::error!(error = %e, "gguf load failed; using mock engine");
                }
            }
        } else {
            tracing::warn!(
                path = %config.model_path.display(),
                "gguf model not found; using mock engine"
            );
        }
    }

    // Referenced so the parameter isn't flagged unused when `llama` is off.
    let _ = config;
    Box::new(MockEngine::new())
}
