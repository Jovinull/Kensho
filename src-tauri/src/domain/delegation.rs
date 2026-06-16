//! `DelegatedTask` — a ticket assigned to a team member, modelled like an
//! agile-board issue. Produced by the `DELEGATE` tool.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::ids::DelegateId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegatedTask {
    pub id: DelegateId,
    pub assignee: String,
    pub description: String,
    pub status: String,
    /// Structured agile-issue payload (JSON) — simulates a board ticket.
    pub payload: String,
    pub created_at: DateTime<Utc>,
}

impl DelegatedTask {
    pub fn new(assignee: impl Into<String>, description: impl Into<String>) -> Self {
        let assignee = assignee.into();
        let description = description.into();
        let now = Utc::now();
        let id = DelegateId::new();

        let title: String = description.chars().take(60).collect();
        let payload = serde_json::json!({
            "type": "issue",
            "id": id.to_string(),
            "title": title,
            "assignee": assignee,
            "description": description,
            "status": "open",
            "labels": ["delegated", "kensho"],
            "created_at": now.to_rfc3339(),
        })
        .to_string();

        Self {
            id,
            assignee,
            description,
            status: "open".to_string(),
            payload,
            created_at: now,
        }
    }
}
