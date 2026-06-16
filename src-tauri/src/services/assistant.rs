//! Assistant orchestration: builds the dynamic, context-aware **system prompt**
//! (today's tasks/events + tool instructions). ChatML wrapping and the rolling
//! history live in [`crate::services::conversation`]; the actor owns the loop.

use chrono::{Duration, Timelike, Utc};

use crate::core::AppResult;
use crate::domain::UserProfile;
use crate::infrastructure::Database;

#[derive(Clone)]
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

    /// Build the dynamic **system prompt**: persona + today's tasks/events
    /// (queried from SQLite) + the tool-calling protocol. This is the invisible
    /// `system` role content; the conversation layer wraps it in ChatML.
    pub fn system_prompt(&self) -> AppResult<String> {
        let now = Utc::now();
        // Naive day window in UTC; timezone-aware windowing is a later concern.
        let day_start = now - Duration::hours(now.hour() as i64);
        let day_end = day_start + Duration::days(1);

        let tasks = self.db.pending_tasks_due_by(day_end).unwrap_or_default();
        let events = self.db.events_between(day_start, day_end).unwrap_or_default();

        let context = Self::render_context(&tasks, &events);

        let system = format!(
            "Você é Kensho, um assistente local que roda no Ubuntu de {name}. \
             Persona: {persona}. Responda em {locale}, de forma breve, objetiva e útil. \
             {context} \
             Leve essas informações em conta ao responder, mas só as mencione se forem relevantes. \
             \
             FERRAMENTAS: você pode agir no sistema. Para adicionar uma tarefa, \
             imprima EXATAMENTE a tag, sem aspas e sem explicar a sintaxe: \
             <CALL:ADD_TASK>Nome da tarefa|AAAA-MM-DD</CALL> \
             (a data é opcional; omita o '|AAAA-MM-DD' se não houver). \
             Emita a tag e, em seguida, confirme em linguagem natural para o usuário. \
             Use a tag apenas quando o usuário pedir para registrar/lembrar/agendar algo.",
            name = self.profile.display_name,
            persona = self.profile.persona,
            locale = self.profile.locale,
            context = context,
        );

        Ok(system)
    }

    /// Turn the persisted state into a compact natural-language context block.
    fn render_context(
        tasks: &[crate::domain::Task],
        events: &[crate::domain::ScheduleEvent],
    ) -> String {
        if tasks.is_empty() && events.is_empty() {
            return "O usuário não tem tarefas pendentes nem compromissos para hoje.".to_string();
        }

        let mut parts = Vec::new();

        if !tasks.is_empty() {
            let titles: Vec<&str> = tasks.iter().take(5).map(|t| t.title.as_str()).collect();
            parts.push(format!(
                "O usuário tem {} tarefa(s) pendente(s) para hoje: {}.",
                tasks.len(),
                titles.join("; ")
            ));
        }

        if !events.is_empty() {
            let titles: Vec<String> = events
                .iter()
                .take(5)
                .map(|e| format!("{} ({})", e.title, e.start_at.format("%H:%M")))
                .collect();
            parts.push(format!(
                "Compromissos na agenda de hoje: {}.",
                titles.join("; ")
            ));
        }

        parts.join(" ")
    }

    /// Placeholder for proactive-reminder logic (checked by a future scheduler).
    pub fn should_nudge(&self) -> bool {
        false
    }
}
