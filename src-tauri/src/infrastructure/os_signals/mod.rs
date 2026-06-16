//! Native Ubuntu/Linux desktop notifications via D-Bus (`notify-rust`).

use crate::core::{AppError, AppResult};

/// Thin wrapper around the system notification daemon.
pub struct Notifier {
    app_name: String,
}

impl Notifier {
    pub fn new(app_name: impl Into<String>) -> Self {
        Self {
            app_name: app_name.into(),
        }
    }

    /// Fire a desktop notification. Maps backend failures into `AppError`.
    pub fn notify(&self, summary: &str, body: &str) -> AppResult<()> {
        notify_rust::Notification::new()
            .appname(&self.app_name)
            .summary(summary)
            .body(body)
            .timeout(notify_rust::Timeout::Milliseconds(6000))
            .show()
            .map_err(|e| AppError::Notify(e.to_string()))?;
        tracing::debug!(summary, "desktop notification sent");
        Ok(())
    }
}

impl Default for Notifier {
    fn default() -> Self {
        Self::new("Kensho")
    }
}
