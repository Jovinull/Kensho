//! System configuration.
//!
//! Business/runtime parameters are externalized to `~/.config/kensho/config.toml`
//! (created with defaults on first boot) and overridable by `KENSHO_*` env vars,
//! merged via Figment. The binary is portable — no recompile to reconfigure.

use std::path::{Path, PathBuf};

use figment::providers::{Env, Format, Serialized, Toml};
use figment::Figment;
use serde::{Deserialize, Serialize};

/// Default dev team for delegation (used as a config default).
pub const DEFAULT_TEAM: [&str; 3] = ["Waldston", "Joãozinho", "Rafaela"];

/// On-disk / env-overridable configuration schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KenshoConfig {
    /// Path to the local Qwen `.gguf`. Empty → `<data_dir>/models/qwen.gguf`.
    pub model_path: String,
    pub always_on_top: bool,
    pub max_tokens: usize,
    pub context_size: u32,
    pub heartbeat_secs: u64,
    pub piper_bin: String,
    pub piper_model: String,
    pub piper_sample_rate: u32,
    pub mcp_port: u16,
    /// Valid delegation targets (the dev team).
    pub team_members: Vec<String>,
    /// Global hotkey, parsed by the shortcut plugin (e.g. "Ctrl+Shift+K").
    pub global_shortcut: String,
}

impl Default for KenshoConfig {
    fn default() -> Self {
        Self {
            model_path: String::new(),
            always_on_top: true,
            max_tokens: 512,
            context_size: 2048,
            heartbeat_secs: 300,
            piper_bin: "piper".to_string(),
            piper_model: String::new(),
            piper_sample_rate: 22050,
            mcp_port: 8181,
            team_members: DEFAULT_TEAM.iter().map(|s| s.to_string()).collect(),
            global_shortcut: "Ctrl+Shift+K".to_string(),
        }
    }
}

impl KenshoConfig {
    /// Path to the config file: `~/.config/kensho/config.toml` on Linux.
    pub fn config_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("com", "Kensho", "kensho")
            .map(|dirs| dirs.config_dir().join("config.toml"))
    }

    /// Load defaults ← config.toml ← `KENSHO_*` env (later wins). Creates the
    /// file with defaults on first boot.
    pub fn load() -> Self {
        if let Some(path) = Self::config_path() {
            if !path.exists() {
                Self::default().write_default(&path);
            }
            Self::from_figment(Some(&path))
        } else {
            Self::from_figment(None)
        }
    }

    fn from_figment(path: Option<&Path>) -> Self {
        let mut fig = Figment::from(Serialized::defaults(KenshoConfig::default()));
        if let Some(p) = path {
            fig = fig.merge(Toml::file(p));
        }
        fig = fig.merge(Env::prefixed("KENSHO_"));
        fig.extract().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "invalid config; using defaults");
            KenshoConfig::default()
        })
    }

    fn write_default(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match toml::to_string_pretty(self) {
            Ok(body) => {
                if std::fs::write(path, body).is_ok() {
                    tracing::info!(path = %path.display(), "wrote default config");
                }
            }
            Err(e) => tracing::warn!(error = %e, "could not serialize default config"),
        }
    }
}

/// Resolved runtime configuration for the whole application.
#[derive(Debug, Clone)]
pub struct SystemConfig {
    pub data_dir: PathBuf,
    pub database_path: PathBuf,
    pub model_path: PathBuf,
    pub always_on_top: bool,
    pub max_tokens: usize,
    pub context_size: u32,
    pub heartbeat_secs: u64,
    pub piper_bin: String,
    pub piper_model: String,
    pub piper_sample_rate: u32,
    pub mcp_port: u16,
    pub team_members: Vec<String>,
    pub global_shortcut: String,
}

impl SystemConfig {
    /// Load config (file + env) and resolve runtime paths under `data_dir`.
    pub fn from_data_dir(data_dir: impl AsRef<Path>) -> Self {
        Self::resolve(data_dir, KenshoConfig::load())
    }

    fn resolve(data_dir: impl AsRef<Path>, cfg: KenshoConfig) -> Self {
        let data_dir = data_dir.as_ref().to_path_buf();
        let model_path = if cfg.model_path.trim().is_empty() {
            data_dir.join("models").join("qwen.gguf")
        } else {
            PathBuf::from(&cfg.model_path)
        };

        Self {
            database_path: data_dir.join("kensho.sqlite"),
            data_dir,
            model_path,
            always_on_top: cfg.always_on_top,
            max_tokens: cfg.max_tokens,
            context_size: cfg.context_size.max(256),
            heartbeat_secs: cfg.heartbeat_secs.max(10),
            piper_bin: cfg.piper_bin,
            piper_model: cfg.piper_model,
            piper_sample_rate: cfg.piper_sample_rate.max(8000),
            mcp_port: cfg.mcp_port,
            team_members: cfg.team_members,
            global_shortcut: cfg.global_shortcut,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_resolve_model_path_under_data_dir() {
        let cfg = SystemConfig::resolve("/tmp/kensho-x", KenshoConfig::default());
        assert!(cfg.model_path.ends_with("models/qwen.gguf"));
        assert_eq!(cfg.mcp_port, 8181);
        assert_eq!(cfg.team_members.len(), 3);
        assert_eq!(cfg.global_shortcut, "Ctrl+Shift+K");
    }

    #[test]
    fn explicit_model_path_is_used_verbatim() {
        let mut kc = KenshoConfig::default();
        kc.model_path = "/models/custom.gguf".to_string();
        let cfg = SystemConfig::resolve("/tmp/kensho-y", kc);
        assert_eq!(cfg.model_path, PathBuf::from("/models/custom.gguf"));
    }

    #[test]
    fn config_roundtrips_through_toml() {
        let body = toml::to_string_pretty(&KenshoConfig::default()).expect("serialize");
        let parsed: KenshoConfig = toml::from_str(&body).expect("parse");
        assert_eq!(parsed.team_members, DEFAULT_TEAM);
    }
}
