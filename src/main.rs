use futures_util::StreamExt;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use unifi_monitor::db::{Classification, Database};
use unifi_monitor::processor::{EventProcessor, NotificationSender, ProcessorConfig};
use unifi_monitor::unifi::{UnifiClient, UnifiConfig};
use unifi_monitor::web::{self, auth::AuthState, FullAppState, SseEvent, TelegramConfig};

/// Clean up old log files to stay under size limit
fn cleanup_logs(log_dir: &str, max_size_mb: u64) -> anyhow::Result<()> {
    let max_size_bytes = max_size_mb * 1024 * 1024;
    let log_path = Path::new(log_dir);

    if !log_path.exists() {
        return Ok(());
    }

    // Collect log files with their metadata
    let mut files: Vec<(std::path::PathBuf, std::fs::Metadata)> = std::fs::read_dir(log_path)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            name.starts_with("unifi-monitor.log")
        })
        .filter_map(|entry| entry.metadata().ok().map(|meta| (entry.path(), meta)))
        .collect();

    // Sort by modification time (oldest first)
    files.sort_by(|a, b| {
        a.1.modified()
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
            .cmp(&b.1.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH))
    });

    // Calculate total size
    let total_size: u64 = files.iter().map(|(_, meta)| meta.len()).sum();

    if total_size <= max_size_bytes {
        return Ok(());
    }

    // Delete oldest files until under limit
    let mut current_size = total_size;
    let mut deleted_count = 0;

    for (path, meta) in files {
        if current_size <= max_size_bytes {
            break;
        }

        let file_size = meta.len();
        if std::fs::remove_file(&path).is_ok() {
            current_size -= file_size;
            deleted_count += 1;
            tracing::debug!("Deleted old log file: {}", path.display());
        }
    }

    if deleted_count > 0 {
        tracing::info!(
            "Log cleanup: deleted {} files, size {:.1}MB -> {:.1}MB",
            deleted_count,
            total_size as f64 / 1024.0 / 1024.0,
            current_size as f64 / 1024.0 / 1024.0
        );
    }

    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env file first (before logging setup to read LOG_DIR)
    dotenvy::dotenv().ok();

    // Log configuration
    let log_dir = std::env::var("LOG_DIR").unwrap_or_else(|_| "data/logs".to_string());
    let log_max_size_mb: u64 = std::env::var("LOG_MAX_SIZE_MB")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(512);

    // Ensure log directory exists
    std::fs::create_dir_all(&log_dir)?;

    // Create file appender with daily rotation
    let file_appender = tracing_appender::rolling::daily(&log_dir, "unifi-monitor.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    // Initialize logging to file
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| {
                "info,unifi_monitor=debug,tokio_tungstenite=info,tungstenite=info,hyper=info,reqwest=info".to_string()
            }),
        ))
        .with(tracing_subscriber::fmt::layer().with_writer(non_blocking))
        .init();

    tracing::info!("UniFi Monitor starting...");
    tracing::info!("Logging to {} (max {}MB)", log_dir, log_max_size_mb);

    // Run log cleanup on startup
    if let Err(e) = cleanup_logs(&log_dir, log_max_size_mb) {
        tracing::warn!("Log cleanup on startup failed: {}", e);
    }

    // UniFi configuration
    let host = std::env::var("UNIFI_HOST").expect("UNIFI_HOST required");
    let username = std::env::var("UNIFI_USERNAME").expect("UNIFI_USERNAME required");
    let password = std::env::var("UNIFI_PASSWORD").expect("UNIFI_PASSWORD required");

    // Telegram configuration (optional for now)
    let telegram_token = std::env::var("TELEGRAM_BOT_TOKEN").ok();
    let telegram_chat_id = std::env::var("TELEGRAM_CHAT_ID").ok();

    // Database path
    let db_path = std::env::var("DATABASE_PATH").unwrap_or_else(|_| "data/unifi-monitor.db".to_string());

    // Database max size (MB)
    let db_max_size_mb: f64 = std::env::var("DB_MAX_SIZE_MB")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(512.0);

    // Ensure data directory exists
    if let Some(parent) = std::path::Path::new(&db_path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Open database
    tracing::info!("Opening database at {}...", db_path);
    let db = Database::open(&db_path)?;

    // Run cleanup on startup
    tracing::info!("Checking database size (max {}MB)...", db_max_size_mb);
    match db.cleanup_by_size(db_max_size_mb) {
        Ok(result) => {
            if result.deleted_events > 0 {
                tracing::info!(
                    "Startup cleanup: deleted {} events, size {:.1}MB -> {:.1}MB",
                    result.deleted_events,
                    result.size_before_mb,
                    result.size_after_mb
                );
            } else {
                tracing::info!("Database size OK: {:.1}MB", result.size_before_mb);
            }
        }
        Err(e) => {
            tracing::warn!("Failed to run startup cleanup: {}", e);
        }
    }

    // Spawn periodic cleanup task (every hour)
    let cleanup_db = db.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
        interval.tick().await; // Skip immediate tick
        loop {
            interval.tick().await;
            tracing::debug!("Running periodic database cleanup check");
            match cleanup_db.cleanup_by_size(db_max_size_mb) {
                Ok(result) => {
                    if result.deleted_events > 0 {
                        tracing::info!(
                            "Periodic cleanup: deleted {} events, size {:.1}MB -> {:.1}MB",
                            result.deleted_events,
                            result.size_before_mb,
                            result.size_after_mb
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!("Periodic cleanup failed: {}", e);
                }
            }
        }
    });

    // Spawn periodic log cleanup task (every hour)
    let log_dir_cleanup = log_dir.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
        interval.tick().await; // Skip immediate tick
        loop {
            interval.tick().await;
            if let Err(e) = cleanup_logs(&log_dir_cleanup, log_max_size_mb) {
                tracing::warn!("Periodic log cleanup failed: {}", e);
            }
        }
    });

    // Create notification channel
    let (notify_tx, notify_rx) = mpsc::channel(100);

    // Create broadcast channel for SSE (live event updates to frontend)
    let (sse_tx, _) = broadcast::channel::<SseEvent>(100);

    // Create event processor
    let processor = EventProcessor::new(db.clone(), ProcessorConfig::default(), notify_tx);

    // Load any pending notifications from database
    processor.load_pending_notifications().await?;

    // Start web server with authentication
    let listen_addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let static_dir = std::env::var("STATIC_DIR").ok();

    // Create WebAuthn config
    let webauthn = web::create_webauthn_from_env()
        .expect("Failed to create WebAuthn config");

    // Determine if we should use secure cookies (HTTPS)
    let rp_origin = std::env::var("RP_ORIGIN").unwrap_or_else(|_| "http://localhost:8080".to_string());
    let use_secure_cookies = rp_origin.starts_with("https://");
    if use_secure_cookies {
        tracing::info!("Secure cookies enabled (HTTPS detected)");
    } else {
        tracing::warn!("Secure cookies disabled (HTTP mode - use HTTPS in production)");
    }

    // Create auth state
    let reg_challenges = Arc::new(Mutex::new(HashMap::new()));
    let auth_challenges = Arc::new(Mutex::new(HashMap::new()));

    let auth_state = AuthState {
        db: db.clone(),
        webauthn: Arc::new(webauthn),
        reg_challenges: reg_challenges.clone(),
        auth_challenges: auth_challenges.clone(),
        use_secure_cookies,
    };

    // Spawn challenge cleanup task (every minute)
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        interval.tick().await; // Skip immediate tick
        loop {
            interval.tick().await;
            web::auth::cleanup_expired_challenges(&reg_challenges, &auth_challenges).await;
        }
    });

    // Check if we need to generate a setup token
    if !db.has_any_passkeys()? {
        // Generate setup token
        use rand::Rng;
        let token: String = rand::thread_rng()
            .sample_iter(&rand::distributions::Alphanumeric)
            .take(32)
            .map(char::from)
            .collect();

        db.set_setup_token(&token)?;

        // Write to file for admin access
        let token_path = std::env::var("SETUP_TOKEN_PATH")
            .unwrap_or_else(|_| "data/setup-token.txt".to_string());

        // Ensure parent directory exists
        if let Some(parent) = std::path::Path::new(&token_path).parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::write(&token_path, &token)?;
        tracing::info!("==================================================");
        tracing::info!("No passkeys registered. Setup token written to: {}", token_path);
        tracing::info!("Use this token to register your first passkey.");
        tracing::info!("==================================================");
    }

    // Build telegram config if both token and chat_id are set
    let telegram_config = match (&telegram_token, &telegram_chat_id) {
        (Some(token), Some(chat_id)) => Some(TelegramConfig {
            token: token.clone(),
            chat_id: chat_id.clone(),
        }),
        _ => None,
    };

    let web_state = FullAppState {
        db: db.clone(),
        sse_tx: sse_tx.clone(),
        auth: auth_state,
        telegram: telegram_config,
    };
    tokio::spawn(async move {
        if let Err(e) = web::start_server_with_auth(web_state, &listen_addr, static_dir.as_deref()).await {
            tracing::error!("Web server error: {}", e);
        }
    });

    // Start notification sender task if Telegram is configured
    if let (Some(token), Some(chat_id)) = (telegram_token, telegram_chat_id) {
        tracing::info!("Telegram notifications enabled");
        let sender = NotificationSender::new(
            db.clone(),
            notify_rx,
            token,
            chat_id,
            10, // max attempts
        );
        tokio::spawn(async move {
            sender.run().await;
        });
    } else {
        tracing::warn!("Telegram not configured (TELEGRAM_BOT_TOKEN and TELEGRAM_CHAT_ID required)");
        // Drain the channel so it doesn't block
        tokio::spawn(async move {
            let mut rx = notify_rx;
            while rx.recv().await.is_some() {}
        });
    }

    // Connect to UniFi
    let config = UnifiConfig::new(&host, &username, &password);
    tracing::info!("Connecting to UniFi console at {}...", host);
    let mut client = UnifiClient::connect(config, Some(db.clone())).await?;
    tracing::info!("Connected. Listening for events...");

    // Process events
    let mut count = 0;
    while let Some(event) = client.events().next().await {
        // Store and classify event
        let classification = processor.process(event.clone()).await?;

        // Skip SSE broadcast and logging for suppressed events
        if classification == Classification::Suppressed {
            continue;
        }

        count += 1;
        let local_ts = event.timestamp.with_timezone(&chrono::Local);
        let ts = local_ts.format("%H:%M:%S");

        // Broadcast to SSE clients (ignore errors if no clients connected)
        let _ = sse_tx.send(SseEvent {
            id: event.id.clone(),
            source: event.source.to_string(),
            event_type: event.event_type.clone(),
            severity: event.severity.map(|s| format!("{:?}", s).to_lowercase()),
            summary: event.summary.clone(),
            timestamp: event.timestamp.timestamp(),
            classification: classification.as_str().to_string(),
            notified: false,
            created_at: chrono::Utc::now().timestamp(),
        });

        println!(
            "[{}] {} {} | {} | {} [{}]",
            count,
            ts,
            event.source,
            event.event_type,
            event.summary,
            classification.as_str()
        );
    }

    Ok(())
}
