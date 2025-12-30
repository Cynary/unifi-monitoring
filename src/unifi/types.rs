use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;

/// Source of a UniFi event
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventSource {
    Protect,
    Network,
    System,
}

impl std::fmt::Display for EventSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EventSource::Protect => write!(f, "protect"),
            EventSource::Network => write!(f, "network"),
            EventSource::System => write!(f, "system"),
        }
    }
}

/// Unified event from any UniFi source
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiEvent {
    /// Unique event ID (for deduplication)
    pub id: String,

    /// When the event occurred
    pub timestamp: DateTime<Utc>,

    /// Which UniFi subsystem generated this event
    pub source: EventSource,

    /// Event type (e.g., "alarm", "nvr.storage_warning", "sta:sync")
    pub event_type: String,

    /// Human-readable summary of the event
    pub summary: String,

    /// Optional severity level
    pub severity: Option<Severity>,

    /// Full raw payload for debugging/UI
    pub raw: serde_json::Value,
}

/// Event severity levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
    Critical,
}

/// Configuration for connecting to UniFi
#[derive(Debug, Clone)]
pub struct UnifiConfig {
    /// UniFi console hostname or IP
    pub host: String,

    /// Local admin username (not SSO)
    pub username: String,

    /// Password
    pub password: String,

    /// Whether to verify TLS certificates (default: false for self-signed)
    pub verify_ssl: bool,
}

impl UnifiConfig {
    pub fn new(host: impl Into<String>, username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            username: username.into(),
            password: password.into(),
            verify_ssl: false, // UniFi uses self-signed certs by default
        }
    }

    /// Base URL for HTTP requests
    pub fn base_url(&self) -> String {
        format!("https://{}", self.host)
    }
}

/// Generate a deterministic event ID based on content.
/// This ensures the same event always gets the same ID for deduplication.
///
/// The ID is based on: source + event_type + timestamp (to the second) + key fields
pub fn generate_event_id(
    source: EventSource,
    event_type: &str,
    timestamp: DateTime<Utc>,
    key_fields: &[&str],
) -> String {
    let mut hasher = DefaultHasher::new();

    // Include source
    source.to_string().hash(&mut hasher);

    // Include event type
    event_type.hash(&mut hasher);

    // Include timestamp truncated to seconds (not millis) for slight timing tolerance
    timestamp.timestamp().hash(&mut hasher);

    // Include key identifying fields (device ID, MAC address, etc.)
    for field in key_fields {
        field.hash(&mut hasher);
    }

    // Format as hex string with source prefix for readability
    format!("{}-{:016x}", source, hasher.finish())
}

/// Extract key fields from a JSON payload for ID generation.
/// Looks for common identifier fields in order of preference.
pub fn extract_key_fields(payload: &serde_json::Value) -> Vec<String> {
    let mut fields = Vec::new();

    // Common ID fields in UniFi events
    let id_keys = ["_id", "id", "mac", "deviceId", "camera", "sensor"];

    for key in id_keys {
        if let Some(val) = payload.get(key).and_then(|v| v.as_str()) {
            fields.push(val.to_string());
            break; // Usually one ID field is enough
        }
    }

    // For events with nested data array, check first element
    if fields.is_empty() {
        if let Some(data) = payload.get("data").and_then(|d| d.as_array()).and_then(|a| a.first()) {
            for key in id_keys {
                if let Some(val) = data.get(key).and_then(|v| v.as_str()) {
                    fields.push(val.to_string());
                    break;
                }
            }
        }
    }

    fields
}
