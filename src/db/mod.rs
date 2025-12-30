//! SQLite database module for event storage and classification

use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::{debug, info};

use crate::unifi::types::{EventSource, Severity, UnifiEvent};

/// Classification states for events
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    Ignored,
    Unclassified,
    Notify,
}

impl Classification {
    pub fn as_str(&self) -> &'static str {
        match self {
            Classification::Ignored => "ignored",
            Classification::Unclassified => "unclassified",
            Classification::Notify => "notify",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "ignored" => Some(Classification::Ignored),
            "unclassified" => Some(Classification::Unclassified),
            "notify" => Some(Classification::Notify),
            _ => None,
        }
    }
}

/// Stored event with classification info
#[derive(Debug, Clone)]
pub struct StoredEvent {
    pub id: String,
    pub source: EventSource,
    pub event_type: String,
    pub severity: Option<Severity>,
    pub payload: serde_json::Value,
    pub summary: String,
    pub timestamp: i64,
    pub classification: Classification,
    pub notified: bool,
    pub notify_attempts: i32,
    pub created_at: i64,
}

/// Database handle (thread-safe)
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    /// Open or create the database at the given path
    pub fn open<P: AsRef<Path>>(path: P) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.initialize()?;
        Ok(db)
    }

    /// Open an in-memory database (for testing)
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.initialize()?;
        Ok(db)
    }

    /// Initialize database schema
    fn initialize(&self) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();

        conn.execute_batch(
            r#"
            -- Event type classification rules
            CREATE TABLE IF NOT EXISTS event_type_rules (
                event_type TEXT PRIMARY KEY,
                classification TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );

            -- All events (stored regardless of classification)
            CREATE TABLE IF NOT EXISTS events (
                id TEXT PRIMARY KEY,
                source TEXT NOT NULL,
                event_type TEXT NOT NULL,
                severity TEXT,
                payload TEXT NOT NULL,
                summary TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                classification TEXT NOT NULL DEFAULT 'unclassified',
                notified INTEGER DEFAULT 0,
                notify_attempts INTEGER DEFAULT 0,
                created_at INTEGER NOT NULL
            );

            -- Indexes for common queries
            CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events(timestamp DESC);
            CREATE INDEX IF NOT EXISTS idx_events_event_type ON events(event_type);
            CREATE INDEX IF NOT EXISTS idx_events_classification ON events(classification);
            CREATE INDEX IF NOT EXISTS idx_events_notified ON events(notified) WHERE notified = 0;

            -- Sync state for WebSocket reconnection
            CREATE TABLE IF NOT EXISTS sync_state (
                source TEXT PRIMARY KEY,
                last_update_id TEXT,
                updated_at INTEGER NOT NULL
            );
            "#,
        )?;

        info!("Database initialized");
        Ok(())
    }

    /// Get classification rule for an event type
    pub fn get_rule(&self, event_type: &str) -> rusqlite::Result<Option<Classification>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT classification FROM event_type_rules WHERE event_type = ?1",
            params![event_type],
            |row| {
                let s: String = row.get(0)?;
                Ok(Classification::from_str(&s))
            },
        )
        .optional()
        .map(|opt| opt.flatten())
    }

    /// Set classification rule for an event type
    /// Also updates all existing events of this type to the new classification
    pub fn set_rule(&self, event_type: &str, classification: Classification) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp();

        conn.execute(
            r#"
            INSERT INTO event_type_rules (event_type, classification, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?3)
            ON CONFLICT(event_type) DO UPDATE SET
                classification = excluded.classification,
                updated_at = excluded.updated_at
            "#,
            params![event_type, classification.as_str(), now],
        )?;

        // Update all existing events of this type to the new classification
        let updated = conn.execute(
            "UPDATE events SET classification = ?1 WHERE event_type = ?2",
            params![classification.as_str(), event_type],
        )?;

        debug!(event_type, classification = classification.as_str(), updated, "Rule set and events updated");
        Ok(())
    }

    /// Delete a classification rule
    /// Also reverts all existing events of this type to unclassified
    pub fn delete_rule(&self, event_type: &str) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let rows = conn.execute(
            "DELETE FROM event_type_rules WHERE event_type = ?1",
            params![event_type],
        )?;

        if rows > 0 {
            // Revert all events of this type to unclassified
            let updated = conn.execute(
                "UPDATE events SET classification = 'unclassified' WHERE event_type = ?1",
                params![event_type],
            )?;
            debug!(event_type, updated, "Rule deleted and events reverted to unclassified");
        }

        Ok(rows > 0)
    }

    /// Get all classification rules
    pub fn get_all_rules(&self) -> rusqlite::Result<Vec<(String, Classification)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT event_type, classification FROM event_type_rules ORDER BY event_type"
        )?;

        let rows = stmt.query_map([], |row| {
            let event_type: String = row.get(0)?;
            let classification_str: String = row.get(1)?;
            let classification = Classification::from_str(&classification_str)
                .unwrap_or(Classification::Unclassified);
            Ok((event_type, classification))
        })?;

        rows.collect()
    }

    /// Store an event, applying classification rules
    /// Returns the classification applied
    pub fn store_event(&self, event: &UnifiEvent) -> rusqlite::Result<Classification> {
        // First, look up the classification rule
        let classification = self
            .get_rule(&event.event_type)?
            .unwrap_or(Classification::Unclassified);

        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp();
        let payload = serde_json::to_string(&event.raw).unwrap_or_default();
        let severity = event.severity.map(|s| format!("{:?}", s).to_lowercase());

        conn.execute(
            r#"
            INSERT OR IGNORE INTO events
            (id, source, event_type, severity, payload, summary, timestamp, classification, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
            params![
                event.id,
                event.source.to_string(),
                event.event_type,
                severity,
                payload,
                event.summary,
                event.timestamp.timestamp(),
                classification.as_str(),
                now,
            ],
        )?;

        debug!(
            id = event.id,
            event_type = event.event_type,
            classification = classification.as_str(),
            "Event stored"
        );

        Ok(classification)
    }

    /// Get events that need notification (notify classification, not yet notified)
    pub fn get_pending_notifications(&self) -> rusqlite::Result<Vec<StoredEvent>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            r#"
            SELECT id, source, event_type, severity, payload, summary, timestamp,
                   classification, notified, notify_attempts, created_at
            FROM events
            WHERE classification = 'notify' AND notified = 0
            ORDER BY timestamp ASC
            "#,
        )?;

        let rows = stmt.query_map([], |row| Self::row_to_stored_event(row))?;
        rows.collect()
    }

    /// Mark an event as notified
    pub fn mark_notified(&self, event_id: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE events SET notified = 1 WHERE id = ?1",
            params![event_id],
        )?;
        debug!(event_id, "Event marked as notified");
        Ok(())
    }

    /// Increment notify attempts for an event
    pub fn increment_notify_attempts(&self, event_id: &str) -> rusqlite::Result<i32> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE events SET notify_attempts = notify_attempts + 1 WHERE id = ?1",
            params![event_id],
        )?;

        let attempts: i32 = conn.query_row(
            "SELECT notify_attempts FROM events WHERE id = ?1",
            params![event_id],
            |row| row.get(0),
        )?;

        Ok(attempts)
    }

    /// Get last update ID for a source (for WebSocket reconnection)
    pub fn get_last_update_id(&self, source: &str) -> rusqlite::Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT last_update_id FROM sync_state WHERE source = ?1",
            params![source],
            |row| row.get(0),
        )
        .optional()
    }

    /// Set last update ID for a source
    pub fn set_last_update_id(&self, source: &str, update_id: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp();

        conn.execute(
            r#"
            INSERT INTO sync_state (source, last_update_id, updated_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(source) DO UPDATE SET
                last_update_id = excluded.last_update_id,
                updated_at = excluded.updated_at
            "#,
            params![source, update_id, now],
        )?;

        debug!(source, update_id, "Sync state updated");
        Ok(())
    }

    /// Query events with filters (supports multiple classifications and event types)
    pub fn query_events(
        &self,
        classifications: &[Classification],
        event_types: &[&str],
        search: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> rusqlite::Result<Vec<StoredEvent>> {
        let conn = self.conn.lock().unwrap();

        let mut sql = String::from(
            r#"
            SELECT id, source, event_type, severity, payload, summary, timestamp,
                   classification, notified, notify_attempts, created_at
            FROM events
            WHERE 1=1
            "#,
        );

        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        // Multiple classifications (OR within)
        if !classifications.is_empty() {
            let placeholders: Vec<&str> = classifications.iter().map(|_| "?").collect();
            sql.push_str(&format!(" AND classification IN ({})", placeholders.join(",")));
            for c in classifications {
                params_vec.push(Box::new(c.as_str().to_string()));
            }
        }

        // Multiple event types (OR within)
        if !event_types.is_empty() {
            let placeholders: Vec<&str> = event_types.iter().map(|_| "?").collect();
            sql.push_str(&format!(" AND event_type IN ({})", placeholders.join(",")));
            for et in event_types {
                params_vec.push(Box::new(et.to_string()));
            }
        }

        if let Some(q) = search {
            // Search across event_type, summary, source, and payload (case-insensitive)
            sql.push_str(" AND (event_type LIKE ? OR summary LIKE ? OR source LIKE ? OR payload LIKE ?)");
            let pattern = format!("%{}%", q);
            params_vec.push(Box::new(pattern.clone()));
            params_vec.push(Box::new(pattern.clone()));
            params_vec.push(Box::new(pattern.clone()));
            params_vec.push(Box::new(pattern));
        }

        sql.push_str(" ORDER BY timestamp DESC, id DESC LIMIT ? OFFSET ?");
        params_vec.push(Box::new(limit as i64));
        params_vec.push(Box::new(offset as i64));

        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_refs.as_slice(), |row| Self::row_to_stored_event(row))?;

        rows.collect()
    }

    /// Count events matching filters
    pub fn count_events(
        &self,
        classifications: &[Classification],
        event_types: &[&str],
        search: Option<&str>,
    ) -> rusqlite::Result<i64> {
        let conn = self.conn.lock().unwrap();

        let mut sql = String::from("SELECT COUNT(*) FROM events WHERE 1=1");
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if !classifications.is_empty() {
            let placeholders: Vec<&str> = classifications.iter().map(|_| "?").collect();
            sql.push_str(&format!(" AND classification IN ({})", placeholders.join(",")));
            for c in classifications {
                params_vec.push(Box::new(c.as_str().to_string()));
            }
        }

        if !event_types.is_empty() {
            let placeholders: Vec<&str> = event_types.iter().map(|_| "?").collect();
            sql.push_str(&format!(" AND event_type IN ({})", placeholders.join(",")));
            for et in event_types {
                params_vec.push(Box::new(et.to_string()));
            }
        }

        if let Some(q) = search {
            sql.push_str(" AND (event_type LIKE ? OR summary LIKE ? OR source LIKE ? OR payload LIKE ?)");
            let pattern = format!("%{}%", q);
            params_vec.push(Box::new(pattern.clone()));
            params_vec.push(Box::new(pattern.clone()));
            params_vec.push(Box::new(pattern.clone()));
            params_vec.push(Box::new(pattern));
        }

        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();

        conn.query_row(&sql, params_refs.as_slice(), |row| row.get(0))
    }

    /// Get distinct event types with counts and their classification
    pub fn get_event_type_summary(&self) -> rusqlite::Result<Vec<EventTypeSummary>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            r#"
            SELECT
                e.event_type,
                COUNT(*) as count,
                MAX(e.timestamp) as latest,
                COALESCE(r.classification, 'unclassified') as classification
            FROM events e
            LEFT JOIN event_type_rules r ON e.event_type = r.event_type
            GROUP BY e.event_type
            ORDER BY latest DESC
            "#,
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(EventTypeSummary {
                event_type: row.get(0)?,
                count: row.get(1)?,
                latest_timestamp: row.get(2)?,
                classification: Classification::from_str(&row.get::<_, String>(3)?)
                    .unwrap_or(Classification::Unclassified),
            })
        })?;

        rows.collect()
    }

    fn row_to_stored_event(row: &rusqlite::Row) -> rusqlite::Result<StoredEvent> {
        let source_str: String = row.get(1)?;
        let source = match source_str.as_str() {
            "protect" => EventSource::Protect,
            "network" => EventSource::Network,
            "system" => EventSource::System,
            _ => EventSource::System,
        };

        let severity_str: Option<String> = row.get(3)?;
        let severity = severity_str.and_then(|s| match s.as_str() {
            "info" => Some(Severity::Info),
            "warning" => Some(Severity::Warning),
            "error" => Some(Severity::Error),
            "critical" => Some(Severity::Critical),
            _ => None,
        });

        let payload_str: String = row.get(4)?;
        let payload = serde_json::from_str(&payload_str).unwrap_or(serde_json::Value::Null);

        let classification_str: String = row.get(7)?;
        let classification = Classification::from_str(&classification_str)
            .unwrap_or(Classification::Unclassified);

        Ok(StoredEvent {
            id: row.get(0)?,
            source,
            event_type: row.get(2)?,
            severity,
            payload,
            summary: row.get(5)?,
            timestamp: row.get(6)?,
            classification,
            notified: row.get::<_, i32>(8)? != 0,
            notify_attempts: row.get(9)?,
            created_at: row.get(10)?,
        })
    }
}

