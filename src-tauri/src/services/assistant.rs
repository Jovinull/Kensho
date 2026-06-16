//! Assistant orchestration: builds the dynamic, context-aware **system prompt**
//! (today's tasks/events + tool instructions). ChatML wrapping and the rolling
//! history live in [`crate::services::conversation`]; the actor owns the loop.

use chrono::{Duration, Timelike, Utc};

use crate::core::AppResult;
use crate::domain::{Task, UserProfile};
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
             FERRAMENTAS: você pode agir no sistema. Imprima a tag EXATAMENTE, \
             sem aspas e sem explicar a sintaxe, e depois confirme em linguagem \
             natural. Use uma tag apenas quando o usuário realmente pedir a ação. \
             \
             1) Registrar tarefa pessoal: \
             <CALL:ADD_TASK>Nome da tarefa|AAAA-MM-DD</CALL> \
             (a data é opcional; omita '|AAAA-MM-DD' se não houver). \
             \
             2) Delegar para a equipe (somente estes nomes: Waldston, Joãozinho, Rafaela): \
             <CALL:DELEGATE>Responsável|Descrição da tarefa</CALL> \
             \
             3) Ler um arquivo local para analisar (logs, código): \
             <CALL:READ_FILE>/caminho/absoluto/do/arquivo.ext</CALL> \
             (após ler, o conteúdo voltará para você analisar e responder). \
             \
             4) Executar um comando de terminal e analisar a saída: \
             <CALL:CMD>git status</CALL> \
             (use só comandos não-interativos e rápidos; a saída voltará para você; \
             comandos que alteram o sistema exigem aprovação do usuário). \
             \
             5) Varrer e resumir um diretório inteiro (docs, código, rascunhos): \
             <CALL:SCAN_DIR>/caminho/do/diretorio</CALL> \
             (retorna um resumo condensado de vários arquivos para você analisar).",
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

    /// Open tasks whose deadline falls within today (used by the heartbeat).
    pub fn pending_due_today(&self) -> AppResult<Vec<Task>> {
        let now = Utc::now();
        let day_start = now - Duration::hours(now.hour() as i64);
        let day_end = day_start + Duration::days(1);
        self.db.pending_tasks_due_between(day_start, day_end)
    }

    /// Build the synthetic system nudge that drives a proactive reminder.
    /// Returns `None` when there is nothing to remind about.
    pub fn deadline_reminder_prompt(tasks: &[Task]) -> Option<String> {
        if tasks.is_empty() {
            return None;
        }
        let titles: Vec<&str> = tasks.iter().take(5).map(|t| t.title.as_str()).collect();
        Some(format!(
            "[AVISO DO SISTEMA] {} tarefa(s) vencem hoje e seguem pendentes: {}. \
             Gere um lembrete curto, gentil e proativo cobrando o usuário sobre \
             elas. Não use nenhuma tag de ferramenta.",
            tasks.len(),
            titles.join("; ")
        ))
    }

    /// Placeholder for proactive-reminder logic (checked by a future scheduler).
    pub fn should_nudge(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::Database;

    #[test]
    fn reminder_prompt_empty_is_none() {
        assert!(AssistantService::deadline_reminder_prompt(&[]).is_none());
    }

    #[test]
    fn reminder_prompt_lists_titles() {
        let tasks = vec![Task::new("Deploy"), Task::new("Code review")];
        let p = AssistantService::deadline_reminder_prompt(&tasks).expect("some");
        assert!(p.contains("Deploy"));
        assert!(p.contains("Code review"));
        assert!(p.contains("AVISO DO SISTEMA"));
    }

    #[test]
    fn pending_due_today_finds_task_due_today() {
        let db = Database::open_in_memory().expect("db");
        let mut task = Task::new("Entregar relatório");
        task.due_at = Some(Utc::now());
        db.insert_task(&task).expect("insert");

        let svc = AssistantService::new(db);
        let due = svc.pending_due_today().expect("query");
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].title, "Entregar relatório");
    }
}
