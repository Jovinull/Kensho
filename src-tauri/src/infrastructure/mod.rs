//! Infrastructure layer: concrete adapters to the outside world
//! (database, local LLM runtime, OS notifications).

pub mod database;
pub mod llm;
pub mod os_signals;

pub use database::Database;
pub use os_signals::Notifier;
