//! Event processor - stores events and queues notifications

use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::db::{Classification, Database, StoredEvent};
use crate::unifi::UnifiEvent;

/// Event processor configuration
#[derive(Debug, Clone)]
pub struct ProcessorConfig {
    /// Maximum notification retry attempts
    pub max_notify_attempts: i32,
}

impl Default for ProcessorConfig {
    fn default() -> Self {
        Self {
            max_notify_attempts: 10,
        }
    }
}

/// Event processor - receives events, stores them, and queues notifications
pub struct EventProcessor {
    db: Database,
    config: ProcessorConfig,
    /// Channel to send events that need notification
    notify_tx: mpsc::Sender<StoredEvent>,
}

impl EventProcessor {
    /// Create a new event processor
    pub fn new(
        db: Database,
        config: ProcessorConfig,
        notify_tx: mpsc::Sender<StoredEvent>,
    ) -> Self {
        Self {
            db,
            config,
            notify_tx,
        }
    }

    /// Process an incoming event
    /// - Stores it in the database
    /// - Applies classification rules
    /// - Queues for notification if classified as "notify"
    pub async fn process(&self, event: UnifiEvent) -> Result<Classification, ProcessorError> {
        // Store event and get classification
        let classification = self
            .db
            .store_event(&event)
            .map_err(ProcessorError::Database)?;

        // Skip logging for suppressed events
        if classification != Classification::Suppressed {
            debug!(
                id = event.id,
                event_type = event.event_type,
                classification = classification.as_str(),
                "Processed event"
            );
        }

        // If notify, queue for notification
        if classification == Classification::Notify {
            let stored = StoredEvent {
                id: event.id.clone(),
                source: event.source,
                event_type: event.event_type.clone(),
                severity: event.severity,
                payload: event.raw.clone(),
                summary: event.summary.clone(),
                timestamp: event.timestamp.timestamp(),
                classification,
                notified: false,
                notify_attempts: 0,
                created_at: chrono::Utc::now().timestamp(),
            };

            if let Err(e) = self.notify_tx.send(stored).await {
                error!("Failed to queue notification: {}", e);
            }
        }

        Ok(classification)
    }

    /// Load pending notifications from database and queue them
    /// Call this on startup to handle any notifications that were queued but not sent
    pub async fn load_pending_notifications(&self) -> Result<usize, ProcessorError> {
        let pending = self
            .db
            .get_pending_notifications()
            .map_err(ProcessorError::Database)?;

        let count = pending.len();
        info!(count, "Loading pending notifications from database");

        for event in pending {
            if event.notify_attempts >= self.config.max_notify_attempts {
                warn!(
                    id = event.id,
                    attempts = event.notify_attempts,
                    "Skipping event that exceeded max notify attempts"
                );
                continue;
            }

            if let Err(e) = self.notify_tx.send(event).await {
                error!("Failed to queue pending notification: {}", e);
            }
        }

        Ok(count)
    }

    /// Get database reference for direct queries
    pub fn db(&self) -> &Database {
        &self.db
    }
}

/// Errors that can occur during event processing
#[derive(Debug, thiserror::Error)]
pub enum ProcessorError {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),
}

/// Notification sender task - sends Telegram notifications
pub struct NotificationSender {
    db: Database,
    notify_rx: mpsc::Receiver<StoredEvent>,
    telegram_token: String,
    telegram_chat_id: String,
    max_attempts: i32,
}

impl NotificationSender {
    pub fn new(
        db: Database,
        notify_rx: mpsc::Receiver<StoredEvent>,
        telegram_token: String,
        telegram_chat_id: String,
        max_attempts: i32,
    ) -> Self {
        Self {
            db,
            notify_rx,
            telegram_token,
            telegram_chat_id,
            max_attempts,
        }
    }

    /// Run the notification sender task
    pub async fn run(mut self) {
        info!("Notification sender started");

        while let Some(event) = self.notify_rx.recv().await {
            self.send_notification(event).await;
        }

        info!("Notification sender stopped");
    }

