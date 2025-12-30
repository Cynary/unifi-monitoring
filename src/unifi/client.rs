use futures_util::Stream;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, instrument, trace, warn};

use super::auth::UnifiSession;
use super::error::UnifiError;
use super::network::connect_network_websocket;
use super::protect::connect_protect_websocket;
use super::system::connect_system_websocket;
use super::types::{extract_key_fields, generate_event_id, EventSource, Severity, UnifiConfig, UnifiEvent};

use crate::db::Database;

/// Shared state for event deduplication (by event ID)
pub type SeenEvents = Arc<Mutex<HashSet<String>>>;

/// Shared state for tracking entity states (to filter unchanged updates)
/// Key: entity_id, Value: hash of last known state
pub type StateTracker = Arc<Mutex<HashMap<String, u64>>>;

/// Compute a hash of a JSON value for state comparison
pub fn hash_state(value: &serde_json::Value) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // Serialize to canonical JSON for consistent hashing
    let s = serde_json::to_string(value).unwrap_or_default();
    s.hash(&mut hasher);
    hasher.finish()
}

/// Check if state has changed for an entity, returns true if changed (or new)
pub async fn state_changed(tracker: &StateTracker, entity_id: &str, new_state: &serde_json::Value) -> bool {
    let new_hash = hash_state(new_state);
    let mut states = tracker.lock().await;

    match states.get(entity_id) {
        Some(&old_hash) if old_hash == new_hash => {
            trace!("State unchanged for {}", entity_id);
            false
        }
        _ => {
            states.insert(entity_id.to_string(), new_hash);
            true
        }
    }
}

/// Unified client for all UniFi event sources
pub struct UnifiClient {
    session: Arc<UnifiSession>,
    event_rx: mpsc::Receiver<UnifiEvent>,
    handles: Vec<JoinHandle<()>>,
    seen_events: SeenEvents,
    state_tracker: StateTracker,
    db: Option<Database>,
}

