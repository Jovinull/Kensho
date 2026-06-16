//! OS clipboard context awareness.
//!
//! On global-hotkey invocation the backend snapshots the clipboard into a
//! shared [`ClipboardContext`]; the next turn's (invisible) system prompt
//! consumes it, so the user can copy an error and just ask "what is this?".

use std::sync::{Arc, Mutex};

/// Max clipboard characters injected (safety against huge pastes).
pub const CLIPBOARD_MAX_CHARS: usize = 2000;

/// Shared, one-shot clipboard snapshot consumed by the next prompt build.
#[derive(Clone, Default)]
pub struct ClipboardContext {
    inner: Arc<Mutex<Option<String>>>,
}

impl ClipboardContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store (or clear) the pending clipboard snapshot.
    pub fn set(&self, text: Option<String>) {
        *self.inner.lock().expect("clipboard ctx poisoned") = text;
    }

    /// Consume the pending snapshot (cleared after read).
    pub fn take(&self) -> Option<String> {
        self.inner.lock().expect("clipboard ctx poisoned").take()
    }
}

/// Read the system clipboard via `arboard`, clamped and trimmed. Returns `None`
/// on any error (no display, empty, etc.) — never panics.
pub fn read_system_clipboard() -> Option<String> {
    match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
        Ok(text) if !text.trim().is_empty() => {
            Some(text.chars().take(CLIPBOARD_MAX_CHARS).collect())
        }
        _ => None,
    }
}