    async fn send_notification(&self, event: StoredEvent) {
        let mut attempts = event.notify_attempts;
        let mut backoff_secs = 1u64;

        loop {
            attempts += 1;

            match self.try_send_telegram(&event).await {
                Ok(()) => {
                    // Success - mark as notified and log
                    if let Err(e) = self.db.mark_notified(&event.id) {
                        error!(id = event.id, error = %e, "Failed to mark event as notified");
                    }
                    if let Err(e) = self.db.log_notification(
                        Some(&event.id),
                        Some(&event.event_type),
                        Some(&event.summary),
                        "sent",
                        None,
                    ) {
                        error!(error = %e, "Failed to log notification");
                    }
                    info!(
                        id = event.id,
                        event_type = event.event_type,
                        "Notification sent"
                    );
                    return;
                }
                Err(e) => {
                    let error_msg = e.to_string();
                    warn!(
                        id = event.id,
                        attempt = attempts,
                        error = %e,
                        "Failed to send notification"
                    );

                    // Update attempts in database
                    if let Err(db_err) = self.db.increment_notify_attempts(&event.id) {
                        error!(error = %db_err, "Failed to increment notify attempts");
                    }

                    if attempts >= self.max_attempts {
                        // Log final failure
                        if let Err(log_err) = self.db.log_notification(
                            Some(&event.id),
                            Some(&event.event_type),
                            Some(&event.summary),
                            "failed",
                            Some(&error_msg),
                        ) {
                            error!(error = %log_err, "Failed to log notification failure");
                        }
                        error!(
                            id = event.id,
                            attempts,
                            "Giving up on notification after max attempts"
                        );
                        return;
                    }

                    // Exponential backoff
                    tokio::time::sleep(tokio::time::Duration::from_secs(backoff_secs)).await;
                    backoff_secs = (backoff_secs * 2).min(60);
                }
            }
        }
    }

    async fn try_send_telegram(&self, event: &StoredEvent) -> Result<(), TelegramError> {
        let message = format!(
            "🔔 *{}*\n\n{}\n\n_Source: {} | {}_",
            escape_markdown(&event.event_type),
            escape_markdown(&event.summary),
            event.source,
            chrono::DateTime::from_timestamp(event.timestamp, 0)
                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                .unwrap_or_else(|| "unknown time".to_string())
        );

        let url = format!(
            "https://api.telegram.org/bot{}/sendMessage",
            self.telegram_token
        );

        let client = reqwest::Client::new();
        let response = client
            .post(&url)
            .json(&serde_json::json!({
                "chat_id": self.telegram_chat_id,
                "text": message,
                "parse_mode": "MarkdownV2"
            }))
            .send()
            .await
            .map_err(|e| TelegramError::Request(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(TelegramError::Api(format!("{}: {}", status, body)));
        }

        Ok(())
    }
}

/// Send a test notification to Telegram
pub async fn send_test_notification(
    db: &Database,
    telegram_token: &str,
    telegram_chat_id: &str,
) -> Result<(), TelegramError> {
    let message = "🧪 *Test Notification*\n\nThis is a test message from UniFi Monitor\\. If you see this, your Telegram integration is working correctly\\!";

    let url = format!(
        "https://api.telegram.org/bot{}/sendMessage",
        telegram_token
    );

    let client = reqwest::Client::new();
    let response = client
        .post(&url)
        .json(&serde_json::json!({
            "chat_id": telegram_chat_id,
            "text": message,
            "parse_mode": "MarkdownV2"
        }))
        .send()
        .await
        .map_err(|e| TelegramError::Request(e.to_string()))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let error_msg = format!("{}: {}", status, body);

        // Log failure
        if let Err(e) = db.log_notification(None, None, Some("Test notification"), "failed", Some(&error_msg)) {
            error!(error = %e, "Failed to log test notification failure");
        }

        return Err(TelegramError::Api(error_msg));
    }

    // Log success
    if let Err(e) = db.log_notification(None, None, Some("Test notification"), "sent", None) {
        error!(error = %e, "Failed to log test notification");
    }

    Ok(())
}

/// Escape special characters for Telegram MarkdownV2
fn escape_markdown(text: &str) -> String {
    let special_chars = ['_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!'];
    let mut result = String::with_capacity(text.len() * 2);
    for c in text.chars() {
        if special_chars.contains(&c) {
            result.push('\\');
        }
        result.push(c);
    }
    result
}

#[derive(Debug, thiserror::Error)]
pub enum TelegramError {
    #[error("Request failed: {0}")]
    Request(String),
    #[error("API error: {0}")]
    Api(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_markdown() {
        assert_eq!(escape_markdown("hello"), "hello");
        assert_eq!(escape_markdown("hello_world"), "hello\\_world");
        assert_eq!(escape_markdown("test.event"), "test\\.event");
    }
}
