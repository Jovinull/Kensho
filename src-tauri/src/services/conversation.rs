//! Short-term conversational memory + ChatML formatting (Qwen2.5-Instruct).
//!
//! A bounded rolling window of recent turns is kept in memory for the current
//! session. The window is rendered into Qwen's strict ChatML layout so the
//! model stays coherent (raw prompts cause hallucination loops).

use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
}

impl Role {
    fn tag(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

/// Bounded rolling window of recent user/assistant messages.
#[derive(Debug)]
pub struct History {
    messages: VecDeque<ChatMessage>,
    /// Max stored messages (≈ 2 × turns). Bounds prompt size vs. the 2048 ctx.
    max_messages: usize,
}

impl History {
    /// `max_turns` user/assistant pairs are retained.
    pub fn new(max_turns: usize) -> Self {
        Self {
            messages: VecDeque::new(),
            max_messages: max_turns * 2,
        }
    }

    pub fn push_user(&mut self, content: impl Into<String>) {
        self.push(Role::User, content.into());
    }

    pub fn push_assistant(&mut self, content: impl Into<String>) {
        self.push(Role::Assistant, content.into());
    }

    fn push(&mut self, role: Role, content: String) {
        if content.trim().is_empty() {
            return;
        }
        self.messages.push_back(ChatMessage { role, content });
        while self.messages.len() > self.max_messages {
            self.messages.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &ChatMessage> {
        self.messages.iter()
    }
}

/// Render the system prompt + rolling history into a ChatML prompt, ending with
/// an open assistant turn (no trailing `<|im_end|>`), so the model generates
/// from `<|im_start|>assistant\n`.
pub fn to_chatml(system: &str, history: &History) -> String {
    let mut out = String::with_capacity(system.len() + 128);
    out.push_str("<|im_start|>system\n");
    out.push_str(system);
    out.push_str("<|im_end|>\n");

    for msg in history.iter() {
        out.push_str("<|im_start|>");
        out.push_str(msg.role.tag());
        out.push('\n');
        out.push_str(&msg.content);
        out.push_str("<|im_end|>\n");
    }

    out.push_str("<|im_start|>assistant\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_trims_to_max_turns() {
        let mut h = History::new(2); // 4 messages max
        for i in 0..5 {
            h.push_user(format!("u{i}"));
            h.push_assistant(format!("a{i}"));
        }
        assert_eq!(h.len(), 4);
        // Oldest dropped; only the last 4 pushes survive.
        let contents: Vec<&str> = h.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, vec!["u3", "a3", "u4", "a4"]);
    }

    #[test]
    fn chatml_has_strict_structure_and_open_assistant() {
        let mut h = History::new(6);
        h.push_user("Olá");
        let prompt = to_chatml("Você é Kensho.", &h);
        assert!(prompt.starts_with("<|im_start|>system\nVocê é Kensho.<|im_end|>\n"));
        assert!(prompt.contains("<|im_start|>user\nOlá<|im_end|>\n"));
        assert!(prompt.ends_with("<|im_start|>assistant\n"));
        // Must NOT close the final assistant turn.
        assert!(!prompt.trim_end().ends_with("<|im_end|>"));
    }
}