/// Summary of an event type for UI display
#[derive(Debug, Clone)]
pub struct EventTypeSummary {
    pub event_type: String,
    pub count: i64,
    pub latest_timestamp: i64,
    pub classification: Classification,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_query_rules() {
        let db = Database::open_in_memory().unwrap();

        // No rule initially
        assert_eq!(db.get_rule("test.event").unwrap(), None);

        // Set rule
        db.set_rule("test.event", Classification::Notify).unwrap();
        assert_eq!(db.get_rule("test.event").unwrap(), Some(Classification::Notify));

        // Update rule
        db.set_rule("test.event", Classification::Ignored).unwrap();
        assert_eq!(db.get_rule("test.event").unwrap(), Some(Classification::Ignored));

        // Delete rule
        assert!(db.delete_rule("test.event").unwrap());
        assert_eq!(db.get_rule("test.event").unwrap(), None);
    }

    #[test]
    fn test_store_and_query_events() {
        let db = Database::open_in_memory().unwrap();

        let event = UnifiEvent {
            id: "test-123".to_string(),
            timestamp: chrono::Utc::now(),
            source: EventSource::Protect,
            event_type: "motion".to_string(),
            summary: "Motion detected".to_string(),
            severity: Some(Severity::Info),
            raw: serde_json::json!({"test": true}),
        };

        // Store without rule -> unclassified
        let classification = db.store_event(&event).unwrap();
        assert_eq!(classification, Classification::Unclassified);

        // Query back
        let events = db.query_events(&[], &[], None, 10, 0).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "test-123");
        assert_eq!(events[0].classification, Classification::Unclassified);
    }

    #[test]
    fn test_sync_state() {
        let db = Database::open_in_memory().unwrap();

        assert_eq!(db.get_last_update_id("protect").unwrap(), None);

        db.set_last_update_id("protect", "abc123").unwrap();
        assert_eq!(db.get_last_update_id("protect").unwrap(), Some("abc123".to_string()));

        db.set_last_update_id("protect", "def456").unwrap();
        assert_eq!(db.get_last_update_id("protect").unwrap(), Some("def456".to_string()));
    }
}
