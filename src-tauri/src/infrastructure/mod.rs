//! Infrastructure layer: concrete adapters to the outside world
//! (database, local LLM runtime, OS notifications).

pub mod audio;
pub mod database;
pub mod llm;
pub mod mcp_server;
pub mod os_signals;

pub use audio::Speaker;
pub use database::Database;
pub use os_signals::Notifier;
