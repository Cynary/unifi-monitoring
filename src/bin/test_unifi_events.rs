//! Manual test harness for UniFi event streaming
//!
//! Run with: cargo run --bin test-unifi-events
//!
//! Required environment variables (or in .env):
//! - UNIFI_HOST: IP or hostname of your UniFi console
//! - UNIFI_USERNAME: Local admin username
//! - UNIFI_PASSWORD: Password

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use unifi_monitor::unifi::{
    network::connect_network_websocket, protect::connect_protect_websocket,
    system::connect_system_websocket, SeenEvents, StateTracker, UnifiConfig, UnifiEvent, UnifiSession,
};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            "debug,tokio_tungstenite=info,tungstenite=info,hyper=info,reqwest=info",
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Load .env file
    dotenvy::dotenv().ok();

    let host = std::env::var("UNIFI_HOST").expect("UNIFI_HOST environment variable required");
    let username =
        std::env::var("UNIFI_USERNAME").expect("UNIFI_USERNAME environment variable required");
    let password =
        std::env::var("UNIFI_PASSWORD").expect("UNIFI_PASSWORD environment variable required");

    println!("\n=== UniFi Event Stream Test ===\n");
    println!("Host: {}", host);
    println!("Press Ctrl+C to stop and save captured events.\n");

    let config = UnifiConfig::new(&host, &username, &password);

    // Authenticate
    println!("Authenticating...");
    let session = Arc::new(UnifiSession::login(config).await?);
    println!("  ✓ Authenticated\n");

    // Get bootstrap
    println!("Fetching bootstrap...");
    let bootstrap = session.get_protect_bootstrap().await?;
    println!("  ✓ Got lastUpdateId: {}\n", bootstrap.last_update_id);

    // Event collection
    let captured_events: Arc<Mutex<HashMap<String, Vec<serde_json::Value>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // Create channel for events
    let (event_tx, mut event_rx) = mpsc::channel::<UnifiEvent>(100);

    // Create seen events set for deduplication
    let seen_events: SeenEvents = Arc::new(Mutex::new(HashSet::new()));

    // Create state tracker to filter unchanged updates
    let state_tracker: StateTracker = Arc::new(Mutex::new(HashMap::new()));

    // Spawn event printer task
    let events_for_printer = captured_events.clone();
    let start_time = std::time::Instant::now();
    tokio::spawn(async move {
        let mut count = 0;
        while let Some(event) = event_rx.recv().await {
            count += 1;
            let elapsed = start_time.elapsed().as_secs_f32();
            let local_ts = event.timestamp.with_timezone(&chrono::Local);
            let ts = local_ts.format("%H:%M:%S");
            println!(
                "[{:3}] +{:6.2}s {} {} | {} | {}",
                count, elapsed, ts, event.source, event.event_type, event.summary
            );

            // Store event
            let mut events = events_for_printer.lock().await;
            events
                .entry(event.event_type.clone())
                .or_default()
                .push(event.raw);
        }
    });

    // Spawn Network WebSocket task
    let session_for_network = session.clone();
    let network_tx = event_tx.clone();
    let seen_for_network = seen_events.clone();
    let state_for_network = state_tracker.clone();
    let network_handle = tokio::spawn(async move {
        println!("Connecting to Network WebSocket...");
        match connect_network_websocket(&session_for_network, network_tx, seen_for_network, state_for_network).await {
            Ok(_) => println!("Network WebSocket closed normally"),
            Err(e) => println!("Network WebSocket error: {}", e),
        }
    });

    // Spawn System WebSocket task
    let session_for_system = session.clone();
    let system_tx = event_tx.clone();
    let seen_for_system = seen_events.clone();
    let state_for_system = state_tracker.clone();
    let system_handle = tokio::spawn(async move {
        println!("Connecting to System WebSocket...");
        match connect_system_websocket(&session_for_system, system_tx, seen_for_system, state_for_system).await {
            Ok(_) => println!("System WebSocket closed normally"),
            Err(e) => println!("System WebSocket error: {}", e),
        }
    });

    // Spawn Protect WebSocket task
    let session_for_protect = session.clone();
    let protect_tx = event_tx.clone();
    let seen_for_protect = seen_events.clone();
    let state_for_protect = state_tracker.clone();
    let last_update_id = bootstrap.last_update_id.clone();
    let protect_handle = tokio::spawn(async move {
        println!("Connecting to Protect WebSocket...");
        match connect_protect_websocket(&session_for_protect, &last_update_id, protect_tx, seen_for_protect, state_for_protect, None).await {
            Ok(_) => println!("Protect WebSocket closed normally"),
            Err(e) => println!("Protect WebSocket error: {}", e),
        }
    });

    println!("\nListening for events (press Ctrl+C to exit)...\n");

    // Wait for Ctrl+C
    tokio::signal::ctrl_c().await?;

    println!("\n\nShutting down...");

    // Abort WebSocket tasks
    network_handle.abort();
    system_handle.abort();
    protect_handle.abort();

    // Save events
    let events = captured_events.lock().await;
    save_events(&events);

    Ok(())
}

fn save_events(events: &HashMap<String, Vec<serde_json::Value>>) {
    let fixture_dir = "tests/fixtures/events";
    if fs::create_dir_all(fixture_dir).is_err() {
        eprintln!("Failed to create fixture directory");
        return;
    }

    if events.is_empty() {
        println!("No events captured.");
        return;
    }

    let mut total = 0;
    for (event_type, event_list) in events {
        let safe_name = event_type.replace([':', '/'], "_");
        let path = format!("{}/{}.json", fixture_dir, safe_name);

        // Anonymize events before saving
        let anonymized: Vec<_> = event_list.iter().map(|e| anonymize_event(e)).collect();

        match serde_json::to_string_pretty(&anonymized) {
            Ok(json) => {
                if fs::write(&path, json).is_ok() {
                    println!("  Saved {} events to {}", event_list.len(), path);
                    total += event_list.len();
                }
            }
            Err(e) => {
                eprintln!("  Failed to serialize {}: {}", event_type, e);
            }
        }
    }

    println!("\nTotal: {} events saved", total);
}

fn anonymize_event(event: &serde_json::Value) -> serde_json::Value {
    let mut cloned = event.clone();

    if let Some(obj) = cloned.as_object_mut() {
        // Anonymize common sensitive fields
        for key in &["mac", "ip", "hostname", "name", "id", "userId", "user"] {
            if obj.contains_key(*key) {
                obj.insert(
                    key.to_string(),
                    serde_json::Value::String(format!("REDACTED_{}", key.to_uppercase())),
                );
            }
        }

        // Recursively anonymize nested objects
        for (_, value) in obj.iter_mut() {
            if value.is_object() {
                *value = anonymize_event(value);
            }
        }
    }

    cloned
}
