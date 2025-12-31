//! Network WebSocket client for UniFi Network events
//!
//! Connects to: /proxy/network/wss/s/default/events
//! Format: JSON
//! Events: alarm, notification, device:sync, sta:sync, evt, backup:done

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async_tls_with_config,
    tungstenite::{
        client::IntoClientRequest,
        http::HeaderValue,
        Message,
    },
    Connector,
};
use tracing::{error, info, trace, warn};

use super::auth::UnifiSession;
use super::client::{state_changed, SeenEvents, StateTracker};
use super::error::UnifiError;
use super::types::{extract_key_fields, generate_event_id, EventSource, Severity, UnifiEvent};

/// Meta information in network events
#[derive(Debug, Deserialize)]
struct EventMeta {
    /// Event type message (e.g., "sta:sync", "device:sync")
    message: Option<String>,
}

/// Raw network event from WebSocket
#[derive(Debug, Deserialize)]
struct RawNetworkEvent {
    /// Event type (e.g., "alarm", "evt", "sta:sync")
    #[serde(rename = "type")]
    event_type: Option<String>,

    /// Event key (alternative type field)
    key: Option<String>,

    /// Meta information containing message type
    meta: Option<EventMeta>,

    /// Event data
    #[serde(default)]
    data: Vec<serde_json::Value>,

    /// Timestamp (milliseconds)
    time: Option<i64>,

    /// Unique ID
    #[serde(rename = "_id")]
    id: Option<String>,
}

/// Start the Network WebSocket connection and stream events
pub async fn connect_network_websocket(
    session: &UnifiSession,
    event_tx: mpsc::Sender<UnifiEvent>,
    seen_events: SeenEvents,
    state_tracker: StateTracker,
) -> Result<(), UnifiError> {
    let ws_url = format!(
        "wss://{}/proxy/network/wss/s/default/events",
        session.config.host
    );

    info!("Connecting to Network WebSocket: {}", ws_url);

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

    info!("Network WebSocket connected");

    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                trace!("Network event: {}", text);

                match serde_json::from_str::<RawNetworkEvent>(&text) {
                    Ok(raw) => {
                        if let Some((event, is_sync, entity_id, state_data)) = parse_network_event(raw, &text) {
                            // For sync events (sta:sync, device:sync), check if state actually changed
                            if is_sync {
                                if !state_changed(&state_tracker, &entity_id, &state_data).await {
                                    trace!("Skipping unchanged sync for {}", entity_id);
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
                                warn!("Event channel closed, stopping Network WebSocket");
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Failed to parse network event: {}", e);
                    }
                }
            }
            Ok(Message::Ping(data)) => {
                if write.send(Message::Pong(data)).await.is_err() {
                    break;
                }
            }
            Ok(Message::Close(_)) => {
                info!("Network WebSocket closed by server");
                break;
            }
            Err(e) => {
                error!("Network WebSocket error: {}", e);
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

/// Returns (event, is_sync_event, entity_id, state_data) for state tracking
fn parse_network_event(raw: RawNetworkEvent, original: &str) -> Option<(UnifiEvent, bool, String, serde_json::Value)> {
    // Event type can be in: type, key, or meta.message
    let event_type = raw
        .event_type
        .or(raw.key)
        .or_else(|| raw.meta.as_ref().and_then(|m| m.message.clone()))
        .unwrap_or_else(|| "unknown".to_string());

    let timestamp = raw
        .time
        .map(|ms| chrono::DateTime::from_timestamp_millis(ms))
        .flatten()
        .unwrap_or_else(chrono::Utc::now);

    let summary = generate_summary(&event_type, &raw.data);

    let severity = match event_type.as_str() {
        "alarm" => Some(Severity::Warning),
        _ => None,
    };

    // Check if this is a sync event (heartbeat-like)
    let is_sync = event_type == "sta:sync" || event_type == "device:sync";

    // Parse original for key field extraction
    let raw_json: serde_json::Value = serde_json::from_str(original).unwrap_or(serde_json::Value::Null);

    // Generate content-based ID for deduplication
    // If UniFi provides an _id, use it as a key field; otherwise extract from payload
    let key_fields = if let Some(unifi_id) = &raw.id {
        vec![unifi_id.clone()]
    } else {
        extract_key_fields(&raw_json)
    };
    let key_refs: Vec<&str> = key_fields.iter().map(|s| s.as_str()).collect();
    let id = generate_event_id(EventSource::Network, &event_type, timestamp, &key_refs);

    // Entity ID for state tracking - extract from data if possible
    let entity_id = if let Some(first) = raw.data.first() {
        first.get("_id")
            .or_else(|| first.get("mac"))
            .and_then(|v| v.as_str())
            .map(|s| format!("{}:{}", event_type, s))
            .unwrap_or_else(|| format!("{}:{}", event_type, id))
    } else {
        format!("{}:{}", event_type, id)
    };

    // State data for comparison
    let state_data = if raw.data.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::Array(raw.data.clone())
    };

    let event = UnifiEvent {
        id,
        timestamp,
        source: EventSource::Network,
        event_type,
        summary,
        severity,
        raw: raw_json,
    };

    Some((event, is_sync, entity_id, state_data))
}

fn generate_summary(event_type: &str, data: &[serde_json::Value]) -> String {
    match event_type {
        "sta:sync" => {
            if let Some(first) = data.first() {
                if let Some(hostname) = first.get("hostname").and_then(|v| v.as_str()) {
                    return format!("Client sync: {}", hostname);
                }
            }
            "Client sync event".to_string()
        }
        "device:sync" => "Device sync event".to_string(),
        "alarm" => {
            if let Some(first) = data.first() {
                if let Some(msg) = first.get("msg").and_then(|v| v.as_str()) {
                    return msg.to_string();
                }
            }
            "Alarm event".to_string()
        }
        "evt" => {
            if let Some(first) = data.first() {
                if let Some(msg) = first.get("msg").and_then(|v| v.as_str()) {
                    return msg.to_string();
                }
            }
            "System event".to_string()
        }
        _ => format!("{} event", event_type),
    }
}
