//! Domain layer: pure entities and value objects with no infrastructure deps.

pub mod event;
pub mod ids;
pub mod mind_map;
pub mod task;
pub mod user;

// Public domain API surface; some re-exports are consumed only as features land.
#[allow(unused_imports)]
pub use event::ScheduleEvent;
#[allow(unused_imports)]
pub use ids::{EventId, NodeId, TaskId, UserId};
#[allow(unused_imports)]
pub use mind_map::MindMapNode;
#[allow(unused_imports)]
pub use task::{Task, TaskPriority, TaskStatus};
#[allow(unused_imports)]
pub use user::UserProfile;
