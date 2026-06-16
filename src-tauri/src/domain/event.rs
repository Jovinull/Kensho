//! `ScheduleEvent` entity — items on the user's agenda the assistant can remind
//! the user about.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::ids::EventId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleEvent {
    pub id: EventId,
    pub title: String,
    pub notes: Option<String>,
    pub start_at: DateTime<Utc>,
    pub end_at: Option<DateTime<Utc>>,
    /// Whether the user has already been reminded of this event.
    pub reminded: bool,
    pub created_at: DateTime<Utc>,
}

impl ScheduleEvent {
    pub fn new(title: impl Into<String>, start_at: DateTime<Utc>) -> Self {
        Self {
            id: EventId::new(),
            title: title.into(),
            notes: None,
            start_at,
            end_at: None,
            reminded: false,
            created_at: Utc::now(),
        }
    }

    /// True when the event is due (or overdue) and not yet reminded.
    pub fn is_due(&self, now: DateTime<Utc>) -> bool {
        !self.reminded && self.start_at <= now
    }
}
