//! Manual test harness for UniFi authentication
//!
//! Run with: cargo run --bin test-unifi-auth
//!
//! Required environment variables (or in .env):
//! - UNIFI_HOST: IP or hostname of your UniFi console
//! - UNIFI_USERNAME: Local admin username
//! - UNIFI_PASSWORD: Password

use anyhow::Result;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

// Access the unifi module from the main crate
use unifi_monitor::unifi::{BootstrapResponse, UnifiConfig, UnifiSession};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging with debug level
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new("debug"))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Load .env file
    dotenvy::dotenv().ok();

    let host = std::env::var("UNIFI_HOST").expect("UNIFI_HOST environment variable required");
    let username =
        std::env::var("UNIFI_USERNAME").expect("UNIFI_USERNAME environment variable required");
    let password =
        std::env::var("UNIFI_PASSWORD").expect("UNIFI_PASSWORD environment variable required");

    println!("\n=== UniFi Authentication Test ===\n");
    println!("Host: {}", host);
    println!("Username: {}", username);
    println!();

    let config = UnifiConfig::new(&host, &username, &password);

    // Test authentication
    println!("Step 1: Authenticating...");
    let session = match UnifiSession::login(config).await {
        Ok(session) => {
            println!("  ✓ Authentication successful!");
            println!("  CSRF Token: {}...", &session.csrf_token[..20.min(session.csrf_token.len())]);
            session
        }
        Err(e) => {
            println!("  ✗ Authentication failed: {}", e);
            return Err(e.into());
        }
    };

    // Test bootstrap fetch
    println!("\nStep 2: Fetching Protect bootstrap...");
    match session.get_protect_bootstrap().await {
        Ok(bootstrap) => {
            println!("  ✓ Bootstrap fetched successfully!");
            println!("  Last Update ID: {}", bootstrap.last_update_id);
            println!("  Cameras: {}", bootstrap.cameras.len());
            if let Some(nvr) = &bootstrap.nvr {
                println!("  NVR: {} ({})", nvr.name, nvr.version);
            }

            // Save bootstrap response for test fixtures (anonymized)
            let fixture_path = "tests/fixtures/bootstrap_response.json";
            if let Ok(anonymized) = anonymize_bootstrap(&bootstrap) {
                if std::fs::create_dir_all("tests/fixtures").is_ok() {
                    if std::fs::write(fixture_path, anonymized).is_ok() {
                        println!("\n  Saved anonymized fixture to {}", fixture_path);
                    }
                }
            }
        }
        Err(e) => {
            println!("  ✗ Bootstrap fetch failed: {}", e);
            return Err(e.into());
        }
    }

    println!("\n=== All tests passed! ===\n");

    Ok(())
}

fn anonymize_bootstrap(bootstrap: &BootstrapResponse) -> Result<String> {
    let mut json = serde_json::to_value(bootstrap)?;

    // Anonymize sensitive fields
    if let Some(obj) = json.as_object_mut() {
        obj.insert(
            "lastUpdateId".to_string(),
            serde_json::Value::String("REDACTED_UPDATE_ID".to_string()),
        );

        if let Some(nvr) = obj.get_mut("nvr").and_then(|v| v.as_object_mut()) {
            nvr.insert(
                "id".to_string(),
                serde_json::Value::String("REDACTED_NVR_ID".to_string()),
            );
            nvr.insert(
                "name".to_string(),
                serde_json::Value::String("Test NVR".to_string()),
            );
        }

        // Replace camera array with count placeholder
        if let Some(cameras) = obj.get("cameras").and_then(|v| v.as_array()) {
            let count = cameras.len();
            obj.insert(
                "cameras".to_string(),
                serde_json::json!([{"_placeholder": format!("{} cameras redacted", count)}]),
            );
        }
    }

    Ok(serde_json::to_string_pretty(&json)?)
}
