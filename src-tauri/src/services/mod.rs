//! Service layer: business-logic orchestrators that coordinate domain +
//! infrastructure. No Tauri types leak in here.

pub mod approval;
pub mod assistant;
pub mod clipboard;
pub mod conversation;
pub mod tools;

pub use assistant::AssistantService;
// Public service API surface; some re-exports are consumed only as features land.
#[allow(unused_imports)]
pub use approval::{ApprovalGate, PendingApprovals};
#[allow(unused_imports)]
pub use clipboard::ClipboardContext;
#[allow(unused_imports)]
pub use conversation::{ChatMessage, History, Role};
#[allow(unused_imports)]
pub use tools::{Tool, ToolCall, ToolRouter};
