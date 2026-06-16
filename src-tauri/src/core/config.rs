//! System configuration.
//!
//! All persistent state lives under a single app-data directory (resolved by
//! Tauri's path API at startup) so the rest of the code never hard-codes paths.

use std::path::{Path, PathBuf};

/// Runtime configuration for the whole application.
#[derive(Debug, Clone)]
pub struct SystemConfig {
    /// Base directory for all app data (db, logs, downloaded models).
    pub data_dir: PathBuf,
    /// SQLite database file path.
    pub database_path: PathBuf,
    /// Path to the local Qwen `.gguf` model. Read from `KENSHO_MODEL_PATH`
    /// when set, otherwise defaults to `<data_dir>/models/qwen.gguf`.
    pub model_path: PathBuf,
    /// Whether the character window starts pinned on top.
    pub always_on_top: bool,
    /// Max tokens generated per inference request.
    pub max_tokens: usize,
    /// LLM context window size (tokens). Bounds KV-cache RAM usage.
    /// Read from `KENSHO_CTX` when set.
    pub context_size: u32,
    /// Proactive heartbeat interval in seconds. Read from `KENSHO_HEARTBEAT_SECS`.
    pub heartbeat_secs: u64,
    /// Piper TTS executable (`KENSHO_PIPER_BIN`, default `piper`).
    pub piper_bin: String,
    /// Piper voice model path (`KENSHO_PIPER_MODEL`).
    pub piper_model: String,
    /// Piper output sample rate (`KENSHO_PIPER_RATE`, default 22050).
    pub piper_sample_rate: u32,
    /// MCP TCP server port (`KENSHO_MCP_PORT`, default 8181).
    pub mcp_port: u16,
}

impl SystemConfig {
    /// Build a config rooted at the given app-data directory.
    pub fn from_data_dir(data_dir: impl AsRef<Path>) -> Self {
        let data_dir = data_dir.as_ref().to_path_buf();
        let database_path = data_dir.join("kensho.sqlite");
        let model_path = std::env::var_os("KENSHO_MODEL_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.join("models").join("qwen.gguf"));

        let context_size = std::env::var("KENSHO_CTX")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|&v| v >= 256)
            .unwrap_or(2048);

        let heartbeat_secs = std::env::var("KENSHO_HEARTBEAT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|&v| v >= 10)
            .unwrap_or(300);

        let piper_bin = std::env::var("KENSHO_PIPER_BIN").unwrap_or_else(|_| "piper".to_string());
        let piper_model = std::env::var("KENSHO_PIPER_MODEL").unwrap_or_default();
        let piper_sample_rate = std::env::var("KENSHO_PIPER_RATE")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|&v| v >= 8000)
            .unwrap_or(22050);

        let mcp_port = std::env::var("KENSHO_MCP_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(8181);

        Self {
            data_dir,
            database_path,
            model_path,
            always_on_top: true,
            max_tokens: 512,
            context_size,
            heartbeat_secs,
            piper_bin,
            piper_model,
            piper_sample_rate,
            mcp_port,
        }
    }
}
