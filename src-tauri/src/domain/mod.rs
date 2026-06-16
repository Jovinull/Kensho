//! Domain layer: pure entities and value objects with no infrastructure deps.

pub mod chat;
pub mod delegation;
pub mod event;
pub mod ids;
pub mod knowledge;
pub mod mind_map;
pub mod task;
pub mod user;

// Public domain API surface; some re-exports are consumed only as features land.
#[allow(unused_imports)]
pub use chat::{ChatMessage, Role};
#[allow(unused_imports)]
pub use delegation::DelegatedTask;
#[allow(unused_imports)]
pub use knowledge::KnowledgeNote;
#[allow(unused_imports)]
pub use event::ScheduleEvent;
#[allow(unused_imports)]
pub use ids::{DelegateId, EventId, NodeId, TaskId, UserId};
#[allow(unused_imports)]
pub use mind_map::MindMapNode;
#[allow(unused_imports)]
pub use task::{Task, TaskPriority, TaskStatus};
#[allow(unused_imports)]
pub use user::UserProfile;
