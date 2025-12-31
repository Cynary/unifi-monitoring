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
    Suppressed,  // Not stored, not logged
}

impl Classification {
    pub fn as_str(&self) -> &'static str {
        match self {
            Classification::Ignored => "ignored",
            Classification::Unclassified => "unclassified",
            Classification::Notify => "notify",
            Classification::Suppressed => "suppressed",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "ignored" => Some(Classification::Ignored),
            "unclassified" => Some(Classification::Unclassified),
            "notify" => Some(Classification::Notify),
            "suppressed" => Some(Classification::Suppressed),
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

            -- Authentication: Passkey credentials
            CREATE TABLE IF NOT EXISTS passkeys (
                id TEXT PRIMARY KEY,
                credential BLOB NOT NULL,
                name TEXT,
                created_at INTEGER NOT NULL
            );

            -- Authentication: Active sessions
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                expires_at INTEGER NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_expires ON sessions(expires_at);

            -- Authentication: Setup token (exists only when no passkeys registered)
            CREATE TABLE IF NOT EXISTS setup_token (
                token TEXT PRIMARY KEY,
                created_at INTEGER NOT NULL
            );

            -- Authentication: Invite tokens for adding passkeys
            CREATE TABLE IF NOT EXISTS invite_tokens (
                token TEXT PRIMARY KEY,
                expires_at INTEGER NOT NULL,
                created_at INTEGER NOT NULL
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

    /// Get classification for an event type without storing
    pub fn get_classification(&self, event_type: &str) -> rusqlite::Result<Classification> {
        Ok(self.get_rule(event_type)?.unwrap_or(Classification::Unclassified))
    }

    /// Store an event, applying classification rules
    /// Returns the classification applied
    /// Note: Suppressed events are NOT stored
    pub fn store_event(&self, event: &UnifiEvent) -> rusqlite::Result<Classification> {
        // First, look up the classification rule
        let classification = self
            .get_rule(&event.event_type)?
            .unwrap_or(Classification::Unclassified);

        // Don't store suppressed events
        if classification == Classification::Suppressed {
            return Ok(classification);
        }

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

    /// Get event payload by ID
    pub fn get_event_payload(&self, event_id: &str) -> rusqlite::Result<Option<serde_json::Value>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT payload FROM events WHERE id = ?1",
            params![event_id],
            |row| {
                let payload_str: String = row.get(0)?;
                Ok(serde_json::from_str(&payload_str).unwrap_or(serde_json::Value::Null))
            },
        )
        .optional()
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

    /// Clear last update ID for a source (used when saved ID becomes invalid)
    pub fn clear_last_update_id(&self, source: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM sync_state WHERE source = ?1",
            params![source],
        )?;
        debug!(source, "Sync state cleared");
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

    /// Get the current database file size in bytes
    pub fn get_size_bytes(&self) -> rusqlite::Result<u64> {
        let conn = self.conn.lock().unwrap();
        let page_count: u64 = conn.query_row("PRAGMA page_count", [], |row| row.get(0))?;
        let page_size: u64 = conn.query_row("PRAGMA page_size", [], |row| row.get(0))?;
        Ok(page_count * page_size)
    }

    /// Get the current database file size in MB
    pub fn get_size_mb(&self) -> rusqlite::Result<f64> {
        let bytes = self.get_size_bytes()?;
        Ok(bytes as f64 / (1024.0 * 1024.0))
    }

    /// Get total event count
    pub fn get_event_count(&self) -> rusqlite::Result<u64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
    }

    /// Delete oldest events to bring database under size limit
    /// Returns cleanup result with stats
    pub fn cleanup_by_size(&self, max_size_mb: f64) -> rusqlite::Result<CleanupResult> {
        let size_before_mb = self.get_size_mb()?;

        if size_before_mb <= max_size_mb {
            return Ok(CleanupResult {
                deleted_events: 0,
                size_before_mb,
                size_after_mb: size_before_mb,
            });
        }

        let event_count = self.get_event_count()?;
        if event_count == 0 {
            return Ok(CleanupResult {
                deleted_events: 0,
                size_before_mb,
                size_after_mb: size_before_mb,
            });
        }

        // Calculate how many events to delete based on size ratio
        // Target 80% of max to leave headroom
        let target_mb = max_size_mb * 0.8;
        let reduction_ratio = (size_before_mb - target_mb) / size_before_mb;
        let events_to_delete = ((event_count as f64) * reduction_ratio).ceil() as u64;

        info!(
            size_mb = size_before_mb,
            max_mb = max_size_mb,
            event_count,
            events_to_delete,
            "Database exceeds size limit, starting cleanup"
        );

        // Delete oldest events
        let deleted = {
            let conn = self.conn.lock().unwrap();
            conn.execute(
                r#"
                DELETE FROM events WHERE id IN (
                    SELECT id FROM events ORDER BY timestamp ASC LIMIT ?
                )
                "#,
                params![events_to_delete],
            )? as u64
        };

        debug!(deleted, "Deleted old events");

        // Run VACUUM to reclaim space (this actually shrinks the file)
        {
            let conn = self.conn.lock().unwrap();
            conn.execute("VACUUM", [])?;
        }

        let size_after_mb = self.get_size_mb()?;

        info!(
            deleted,
            size_before_mb,
            size_after_mb,
            "Database cleanup complete"
        );

        Ok(CleanupResult {
            deleted_events: deleted,
            size_before_mb,
            size_after_mb,
        })
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

    // ==================== Authentication Methods ====================

    /// Check if any passkeys are registered
    pub fn has_any_passkeys(&self) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM passkeys", [], |row| row.get(0))?;
        Ok(count > 0)
    }

    /// Store a passkey credential
    pub fn store_passkey(&self, id: &str, credential: &[u8], name: Option<&str>) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO passkeys (id, credential, name, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![id, credential, name, now],
        )?;
        debug!(id, "Passkey stored");
        Ok(())
    }

