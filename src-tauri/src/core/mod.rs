//! Cross-cutting concerns: error handling, configuration, logging.

pub mod config;
pub mod error;
pub mod logging;

pub use config::SystemConfig;
pub use error::{AppError, AppResult, CommandError};
