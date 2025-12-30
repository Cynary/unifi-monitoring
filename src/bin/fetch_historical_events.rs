//! Fetch historical events from UniFi to find specific event types
//!
//! Run with: cargo run --bin fetch-historical-events
//!
//! Optional: pass a search term to filter events
//!   cargo run --bin fetch-historical-events -- archiving

use anyhow::Result;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use unifi_monitor::unifi::{UnifiConfig, UnifiSession};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            "info,unifi_monitor=debug,hyper=info,reqwest=info",
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

    // Optional search filter from command line
    let search_filter: Option<String> = std::env::args().nth(1);

    println!("\n=== UniFi Historical Events ===\n");
    println!("Host: {}", host);
    if let Some(ref filter) = search_filter {
        println!("Filter: {}", filter);
    }
    println!();

    let config = UnifiConfig::new(&host, &username, &password);

    // Authenticate
    println!("Authenticating...");
    let session = UnifiSession::login(config).await?;
    println!("  ✓ Authenticated\n");

    // Fetch network events
    println!("Fetching network events...");
    match session.get_network_events(Some(500)).await {
        Ok(events) => {
            println!("  ✓ Got {} network events", events.len());
            show_date_range(&events);
            println!();
            display_events(&events, &search_filter, "Network");
        }
        Err(e) => {
            println!("  ✗ Failed to fetch network events: {}\n", e);
        }
    }

    // Fetch system events
    println!("\nFetching system events...");
    match session.get_system_events(Some(500)).await {
        Ok(events) => {
            println!("  ✓ Got {} system events", events.len());
            show_date_range(&events);
            println!();
            display_events(&events, &search_filter, "System");
        }
        Err(e) => {
            println!("  ✗ Failed to fetch system events: {}\n", e);
        }
    }

    // Fetch Protect events - this is where archiving events should be
    println!("\nFetching Protect events...");
    match fetch_protect_events(&session, 500).await {
        Ok(events) => {
            println!("  ✓ Got {} Protect events", events.len());
            show_date_range(&events);
            println!();
            display_events(&events, &search_filter, "Protect");
        }
        Err(e) => {
            println!("  ✗ Failed to fetch Protect events: {}\n", e);
        }
    }

    // Also try to fetch directly from a few known endpoints
    println!("\nTrying additional endpoints...\n");

    // List of endpoints to try for system/archiving events
    let endpoints = [
        ("/proxy/protect/api/events", "Protect Events"),
        ("/proxy/protect/api/nvr", "NVR Info"),
        ("/proxy/protect/api/nvr/cloud-backup", "Cloud Backup"),
        ("/proxy/protect/api/backups", "Protect Backups"),
        ("/proxy/protect/api/cloud-archive", "Cloud Archive"),
        ("/api/system/logs", "System Logs"),
        ("/api/system/notifications", "System Notifications"),
        ("/proxy/network/api/s/default/stat/alarm", "Network Alarms"),
        ("/api/users/self/notifications", "User Notifications"),
        ("/api/notifications", "Notifications"),
    ];

    for (endpoint, name) in endpoints {
        print!("  Trying {}... ", name);
        match session.get(endpoint).await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    if let Ok(body) = resp.text().await {
                        // Try parsing as array
                        if let Ok(events) = serde_json::from_str::<Vec<serde_json::Value>>(&body) {
                            println!("✓ {} items", events.len());
                            if !events.is_empty() {
                                display_events(&events, &search_filter, name);
                            }
                        } else if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&body) {
                            // Check for nested data
                            if let Some(data) = obj.get("data").and_then(|d| d.as_array()) {
                                println!("✓ {} items (in data)", data.len());
                                if !data.is_empty() {
                                    display_events(data, &search_filter, name);
                                }
                            } else {
                                println!("✓ (object response)");
                                // Show keys if searching
                                if search_filter.is_some() {
                                    let json_str = serde_json::to_string(&obj).unwrap_or_default().to_lowercase();
                                    if json_str.contains(&search_filter.as_ref().unwrap().to_lowercase()) {
                                        println!("    Found match in response!");
                                        println!("    {}", serde_json::to_string_pretty(&obj).unwrap_or_default());
                                    }
                                }
                            }
                        } else {
                            println!("✓ (non-JSON: {} bytes)", body.len());
                        }
                    }
                } else {
                    println!("✗ {}", status);
                }
            }
            Err(e) => {
                println!("✗ {}", e);
            }
        }
    }

    // Show sample of raw events for debugging
    if search_filter.is_none() {
        println!("\n--- Sample Raw Event (first network event) ---");
        if let Ok(events) = session.get_network_events(Some(1)).await {
            if let Some(first) = events.first() {
                println!("{}", serde_json::to_string_pretty(first).unwrap_or_default());
            }
        }
    }

    Ok(())
}