    /// Get a passkey credential by ID
    pub fn get_passkey(&self, id: &str) -> rusqlite::Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT credential FROM passkeys WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()
    }

    /// Get all passkey credentials (for authentication)
    pub fn get_all_passkeys(&self) -> rusqlite::Result<Vec<(String, Vec<u8>)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id, credential FROM passkeys")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        rows.collect()
    }

    /// List passkeys with metadata (for UI)
    pub fn list_passkeys(&self) -> rusqlite::Result<Vec<PasskeyInfo>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id, name, created_at FROM passkeys ORDER BY created_at")?;
        let rows = stmt.query_map([], |row| {
            Ok(PasskeyInfo {
                id: row.get(0)?,
                name: row.get(1)?,
                created_at: row.get(2)?,
            })
        })?;
        rows.collect()
    }

    /// Delete a passkey by ID
    pub fn delete_passkey(&self, id: &str) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let rows = conn.execute("DELETE FROM passkeys WHERE id = ?1", params![id])?;
        Ok(rows > 0)
    }

    /// Create a new session and return its ID
    pub fn create_session(&self, expiry_days: i64) -> rusqlite::Result<String> {
        use rand::Rng;
        let session_id: String = rand::thread_rng()
            .sample_iter(&rand::distributions::Alphanumeric)
            .take(64)
            .map(char::from)
            .collect();

        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp();
        let expires_at = now + (expiry_days * 24 * 60 * 60);

        conn.execute(
            "INSERT INTO sessions (id, expires_at, created_at) VALUES (?1, ?2, ?3)",
            params![session_id, expires_at, now],
        )?;

        debug!("Session created");
        Ok(session_id)
    }

    /// Validate a session ID (returns true if valid and not expired)
    pub fn validate_session(&self, session_id: &str) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sessions WHERE id = ?1 AND expires_at > ?2",
            params![session_id, now],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Delete a session
    pub fn delete_session(&self, session_id: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;
        Ok(())
    }

    /// Delete all sessions (used when all passkeys are deleted)
    pub fn delete_all_sessions(&self) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM sessions", [])?;
        Ok(())
    }

    /// Clean up expired sessions
    pub fn cleanup_expired_sessions(&self) -> rusqlite::Result<usize> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp();
        let rows = conn.execute("DELETE FROM sessions WHERE expires_at <= ?1", params![now])?;
        if rows > 0 {
            debug!(count = rows, "Cleaned up expired sessions");
        }
        Ok(rows)
    }

    /// Get setup token (if exists)
    pub fn get_setup_token(&self) -> rusqlite::Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT token FROM setup_token LIMIT 1", [], |row| row.get(0))
            .optional()
    }

    /// Set setup token (replaces any existing)
    pub fn set_setup_token(&self, token: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp();
        conn.execute("DELETE FROM setup_token", [])?;
        conn.execute(
            "INSERT INTO setup_token (token, created_at) VALUES (?1, ?2)",
            params![token, now],
        )?;
        Ok(())
    }

    /// Delete setup token
    pub fn delete_setup_token(&self) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM setup_token", [])?;
        Ok(())
    }

    /// Validate setup token
    pub fn validate_setup_token(&self, token: &str) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM setup_token WHERE token = ?1",
            params![token],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Create an invite token and return it
    pub fn create_invite_token(&self, expiry_secs: i64) -> rusqlite::Result<String> {
        use rand::Rng;
        // Generate a human-readable token (words separated by dashes)
        let words = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot",
                     "golf", "hotel", "india", "juliet", "kilo", "lima",
                     "mike", "november", "oscar", "papa", "quebec", "romeo",
                     "sierra", "tango", "uniform", "victor", "whiskey", "xray"];
        let mut rng = rand::thread_rng();
        let token: String = (0..4)
            .map(|_| words[rng.gen_range(0..words.len())])
            .collect::<Vec<_>>()
            .join("-");

        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp();
        let expires_at = now + expiry_secs;

        conn.execute(
            "INSERT INTO invite_tokens (token, expires_at, created_at) VALUES (?1, ?2, ?3)",
            params![token, expires_at, now],
        )?;

        debug!("Invite token created");
        Ok(token)
    }

    /// Validate and consume an invite token (returns true if valid)
    pub fn validate_invite_token(&self, token: &str) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp();

        // Check if valid
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM invite_tokens WHERE token = ?1 AND expires_at > ?2",
            params![token, now],
            |row| row.get(0),
        )?;

        if count > 0 {
            // Consume the token
            conn.execute("DELETE FROM invite_tokens WHERE token = ?1", params![token])?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Clean up expired invite tokens
    pub fn cleanup_expired_invite_tokens(&self) -> rusqlite::Result<usize> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp();
        let rows = conn.execute("DELETE FROM invite_tokens WHERE expires_at <= ?1", params![now])?;
        Ok(rows)
    }
}

/// Passkey info for UI display
#[derive(Debug, Clone)]
pub struct PasskeyInfo {
    pub id: String,
    pub name: Option<String>,
    pub created_at: i64,
}

/// Summary of an event type for UI display
#[derive(Debug, Clone)]
pub struct EventTypeSummary {
    pub event_type: String,
    pub count: i64,
    pub latest_timestamp: i64,
    pub classification: Classification,
}

/// Result of a cleanup operation
#[derive(Debug)]
pub struct CleanupResult {
    pub deleted_events: u64,
    pub size_before_mb: f64,
    pub size_after_mb: f64,
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
