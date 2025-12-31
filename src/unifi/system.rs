//! System WebSocket client for UniFi OS system events
//!
//! Connects to: /api/ws/system
//! Format: JSON
//! Events: Cross-application OS-level events

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async_tls_with_config,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message},
    Connector,
};
use tracing::{error, info, trace, warn};

use super::auth::UnifiSession;
use super::client::{state_changed, SeenEvents, StateTracker};
use super::error::UnifiError;
use super::types::{extract_key_fields, generate_event_id, EventSource, UnifiEvent};

/// Raw system event from WebSocket
#[derive(Debug, Deserialize)]
struct RawSystemEvent {
    /// Event type
    #[serde(rename = "type")]
    event_type: Option<String>,

    /// Event key
    key: Option<String>,

    /// Event data
    data: Option<serde_json::Value>,

    /// Timestamp
    timestamp: Option<i64>,

    /// Unique ID
    id: Option<String>,
}

/// Start the System WebSocket connection and stream events
pub async fn connect_system_websocket(
    session: &UnifiSession,
    event_tx: mpsc::Sender<UnifiEvent>,
    seen_events: SeenEvents,
    state_tracker: StateTracker,
) -> Result<(), UnifiError> {
    let ws_url = format!("wss://{}/api/ws/system", session.config.host);

    info!("Connecting to System WebSocket: {}", ws_url);

    // Build request with authentication cookie
    let mut request = ws_url
        .into_client_request()
        .map_err(|e| UnifiError::WebSocket(e.to_string()))?;

    let cookie_header = session.get_cookie_header();
    if !cookie_header.is_empty() {
        request.headers_mut().insert(
            "Cookie",
            HeaderValue::from_str(&cookie_header)
                .map_err(|e| UnifiError::WebSocket(e.to_string()))?,
        );
    }

    // Create TLS connector that accepts self-signed certs
    let tls_connector = native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .map_err(|e| UnifiError::WebSocket(e.to_string()))?;

    let connector = Connector::NativeTls(tls_connector);

    let (ws_stream, _) = connect_async_tls_with_config(request, None, false, Some(connector))
        .await
        .map_err(|e| UnifiError::WebSocket(e.to_string()))?;

    let (mut write, mut read) = ws_stream.split();

    info!("System WebSocket connected");

    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                trace!("System event: {}", text);

                match serde_json::from_str::<RawSystemEvent>(&text) {
                    Ok(raw) => {
                        if let Some((event, is_state_update, entity_id, state_data)) = parse_system_event(raw, &text) {
                            // For state update events, check if state actually changed
                            if is_state_update {
                                if !state_changed(&state_tracker, &entity_id, &state_data).await {
                                    trace!("Skipping unchanged state for {}", entity_id);
                                    continue;
                                }
                            }

                            // Deduplicate against seen events
                            let mut seen = seen_events.lock().await;
                            if !seen.insert(event.id.clone()) {
                                trace!("Skipping duplicate event: {}", event.id);
                                continue;
                            }
                            drop(seen);

                            if event_tx.send(event).await.is_err() {
                                warn!("Event channel closed, stopping System WebSocket");
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Failed to parse system event: {}", e);
                    }
                }
            }
            Ok(Message::Ping(data)) => {
                if write.send(Message::Pong(data)).await.is_err() {
                    break;
                }
            }
            Ok(Message::Close(_)) => {
                info!("System WebSocket closed by server");
                break;
            }
            Err(e) => {
                error!("System WebSocket error: {}", e);
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

/// Returns (event, is_state_update, entity_id, state_data) for state tracking
fn parse_system_event(raw: RawSystemEvent, original: &str) -> Option<(UnifiEvent, bool, String, serde_json::Value)> {
    let event_type = raw
        .event_type
        .or(raw.key)
        .unwrap_or_else(|| "unknown".to_string());

    let timestamp = raw
        .timestamp
        .map(|ms| chrono::DateTime::from_timestamp_millis(ms))
        .flatten()
        .unwrap_or_else(chrono::Utc::now);

    let summary = generate_summary(&event_type, &raw.data);

    // Check if this is a state update event (heartbeat-like)
    let is_state_update = event_type == "DEVICE_STATE_CHANGED" || event_type.contains("state");

    // Parse original for key field extraction
    let raw_json: serde_json::Value = serde_json::from_str(original).unwrap_or(serde_json::Value::Null);

    // Generate content-based ID for deduplication
    // If UniFi provides an id, use it as a key field; otherwise extract from payload
    let key_fields = if let Some(unifi_id) = &raw.id {
        vec![unifi_id.clone()]
    } else {
        extract_key_fields(&raw_json)
    };
    let key_refs: Vec<&str> = key_fields.iter().map(|s| s.as_str()).collect();
    let id = generate_event_id(EventSource::System, &event_type, timestamp, &key_refs);

    // Entity ID for state tracking
    let entity_id = raw.data
        .as_ref()
        .and_then(|d| d.get("deviceId").or_else(|| d.get("id")))
        .and_then(|v| v.as_str())
        .map(|s| format!("system:{}", s))
        .unwrap_or_else(|| format!("system:{}", id));

    // State data for comparison
    let state_data = raw.data.clone().unwrap_or(serde_json::Value::Null);

    let event = UnifiEvent {
        id,
        timestamp,
        source: EventSource::System,
        event_type,
        summary,
        severity: None,
        raw: raw_json,
    };

    Some((event, is_state_update, entity_id, state_data))
}

fn generate_summary(event_type: &str, data: &Option<serde_json::Value>) -> String {
    if let Some(data) = data {
        if let Some(msg) = data.get("message").and_then(|v| v.as_str()) {
            return msg.to_string();
        }
        if let Some(msg) = data.get("msg").and_then(|v| v.as_str()) {
            return msg.to_string();
        }
    }
    format!("System event: {}", event_type)
}
