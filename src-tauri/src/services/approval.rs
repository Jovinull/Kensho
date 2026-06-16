//! Human-in-the-loop approval for dangerous actions.
//!
//! Kept Tauri-free (trait + tokio oneshot registry) so tool logic stays
//! headless-testable. The production Tauri gate lives in `tauri_commands`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::oneshot;

/// Asks the user to approve a mutating action. Returns `true` if approved.
#[async_trait]
pub trait ApprovalGate: Send + Sync {
    async fn request(&self, command: &str) -> bool;
}

/// Registry of in-flight approval requests, resolved out-of-band by the
/// `approve_action` Tauri command.
#[derive(Clone, Default)]
pub struct PendingApprovals {
    inner: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
}

impl PendingApprovals {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `id`; returns a receiver that resolves when the user answers.
    pub fn register(&self, id: String) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        self.inner.lock().expect("approvals poisoned").insert(id, tx);
        rx
    }

    /// Resolve a pending request. Returns `true` if an `id` was waiting.
    pub fn resolve(&self, id: &str, approved: bool) -> bool {
        if let Some(tx) = self.inner.lock().expect("approvals poisoned").remove(id) {
            let _ = tx.send(approved);
            true
        } else {
            false
        }
    }

    pub fn pending_count(&self) -> usize {
        self.inner.lock().expect("approvals poisoned").len()
    }
}

/// Util gate that always approves (default in tests / when no UI is wired).
pub struct AlwaysApprove;

#[async_trait]
impl ApprovalGate for AlwaysApprove {
    async fn request(&self, _command: &str) -> bool {
        true
    }
}

/// Util gate that always denies.
pub struct AlwaysDeny;

#[async_trait]
impl ApprovalGate for AlwaysDeny {
    async fn request(&self, _command: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pending_request_suspends_then_resolves() {
        let pending = PendingApprovals::new();
        let rx = pending.register("cmd-1".to_string());
        assert_eq!(pending.pending_count(), 1);

        // A waiter blocks until approval arrives from elsewhere.
        let waiter = tokio::spawn(async move { rx.await.unwrap_or(false) });
        assert!(pending.resolve("cmd-1", true));
        assert!(waiter.await.expect("join"));
        assert_eq!(pending.pending_count(), 0);
    }

    #[tokio::test]
    async fn resolve_unknown_id_is_false() {
        let pending = PendingApprovals::new();
        assert!(!pending.resolve("missing", true));
    }
}
