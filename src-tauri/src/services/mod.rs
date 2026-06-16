//! Service layer: business-logic orchestrators that coordinate domain +
//! infrastructure. No Tauri types leak in here.

pub mod assistant;

pub use assistant::AssistantService;
