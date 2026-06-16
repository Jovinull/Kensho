//! Assistant orchestration: assembles user context, builds the final prompt,
//! and dispatches it to the LLM actor. This is where "when to nudge the user"
//! and "how to chain context" logic lives.

use crate::actor::LlmHandle;
use crate::core::AppResult;
use crate::domain::UserProfile;
use crate::infrastructure::Database;

pub struct AssistantService {
    db: Database,
    profile: UserProfile,
}

impl AssistantService {
    pub fn new(db: Database) -> Self {
        Self {
            db,
            profile: UserProfile::default(),
        }
    }

    /// Compose a context-aware prompt from the user profile + persisted state.
    pub fn build_prompt(&self, user_input: &str) -> AppResult<String> {
        let pending = self.db.count_tasks().unwrap_or(0);
        let system = format!(
            "Você é Kensho, um assistente local rodando no Ubuntu de {name}. \
             Persona: {persona}. O usuário tem {pending} tarefa(s) registrada(s). \
             Responda em {locale}, de forma breve e útil.",
            name = self.profile.display_name,
            persona = self.profile.persona,
            pending = pending,
            locale = self.profile.locale,
        );
        Ok(format!("{system}\n\nUsuário: {user_input}\nKensho:"))
    }

    /// Build the prompt and forward it to the actor (non-blocking).
    pub async fn ask(&self, handle: &LlmHandle, user_input: &str) -> AppResult<()> {
        let prompt = self.build_prompt(user_input)?;
        handle.generate(prompt).await
    }

    /// Placeholder for proactive-reminder logic (checked by a future scheduler).
    pub fn should_nudge(&self) -> bool {
        false
    }
}
