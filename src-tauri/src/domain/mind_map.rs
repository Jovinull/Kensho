//! `MindMapNode` — a node in the assistant's long-term associative memory graph.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::ids::NodeId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MindMapNode {
    pub id: NodeId,
    /// Optional parent, forming a tree/graph of memory.
    pub parent_id: Option<NodeId>,
    pub label: String,
    pub content: String,
    /// Free-form tags for retrieval.
    pub tags: Vec<String>,
    /// Relevance/recency weight used when assembling context.
    pub weight: f32,
    pub created_at: DateTime<Utc>,
}

impl MindMapNode {
    pub fn new(label: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: NodeId::new(),
            parent_id: None,
            label: label.into(),
            content: content.into(),
            tags: Vec::new(),
            weight: 1.0,
            created_at: Utc::now(),
        }
    }
}
