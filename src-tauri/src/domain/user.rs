//! `UserProfile` — who the assistant is serving. Drives prompt personalization.

use serde::{Deserialize, Serialize};

use super::ids::UserId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfile {
    pub id: UserId,
    pub display_name: String,
    /// IETF language tag, e.g. "pt-BR".
    pub locale: String,
    /// Short free-text persona/preferences fed into the system prompt.
    pub persona: String,
}

impl Default for UserProfile {
    fn default() -> Self {
        Self {
            id: UserId::new(),
            display_name: "Usuário".to_string(),
            locale: "pt-BR".to_string(),
            persona: "Direto, gosta de respostas objetivas.".to_string(),
        }
    }
}
