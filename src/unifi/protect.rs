//! Protect WebSocket client for UniFi Protect events
//!
//! Connects to: /proxy/protect/ws/updates?lastUpdateId=X
//! Format: Binary protocol
//! Events: NVR status, storage health, camera events, motion, doorbell
//!
//! Binary packet structure:
//! [Header: 8 bytes] [Action Frame: JSON] [Header: 8 bytes] [Data Frame: JSON/Buffer]
//!
//! Header format:
//! - Byte 0: Packet type (1=action, 2=payload)
//! - Byte 1: Format (1=JSON, 2=UTF8, 3=Buffer)
//! - Byte 2: Compressed (0=raw, 1=zlib deflated)
//! - Byte 3: Reserved
//! - Bytes 4-7: Payload size (big endian)

use flate2::read::ZlibDecoder;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::io::Read;
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async_tls_with_config,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message},
    Connector,
};
use tracing::{debug, error, info, trace, warn};

use super::auth::UnifiSession;
use super::client::{state_changed, SeenEvents, StateTracker};
use super::error::UnifiError;
use super::types::{generate_event_id, EventSource, Severity, UnifiEvent};

use crate::db::Database;

const PACKET_TYPE_ACTION: u8 = 1;
const PACKET_TYPE_PAYLOAD: u8 = 2;

const FORMAT_JSON: u8 = 1;
const FORMAT_UTF8: u8 = 2;
const FORMAT_BUFFER: u8 = 3;

/// Action frame from Protect WebSocket
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActionFrame {
    /// Operation type: "add" or "update"
    action: String,

    /// Device ID being updated
    id: String,

    /// Device category: camera, nvr, event, sensor, etc.
    model_key: String,

    /// Per-update UUID (can be used for dedup)
    new_update_id: Option<String>,
}

/// Packet header
#[derive(Debug)]
struct PacketHeader {
    packet_type: u8,
    format: u8,
    compressed: bool,
    payload_size: u32,
}

impl PacketHeader {
    fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 8 {
            return None;
        }

        Some(Self {
            packet_type: data[0],
            format: data[1],
            compressed: data[2] == 1,
            payload_size: u32::from_be_bytes([data[4], data[5], data[6], data[7]]),
        })
    }
}

