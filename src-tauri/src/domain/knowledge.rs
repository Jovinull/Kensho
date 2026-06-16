//! `KnowledgeNote` — a permanently stored memory, full-text searchable (FTS5).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeNote {
    pub title: String,
    pub content: String,
    pub tags: String,
}

impl KnowledgeNote {
    pub fn new(
        title: impl Into<String>,
        content: impl Into<String>,
        tags: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            content: content.into(),
            tags: tags.into(),
        }
    }
}
