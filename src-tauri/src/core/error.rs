//! Global error handling.
//!
//! `AppError` is the internal, richly-typed error used across the backend.
//! `CommandError` is the thin, serializable wrapper returned to the frontend so
//! that internal details never leak into the IPC boundary as raw `Debug`.

use serde::Serialize;
use thiserror::Error;

/// Internal error type for the whole backend.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("notification error: {0}")]
    Notify(String),

    #[error("llm error: {0}")]
    Llm(String),

    #[error("the LLM worker channel is closed")]
    WorkerUnavailable,

    #[error("configuration error: {0}")]
    Config(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type AppResult<T> = Result<T, AppError>;

/// Frontend-safe error. Implements `Serialize` so Tauri commands can return
/// `Result<T, CommandError>` directly.
#[derive(Debug, Serialize)]
pub struct CommandError {
    pub message: String,
    pub kind: String,
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for CommandError {}

impl From<AppError> for CommandError {
    fn from(err: AppError) -> Self {
        let kind = match &err {
            AppError::Database(_) => "database",
            AppError::Io(_) => "io",
            AppError::Serde(_) => "serde",
            AppError::Notify(_) => "notify",
            AppError::Llm(_) => "llm",
            AppError::WorkerUnavailable => "worker",
            AppError::Config(_) => "config",
            AppError::Other(_) => "other",
        }
        .to_string();
        CommandError {
            message: err.to_string(),
            kind,
        }
    }
}

impl From<anyhow::Error> for CommandError {
    fn from(err: anyhow::Error) -> Self {
        CommandError {
            message: err.to_string(),
            kind: "other".to_string(),
        }
    }
}
