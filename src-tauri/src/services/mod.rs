//! Service layer: business-logic orchestrators that coordinate domain +
//! infrastructure. No Tauri types leak in here.

pub mod assistant;
pub mod tools;

pub use assistant::AssistantService;
pub use tools::{ToolCall, ToolExecutor};