/// Start the Protect WebSocket connection and stream events
pub async fn connect_protect_websocket(
    session: &UnifiSession,
    last_update_id: &str,
    event_tx: mpsc::Sender<UnifiEvent>,
    seen_events: SeenEvents,
    state_tracker: StateTracker,
    db: Option<Database>,
) -> Result<(), UnifiError> {
    let ws_url = format!(
        "wss://{}/proxy/protect/ws/updates?lastUpdateId={}",
        session.config.host, last_update_id
    );

    info!("Connecting to Protect WebSocket: {}", ws_url);

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

    info!("Protect WebSocket connected");

    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Binary(data)) => {
                trace!("Protect binary message: {} bytes", data.len());

                match parse_protect_packet(&data) {
                    Ok(Some((event, action_type, entity_id, state_data, new_update_id))) => {
                        // For "update" actions, check if state actually changed
                        if action_type == "update" {
                            if !state_changed(&state_tracker, &entity_id, &state_data).await {
                                trace!("Skipping unchanged update for {}", entity_id);
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

                        // Save new_update_id for resume after restart
                        if let (Some(ref db), Some(ref update_id)) = (&db, &new_update_id) {
                            if let Err(e) = db.set_last_update_id("protect", update_id) {
                                warn!(error = %e, "Failed to save lastUpdateId");
                            }
                        }

                        debug!("Protect event: {} | {}", event.event_type, event.summary);
                        if event_tx.send(event).await.is_err() {
                            warn!("Event channel closed, stopping Protect WebSocket");
                            break;
                        }
                    }
                    Ok(None) => {
                        // Packet parsed but not an event we care about
                    }
                    Err(e) => {
                        warn!("Failed to parse Protect packet: {}", e);
                    }
                }
            }
            Ok(Message::Ping(data)) => {
                if write.send(Message::Pong(data)).await.is_err() {
                    break;
                }
            }
            Ok(Message::Close(_)) => {
                info!("Protect WebSocket closed by server");
                break;
            }
            Err(e) => {
                error!("Protect WebSocket error: {}", e);
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

/// Returns (event, action_type, entity_id, state_data, new_update_id) for state tracking
fn parse_protect_packet(data: &[u8]) -> Result<Option<(UnifiEvent, String, String, serde_json::Value, Option<String>)>, UnifiError> {
    if data.len() < 8 {
        return Err(UnifiError::Protocol("Packet too short for header".into()));
    }

    // Parse first header (action frame)
    let action_header = PacketHeader::parse(&data[0..8])
        .ok_or_else(|| UnifiError::Protocol("Invalid action header".into()))?;

    if action_header.packet_type != PACKET_TYPE_ACTION {
        return Err(UnifiError::Protocol(format!(
            "Expected action frame, got type {}",
            action_header.packet_type
        )));
    }

    let action_start = 8;
    let action_end = action_start + action_header.payload_size as usize;

    if data.len() < action_end {
        return Err(UnifiError::Protocol("Packet too short for action payload".into()));
    }

    // Decompress action payload if needed
    let action_payload = decompress_if_needed(
        &data[action_start..action_end],
        action_header.compressed,
        action_header.format,
    )?;

    let action: ActionFrame = serde_json::from_slice(&action_payload)?;

    debug!(
        action = %action.action,
        model_key = %action.model_key,
        id = %action.id,
        "Protect action frame"
    );

    // Parse second header (data frame)
    if data.len() < action_end + 8 {
        return Err(UnifiError::Protocol("Packet too short for data header".into()));
    }

    let data_header = PacketHeader::parse(&data[action_end..action_end + 8])
        .ok_or_else(|| UnifiError::Protocol("Invalid data header".into()))?;

    if data_header.packet_type != PACKET_TYPE_PAYLOAD {
        return Err(UnifiError::Protocol(format!(
            "Expected payload frame, got type {}",
            data_header.packet_type
        )));
    }

    let data_start = action_end + 8;
    let data_end = data_start + data_header.payload_size as usize;

    if data.len() < data_end {
        return Err(UnifiError::Protocol("Packet too short for data payload".into()));
    }

    // Decompress data payload if needed
    let data_payload = decompress_if_needed(
        &data[data_start..data_end],
        data_header.compressed,
        data_header.format,
    )?;

    // Parse data as JSON if it's JSON format
    let data_json: serde_json::Value = if data_header.format == FORMAT_JSON {
        serde_json::from_slice(&data_payload)?
    } else {
        serde_json::Value::Null
    };

    // Entity ID for state tracking (model_key:id)
    let entity_id = format!("{}:{}", action.model_key, action.id);
    let action_type = action.action.clone();
    let new_update_id = action.new_update_id.clone();

    // Convert to UnifiEvent
    let event = create_protect_event(&action, data_json.clone())?;

    Ok(Some((event, action_type, entity_id, data_json, new_update_id)))
}

fn decompress_if_needed(data: &[u8], compressed: bool, format: u8) -> Result<Vec<u8>, UnifiError> {
    if !compressed {
        return Ok(data.to_vec());
    }

    let mut decoder = ZlibDecoder::new(data);
    let mut decompressed = Vec::new();
    decoder
        .read_to_end(&mut decompressed)
        .map_err(|e| UnifiError::Protocol(format!("Decompression failed: {}", e)))?;

    Ok(decompressed)
}

fn create_protect_event(
    action: &ActionFrame,
    data: serde_json::Value,
) -> Result<UnifiEvent, UnifiError> {
    // Extract meaningful event type:
    // - For "event" modelKey: use data.type (e.g., "continuousArchiveDestinationTermination", "motion", "ring")
    // - For others: use modelKey.action (e.g., "camera.update", "nvr.update")
    let event_type = if action.model_key == "event" {
        data.get("type")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("{}.{}", action.model_key, action.action))
    } else {
        format!("{}.{}", action.model_key, action.action)
    };

    let summary = generate_protect_summary(&action.model_key, &action.action, &data);
    let severity = determine_protect_severity(&action.model_key, &data);

    // Extract timestamp from data if available, otherwise use now
    let timestamp = data
        .get("start")
        .or_else(|| data.get("timestamp"))
        .and_then(|v| v.as_i64())
        .and_then(|ts| {
            // Could be milliseconds or seconds
            let ts = if ts > 1_000_000_000_000 { ts / 1000 } else { ts };
            chrono::DateTime::from_timestamp(ts, 0)
        })
        .unwrap_or_else(chrono::Utc::now);

    // Generate content-based ID for deduplication
    // Key fields: action.id (device/entity ID) + event-specific ID if present
    let mut key_fields: Vec<&str> = vec![&action.id];
    let event_id_str;
    if let Some(eid) = data.get("id").and_then(|v| v.as_str()) {
        event_id_str = eid.to_string();
        key_fields.push(&event_id_str);
    }
    let key_refs: Vec<&str> = key_fields.iter().map(|s| *s).collect();
    let id = generate_event_id(EventSource::Protect, &event_type, timestamp, &key_refs);

    Ok(UnifiEvent {
        id,
        timestamp,
        source: EventSource::Protect,
        event_type,
        summary,
        severity,
        raw: serde_json::json!({
            "action": action.action,
            "modelKey": action.model_key,
            "id": action.id,
            "data": data,
        }),
    })
}

fn generate_protect_summary(model_key: &str, action: &str, data: &serde_json::Value) -> String {
    match model_key {
        "nvr" => {
            if let Some(status) = data.get("systemInfo").and_then(|s| s.get("storage")) {
                if let Some(devices) = status.get("devices").and_then(|d| d.as_array()) {
                    let unhealthy: Vec<_> = devices
                        .iter()
                        .filter(|d| d.get("healthy").and_then(|h| h.as_bool()) == Some(false))
                        .collect();
                    if !unhealthy.is_empty() {
                        return format!("Storage: {} unhealthy device(s)", unhealthy.len());
                    }
                }
            }
            format!("NVR {}", action)
        }
        "camera" => {
            let name = data
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("Unknown");
            let state = data
                .get("state")
                .and_then(|s| s.as_str())
                .unwrap_or("unknown");
            format!("Camera '{}': {}", name, state)
        }
        "event" => {
            let event_type = data
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("unknown");
            format!("Protect event: {}", event_type)
        }
        "sensor" => {
            let name = data
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("Unknown");
            format!("Sensor '{}' {}", name, action)
        }
        _ => format!("{} {}", model_key, action),
    }
}

fn determine_protect_severity(model_key: &str, data: &serde_json::Value) -> Option<Severity> {
    match model_key {
        "nvr" => {
            // Check for storage issues
            if let Some(storage) = data.get("systemInfo").and_then(|s| s.get("storage")) {
                if let Some(devices) = storage.get("devices").and_then(|d| d.as_array()) {
                    let has_unhealthy = devices
                        .iter()
                        .any(|d| d.get("healthy").and_then(|h| h.as_bool()) == Some(false));
                    if has_unhealthy {
                        return Some(Severity::Error);
                    }
                }
            }
            None
        }
        "camera" => {
            let state = data.get("state").and_then(|s| s.as_str());
            match state {
                Some("DISCONNECTED") => Some(Severity::Warning),
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_packet_header_parse() {
        let data = [
            1, // packet_type = action
            1, // format = JSON
            0, // not compressed
            0, // reserved
            0, 0, 0, 10, // payload size = 10
        ];

        let header = PacketHeader::parse(&data).unwrap();
        assert_eq!(header.packet_type, PACKET_TYPE_ACTION);
        assert_eq!(header.format, FORMAT_JSON);
        assert!(!header.compressed);
        assert_eq!(header.payload_size, 10);
    }
}