/// Fetch Protect events from REST API
async fn fetch_protect_events(session: &UnifiSession, limit: u32) -> Result<Vec<serde_json::Value>> {
    // Try multiple Protect endpoints
    let endpoints = [
        format!("/proxy/protect/api/events?limit={}", limit),
        format!("/proxy/protect/api/events?orderBy=start&orderDirection=DESC&limit={}", limit),
        "/proxy/protect/api/events".to_string(),
    ];

    for endpoint in &endpoints {
        match session.get(endpoint).await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(body) = resp.text().await {
                    // Try parsing as array directly
                    if let Ok(events) = serde_json::from_str::<Vec<serde_json::Value>>(&body) {
                        return Ok(events);
                    }
                    // Try parsing as object with data field
                    if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&body) {
                        if let Some(data) = obj.get("data").and_then(|d| d.as_array()) {
                            return Ok(data.clone());
                        }
                        if let Some(events) = obj.get("events").and_then(|d| d.as_array()) {
                            return Ok(events.clone());
                        }
                    }
                }
            }
            _ => continue,
        }
    }

    // Also try the bootstrap for NVR system info including archiving status
    match session.get("/proxy/protect/api/bootstrap").await {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(body) = resp.text().await {
                if let Ok(bootstrap) = serde_json::from_str::<serde_json::Value>(&body) {
                    // Check for cloudArchive info in NVR
                    if let Some(nvr) = bootstrap.get("nvr") {
                        if let Some(cloud) = nvr.get("cloudArchive") {
                            println!("    Cloud Archive status: {}", serde_json::to_string_pretty(cloud).unwrap_or_default());
                        }
                    }
                }
            }
        }
        _ => {}
    }

    Ok(vec![])
}

/// Show the date range of events
fn show_date_range(events: &[serde_json::Value]) {
    if events.is_empty() {
        return;
    }

    let timestamps: Vec<i64> = events
        .iter()
        .filter_map(|e| {
            e.get("time")
                .and_then(|v| v.as_i64())
                .or_else(|| e.get("timestamp").and_then(|v| v.as_i64()))
                .or_else(|| e.get("start").and_then(|v| v.as_i64()))
        })
        .map(|ts| if ts > 1_000_000_000_000 { ts / 1000 } else { ts })
        .collect();

    if timestamps.is_empty() {
        println!("    (no timestamps found)");
        return;
    }

    let min_ts = timestamps.iter().min().unwrap();
    let max_ts = timestamps.iter().max().unwrap();

    let oldest = chrono::DateTime::from_timestamp(*min_ts, 0)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let newest = chrono::DateTime::from_timestamp(*max_ts, 0)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "unknown".to_string());

    println!("    Date range: {} to {}", oldest, newest);
}

fn display_events(events: &[serde_json::Value], filter: &Option<String>, source: &str) {
    let filtered: Vec<_> = if let Some(ref f) = filter {
        let f_lower = f.to_lowercase();
        events
            .iter()
            .filter(|e| {
                let json = serde_json::to_string(e).unwrap_or_default().to_lowercase();
                json.contains(&f_lower)
            })
            .collect()
    } else {
        events.iter().collect()
    };

    if filtered.is_empty() {
        if filter.is_some() {
            println!("  No {} events matching filter", source);
        } else {
            println!("  No {} events found", source);
        }
        return;
    }

    println!("  Showing {} {} events:\n", filtered.len(), source);

    for (i, event) in filtered.iter().enumerate().take(50) {
        // Try to extract common fields
        let key = event
            .get("key")
            .and_then(|v| v.as_str())
            .or_else(|| event.get("type").and_then(|v| v.as_str()))
            .or_else(|| event.get("eventType").and_then(|v| v.as_str()))
            .unwrap_or("unknown");

        let msg = event
            .get("msg")
            .and_then(|v| v.as_str())
            .or_else(|| event.get("message").and_then(|v| v.as_str()))
            .or_else(|| event.get("description").and_then(|v| v.as_str()))
            .unwrap_or("");

        let timestamp = event
            .get("time")
            .and_then(|v| v.as_i64())
            .or_else(|| event.get("timestamp").and_then(|v| v.as_i64()))
            .map(|ts| {
                // Could be milliseconds or seconds
                let ts = if ts > 1_000_000_000_000 {
                    ts / 1000
                } else {
                    ts
                };
                chrono::DateTime::from_timestamp(ts, 0)
                    .map(|dt| dt.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
                    .unwrap_or_else(|| format!("{}", ts))
            })
            .unwrap_or_else(|| "no time".to_string());

        println!(
            "  [{:3}] {} | {} | {}",
            i + 1,
            timestamp,
            key,
            if msg.len() > 80 { &msg[..80] } else { msg }
        );

        // If this matches the filter, show full JSON
        if filter.is_some() {
            println!(
                "        Full JSON: {}\n",
                serde_json::to_string_pretty(event).unwrap_or_default()
            );
        }
    }

    if filtered.len() > 50 {
        println!("\n  ... and {} more events", filtered.len() - 50);
    }
}