impl UnifiClient {
    /// Connect to UniFi console and start event collection
    #[instrument(skip(config, db), fields(host = %config.host))]
    pub async fn connect(config: UnifiConfig, db: Option<Database>) -> Result<Self, UnifiError> {
        // Authenticate
        let session = Arc::new(UnifiSession::login(config).await?);

        // Get bootstrap for Protect WebSocket (provides fallback lastUpdateId)
        let bootstrap = session.get_protect_bootstrap().await?;
        let bootstrap_update_id = bootstrap.last_update_id.clone();
        info!(bootstrap_update_id = %bootstrap_update_id, "Got Protect bootstrap");

        // Create event channel
        let (event_tx, event_rx) = mpsc::channel(1000);

        // Create shared set for deduplication
        let seen_events: SeenEvents = Arc::new(Mutex::new(HashSet::new()));

        // Create state tracker to filter unchanged "update" events
        let state_tracker: StateTracker = Arc::new(Mutex::new(HashMap::new()));

        let mut handles = Vec::new();

        // IMPORTANT: Start WebSockets BEFORE REST fetch to avoid missing events.
        // Any events that arrive via both WebSocket and REST will be deduplicated
        // by content-based IDs (same content = same ID = caught by seen_events or DB).
        //
        // Order matters:
        // 1. WebSockets connect and start receiving real-time events
        // 2. REST fetch gets historical events (may overlap with WebSocket)
        // 3. Content-based IDs ensure duplicates are filtered out
        //
        // If we did REST first, there would be a gap between REST completing and
        // WebSocket connecting where events could be missed.

        // Start Network WebSocket
        let session_clone = session.clone();
        let tx_clone = event_tx.clone();
        let seen_clone = seen_events.clone();
        let state_clone = state_tracker.clone();
        handles.push(tokio::spawn(async move {
            loop {
                info!("Starting Network WebSocket connection");
                match connect_network_websocket(&session_clone, tx_clone.clone(), seen_clone.clone(), state_clone.clone()).await {
                    Ok(_) => info!("Network WebSocket disconnected normally"),
                    Err(e) => error!("Network WebSocket error: {}", e),
                }
                warn!("Network WebSocket disconnected, reconnecting in 5s...");
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        }));

        // Start System WebSocket
        let session_clone = session.clone();
        let tx_clone = event_tx.clone();
        let seen_clone = seen_events.clone();
        let state_clone = state_tracker.clone();
        handles.push(tokio::spawn(async move {
            loop {
                info!("Starting System WebSocket connection");
                match connect_system_websocket(&session_clone, tx_clone.clone(), seen_clone.clone(), state_clone.clone()).await {
                    Ok(_) => info!("System WebSocket disconnected normally"),
                    Err(e) => error!("System WebSocket error: {}", e),
                }
                warn!("System WebSocket disconnected, reconnecting in 5s...");
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        }));

        // Start Protect WebSocket
        let session_clone = session.clone();
        let tx_clone = event_tx.clone();
        let seen_clone = seen_events.clone();
        let state_clone = state_tracker.clone();
        let db_clone = db.clone();
        let bootstrap_update_id = bootstrap_update_id.clone();
        handles.push(tokio::spawn(async move {
            loop {
                // Query database for latest lastUpdateId on each reconnect
                // This ensures we resume from where we actually left off, not startup position
                let current_update_id = if let Some(ref db) = db_clone {
                    match db.get_last_update_id("protect") {
                        Ok(Some(saved_id)) => {
                            info!(saved_id = %saved_id, "Resuming Protect from saved lastUpdateId");
                            saved_id
                        }
                        Ok(None) => {
                            info!(update_id = %bootstrap_update_id, "No saved lastUpdateId, using bootstrap");
                            bootstrap_update_id.clone()
                        }
                        Err(e) => {
                            warn!(error = %e, "Failed to load lastUpdateId, using bootstrap");
                            bootstrap_update_id.clone()
                        }
                    }
                } else {
                    bootstrap_update_id.clone()
                };

                info!("Starting Protect WebSocket connection");
                match connect_protect_websocket(&session_clone, &current_update_id, tx_clone.clone(), seen_clone.clone(), state_clone.clone(), db_clone.clone())
                    .await
                {
                    Ok(_) => info!("Protect WebSocket disconnected normally"),
                    Err(e) => error!("Protect WebSocket error: {}", e),
                }
                warn!("Protect WebSocket disconnected, reconnecting in 5s...");
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        }));

        // Now fetch historical events from REST API
        // These will be deduplicated against any events already received via WebSocket
        info!("Fetching historical events from REST API...");
        let historical_count = Self::fetch_historical_events(&session, &event_tx, &seen_events).await;
        info!(count = historical_count, "Loaded historical events");

        Ok(Self {
            session,
            event_rx,
            handles,
            seen_events,
            state_tracker,
            db,
        })
    }

    /// Fetch historical events from REST API and send through channel
    async fn fetch_historical_events(
        session: &UnifiSession,
        event_tx: &mpsc::Sender<UnifiEvent>,
        seen_events: &SeenEvents,
    ) -> usize {
        let mut count = 0;

        // Fetch network events
        match session.get_network_events(Some(1000)).await {
            Ok(events) => {
                for raw in events {
                    if let Some(event) = Self::parse_network_event(&raw) {
                        let mut seen = seen_events.lock().await;
                        if seen.insert(event.id.clone()) {
                            drop(seen);
                            if event_tx.send(event).await.is_err() {
                                warn!("Event channel closed while loading historical events");
                                return count;
                            }
                            count += 1;
                        }
                    }
                }
                debug!(count = count, "Loaded network events");
            }
            Err(e) => {
                warn!("Failed to fetch network events: {}", e);
            }
        }

        // Fetch system events
        match session.get_system_events(Some(500)).await {
            Ok(events) => {
                let before = count;
                for raw in events {
                    if let Some(event) = Self::parse_system_event(&raw) {
                        let mut seen = seen_events.lock().await;
                        if seen.insert(event.id.clone()) {
                            drop(seen);
                            if event_tx.send(event).await.is_err() {
                                warn!("Event channel closed while loading historical events");
                                return count;
                            }
                            count += 1;
                        }
                    }
                }
                debug!(count = count - before, "Loaded system events");
            }
            Err(e) => {
                warn!("Failed to fetch system events: {}", e);
            }
        }

        count
    }

    /// Parse a raw network event from REST API
    fn parse_network_event(raw: &serde_json::Value) -> Option<UnifiEvent> {
        let event_type = raw.get("key")
            .and_then(|v| v.as_str())
            .or_else(|| raw.get("type").and_then(|v| v.as_str()))
            .unwrap_or("unknown");

        let timestamp = raw.get("time")
            .and_then(|v| v.as_i64())
            .or_else(|| raw.get("datetime").and_then(|v| v.as_i64()))
            .and_then(|ts| {
                // Could be milliseconds or seconds
                let ts = if ts > 1_000_000_000_000 { ts / 1000 } else { ts };
                chrono::DateTime::from_timestamp(ts, 0)
            })
            .unwrap_or_else(chrono::Utc::now);

        let msg = raw.get("msg").and_then(|v| v.as_str()).unwrap_or("");
        let summary = if msg.is_empty() {
            format!("{} event", event_type)
        } else {
            msg.to_string()
        };

        let severity = match event_type {
            "EVT_LAN_CLIENT_BLOCKED" | "EVT_AP_LOST_CONTACT" => Some(Severity::Warning),
            _ => None,
        };

        // Generate content-based ID for deduplication
        // Use UniFi's _id if available, otherwise extract key fields
        let key_fields = if let Some(unifi_id) = raw.get("_id").and_then(|v| v.as_str()) {
            vec![unifi_id.to_string()]
        } else {
            extract_key_fields(raw)
        };
        let key_refs: Vec<&str> = key_fields.iter().map(|s| s.as_str()).collect();
        let id = generate_event_id(EventSource::Network, event_type, timestamp, &key_refs);

        Some(UnifiEvent {
            id,
            timestamp,
            source: EventSource::Network,
            event_type: event_type.to_string(),
            summary,
            severity,
            raw: raw.clone(),
        })
    }

    /// Parse a raw system event from REST API
    fn parse_system_event(raw: &serde_json::Value) -> Option<UnifiEvent> {
        let event_type = raw.get("key")
            .and_then(|v| v.as_str())
            .or_else(|| raw.get("type").and_then(|v| v.as_str()))
            .or_else(|| raw.get("eventType").and_then(|v| v.as_str()))
            .unwrap_or("unknown");

        let timestamp = raw.get("time")
            .and_then(|v| v.as_i64())
            .or_else(|| raw.get("timestamp").and_then(|v| v.as_i64()))
            .and_then(|ts| {
                let ts = if ts > 1_000_000_000_000 { ts / 1000 } else { ts };
                chrono::DateTime::from_timestamp(ts, 0)
            })
            .unwrap_or_else(chrono::Utc::now);

        let msg = raw.get("msg")
            .and_then(|v| v.as_str())
            .or_else(|| raw.get("message").and_then(|v| v.as_str()))
            .or_else(|| raw.get("description").and_then(|v| v.as_str()))
            .unwrap_or("");

        let summary = if msg.is_empty() {
            format!("{} event", event_type)
        } else {
            msg.to_string()
        };

        // Generate content-based ID for deduplication
        // Use UniFi's _id if available, otherwise extract key fields
        let key_fields = if let Some(unifi_id) = raw.get("_id").and_then(|v| v.as_str()) {
            vec![unifi_id.to_string()]
        } else {
            extract_key_fields(raw)
        };
        let key_refs: Vec<&str> = key_fields.iter().map(|s| s.as_str()).collect();
        let id = generate_event_id(EventSource::System, event_type, timestamp, &key_refs);

        Some(UnifiEvent {
            id,
            timestamp,
            source: EventSource::System,
            event_type: event_type.to_string(),
            summary,
            severity: None,
            raw: raw.clone(),
        })
    }

    /// Get the event stream
    pub fn events(&mut self) -> impl Stream<Item = UnifiEvent> + '_ {
        futures_util::stream::poll_fn(move |cx| self.event_rx.poll_recv(cx))
    }

    /// Get a reference to the session for direct API calls
    pub fn session(&self) -> &UnifiSession {
        &self.session
    }
}

impl Drop for UnifiClient {
    fn drop(&mut self) {
        for handle in &self.handles {
            handle.abort();
        }
    }
}
