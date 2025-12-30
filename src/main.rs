use futures_util::StreamExt;
use tokio::sync::{broadcast, mpsc};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use unifi_monitor::db::Database;
use unifi_monitor::processor::{EventProcessor, NotificationSender, ProcessorConfig};
use unifi_monitor::unifi::{UnifiClient, UnifiConfig};
use unifi_monitor::web::{self, AppState, SseEvent};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            "info,unifi_monitor=debug,tokio_tungstenite=info,tungstenite=info,hyper=info,reqwest=info",
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!("UniFi Monitor starting...");

    // Load .env file if present
    dotenvy::dotenv().ok();

    // UniFi configuration
    let host = std::env::var("UNIFI_HOST").expect("UNIFI_HOST required");
    let username = std::env::var("UNIFI_USERNAME").expect("UNIFI_USERNAME required");
    let password = std::env::var("UNIFI_PASSWORD").expect("UNIFI_PASSWORD required");

    // Telegram configuration (optional for now)
    let telegram_token = std::env::var("TELEGRAM_BOT_TOKEN").ok();
    let telegram_chat_id = std::env::var("TELEGRAM_CHAT_ID").ok();

    // Database path
    let db_path = std::env::var("DATABASE_PATH").unwrap_or_else(|_| "data/unifi-monitor.db".to_string());

    // Ensure data directory exists
    if let Some(parent) = std::path::Path::new(&db_path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Open database
    tracing::info!("Opening database at {}...", db_path);
    let db = Database::open(&db_path)?;

    // Create notification channel
    let (notify_tx, notify_rx) = mpsc::channel(100);

    // Create broadcast channel for SSE (live event updates to frontend)
    let (sse_tx, _) = broadcast::channel::<SseEvent>(100);

    // Create event processor
    let processor = EventProcessor::new(db.clone(), ProcessorConfig::default(), notify_tx);

    // Load any pending notifications from database
    processor.load_pending_notifications().await?;

    // Start web server
    let listen_addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let static_dir = std::env::var("STATIC_DIR").ok();
    let web_state = AppState {
        db: db.clone(),
        sse_tx: sse_tx.clone(),
    };
    tokio::spawn(async move {
        if let Err(e) = web::start_server(web_state, &listen_addr, static_dir.as_deref()).await {
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
        count += 1;
        let local_ts = event.timestamp.with_timezone(&chrono::Local);
        let ts = local_ts.format("%H:%M:%S");

        // Store and classify event
        let classification = processor.process(event.clone()).await?;

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
            payload: event.raw.clone(),
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
