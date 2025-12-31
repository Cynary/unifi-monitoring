//! Web server module - Axum-based API and UI server

pub mod auth;

use axum::{
    extract::{Query, State},
    http::{header, Method, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    routing::{delete, get, post},
    Json, Router,
};
use axum_extra::extract::CookieJar;
use futures_util::stream::Stream;
use serde::{Deserialize, Serialize};
use std::{convert::Infallible, sync::Arc};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tower_governor::{governor::GovernorConfigBuilder, key_extractor::PeerIpKeyExtractor, GovernorLayer};
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};
use tracing::{info, warn};
use url::Url;
use webauthn_rs::Webauthn;

use crate::db::{Classification, Database};
use auth::{AuthState, validate_session_from_cookies};

/// Event sent via SSE to frontend (no payload - fetch separately)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SseEvent {
    pub id: String,
    pub source: String,
    pub event_type: String,
    pub severity: Option<String>,
    pub summary: String,
    pub timestamp: i64,
    pub classification: String,
    pub notified: bool,
    pub created_at: i64,
}

/// Shared application state (basic, for backwards compat)
#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub sse_tx: broadcast::Sender<SseEvent>,
}

/// Telegram configuration
#[derive(Clone)]
pub struct TelegramConfig {
    pub token: String,
    pub chat_id: String,
}

/// Full application state with auth
#[derive(Clone)]
pub struct FullAppState {
    pub db: Database,
    pub sse_tx: broadcast::Sender<SseEvent>,
    pub auth: AuthState,
    pub telegram: Option<TelegramConfig>,
}

/// Create the web server router (legacy - no auth)
pub fn create_router(state: AppState, static_dir: Option<&str>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let api_router = Router::new()
        // Health check
        .route("/api/health", get(health_legacy))
        // Events API
        .route("/api/events", get(list_events_legacy))
        .route("/api/events/count", get(count_events_legacy))
        .route("/api/events/types", get(list_event_types_legacy))
        .route("/api/events/stream", get(event_stream_legacy))
        .route("/api/events/{id}/payload", get(get_event_payload_legacy))
        // Rules API
        .route("/api/rules", get(list_rules_legacy))
        .route("/api/rules", post(set_rule_legacy))
        .route("/api/rules/{event_type}", delete(delete_rule_legacy))
        // Stats
        .route("/api/stats", get(get_stats_legacy))
        .layer(cors)
        .with_state(Arc::new(state));

    // If static directory is provided, serve it as fallback
    if let Some(dir) = static_dir {
        let serve_dir = ServeDir::new(dir).fallback(ServeFile::new(format!("{}/index.html", dir)));
        api_router.fallback_service(serve_dir)
    } else {
        api_router
    }
}

/// Create WebAuthn instance from environment
pub fn create_webauthn_from_env() -> Result<Webauthn, String> {
    let rp_id = std::env::var("RP_ID").unwrap_or_else(|_| "localhost".to_string());
    let rp_origin = std::env::var("RP_ORIGIN").unwrap_or_else(|_| "http://localhost:8080".to_string());

    let origin_url = Url::parse(&rp_origin)
        .map_err(|e| format!("Invalid RP_ORIGIN: {}", e))?;

    auth::create_webauthn(&rp_id, &origin_url)
        .map_err(|e| format!("Failed to create WebAuthn: {}", e))
}

/// Create CORS layer from environment
fn create_cors_layer() -> CorsLayer {
    let cors_origins = std::env::var("CORS_ORIGINS").ok();

    if let Some(origins_str) = cors_origins {
        // Parse comma-separated origins
        let origins: Vec<_> = origins_str
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse().ok())
            .collect();

        if origins.is_empty() {
            warn!("CORS_ORIGINS set but no valid origins parsed, falling back to RP_ORIGIN");
        } else {
            info!("CORS configured for origins: {:?}", origins_str);
            return CorsLayer::new()
                .allow_origin(origins)
                .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
                .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION, header::ACCEPT])
                .allow_credentials(true);
        }
    }

    // Fall back to RP_ORIGIN if set
    let rp_origin = std::env::var("RP_ORIGIN").ok();
    if let Some(origin) = rp_origin {
        if let Ok(parsed) = origin.parse() {
            info!("CORS configured for RP_ORIGIN: {}", origin);
            return CorsLayer::new()
                .allow_origin([parsed])
                .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
                .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION, header::ACCEPT])
                .allow_credentials(true);
        }
    }

    // Development fallback - allow any origin (only for localhost)
    warn!("No CORS_ORIGINS or RP_ORIGIN set, allowing any origin (development mode)");
    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
}

/// Create the web server router with authentication
pub fn create_router_with_auth(state: FullAppState, static_dir: Option<&str>) -> Router {
    let cors = create_cors_layer();

    // Create rate limiter for auth endpoints (10 requests per second burst, 1 request/sec sustained)
    // Using PeerIpKeyExtractor which works for both direct connections and behind proxies
    let rate_limit_config = GovernorConfigBuilder::default()
        .key_extractor(PeerIpKeyExtractor)
        .per_second(1) // Base: 1 request per second
        .burst_size(10) // Allow burst of 10 requests
        .finish()
        .unwrap();

    let rate_limiter = GovernorLayer {
        config: Arc::new(rate_limit_config),
    };

    let auth_state = Arc::new(state.auth.clone());
    let full_state = Arc::new(state);

    // Public auth routes (no auth required, rate limited)
    let auth_routes = Router::new()
        .route("/api/auth/status", get(auth::auth_status))
        .route("/api/auth/register/start", post(auth::register_start))
        .route("/api/auth/register/finish", post(auth::register_finish))
        .route("/api/auth/login/start", post(auth::login_start))
        .route("/api/auth/login/finish", post(auth::login_finish))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/passkeys", get(auth::list_passkeys))
        .route("/api/auth/passkeys/{id}", delete(auth::delete_passkey))
        .route("/api/auth/invite", post(auth::create_invite))
        .layer(rate_limiter)
        .with_state(auth_state);

    // Protected routes (require valid session)
    let protected_routes = Router::new()
        // Events API
        .route("/api/events", get(list_events))
        .route("/api/events/count", get(count_events))
        .route("/api/events/types", get(list_event_types))
        .route("/api/events/stream", get(event_stream))
        .route("/api/events/{id}/payload", get(get_event_payload))
        // Rules API
        .route("/api/rules", get(list_rules))
        .route("/api/rules", post(set_rule))
        .route("/api/rules/{event_type}", delete(delete_rule))
        // Stats
        .route("/api/stats", get(get_stats))
        // Notifications API
        .route("/api/notifications/history", get(get_notification_history))
        .route("/api/notifications/test", post(send_test_notification))
        .route("/api/notifications/status", get(get_notification_status))
        .with_state(full_state.clone());

    // Public routes (no auth required)
    let public_routes = Router::new()
        .route("/api/health", get(health))
        .with_state(full_state);

    let api_router = Router::new()
        .merge(auth_routes)
        .merge(protected_routes)
        .merge(public_routes)
        .layer(cors);

    // If static directory is provided, serve it as fallback
    if let Some(dir) = static_dir {
        let serve_dir = ServeDir::new(dir).fallback(ServeFile::new(format!("{}/index.html", dir)));
        api_router.fallback_service(serve_dir)
    } else {
        api_router
    }
}

/// Start the web server (legacy - no auth)
pub async fn start_server(state: AppState, addr: &str, static_dir: Option<&str>) -> anyhow::Result<()> {
    let router = create_router(state, static_dir);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("Web server listening on {}", addr);
    axum::serve(listener, router).await?;
    Ok(())
}

/// Start the web server with authentication
pub async fn start_server_with_auth(state: FullAppState, addr: &str, static_dir: Option<&str>) -> anyhow::Result<()> {
    use std::net::SocketAddr;

    let router = create_router_with_auth(state, static_dir);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("Web server listening on {} (auth enabled)", addr);
    // Use into_make_service_with_connect_info to provide peer IP for rate limiting
    axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

// ============================================================================
// Auth helper
// ============================================================================

/// Check if request is authenticated, return error if not
fn require_auth(jar: &CookieJar, db: &Database) -> Result<(), AppError> {
    if validate_session_from_cookies(jar, db).is_some() {
        Ok(())
    } else {
        Err(AppError::Unauthorized("Not authenticated".to_string()))
    }
}

// ============================================================================
// Health endpoint
// ============================================================================

async fn health(
    State(_state): State<Arc<FullAppState>>,
) -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn health_legacy() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

// ============================================================================
// SSE Event Stream
// ============================================================================

async fn event_stream(
    State(state): State<Arc<FullAppState>>,
    jar: CookieJar,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AppError> {
    require_auth(&jar, &state.db)?;

    let rx = state.sse_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| {
        match result {
            Ok(sse_event) => {
                let json = serde_json::to_string(&sse_event).unwrap_or_default();
                Some(Ok(Event::default().event("event").data(json)))
            }
            Err(_) => None, // Skip lagged messages
        }
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn event_stream_legacy(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.sse_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| {
        match result {
            Ok(sse_event) => {
                let json = serde_json::to_string(&sse_event).unwrap_or_default();
                Some(Ok(Event::default().event("event").data(json)))
            }
            Err(_) => None, // Skip lagged messages
        }
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ============================================================================
// Events API
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct ListEventsQuery {
    /// Filter by classifications (comma-separated: "notify,ignored")
    classification: Option<String>,
    /// Filter by event types (comma-separated)
    event_type: Option<String>,
    /// Search query (searches event_type, summary, source, payload)
    search: Option<String>,
    /// Number of events to return (default 200)
    limit: Option<usize>,
    /// Offset for pagination
    offset: Option<usize>,
    /// Include payload in response (default false for list)
    include_payload: Option<bool>,
}

impl ListEventsQuery {
    fn classifications(&self) -> Vec<Classification> {
        self.classification
            .as_ref()
            .map(|s| {
                s.split(',')
                    .filter_map(|c| Classification::from_str(c.trim()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn event_types(&self) -> Vec<String> {
        self.event_type
            .as_ref()
            .map(|s| s.split(',').map(|t| t.trim().to_string()).collect())
            .unwrap_or_default()
    }
}

#[derive(Debug, Serialize)]
pub struct EventResponse {
    pub id: String,
    pub source: String,
    pub event_type: String,
    pub severity: Option<String>,
    pub summary: String,
    pub timestamp: i64,
    pub classification: String,
    pub notified: bool,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

async fn list_events(
    State(state): State<Arc<FullAppState>>,
    jar: CookieJar,
    Query(query): Query<ListEventsQuery>,
) -> Result<Json<Vec<EventResponse>>, AppError> {
    require_auth(&jar, &state.db)?;
    list_events_impl(&state.db, query)
}

async fn list_events_legacy(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListEventsQuery>,
) -> Result<Json<Vec<EventResponse>>, AppError> {
    list_events_impl(&state.db, query)
}

fn list_events_impl(
    db: &Database,
    query: ListEventsQuery,
) -> Result<Json<Vec<EventResponse>>, AppError> {
    let classifications = query.classifications();
    let event_types = query.event_types();
    let event_type_refs: Vec<&str> = event_types.iter().map(|s| s.as_str()).collect();
    let include_payload = query.include_payload.unwrap_or(false);

    let events = db.query_events(
        &classifications,
        &event_type_refs,
        query.search.as_deref(),
        query.limit.unwrap_or(200),
        query.offset.unwrap_or(0),
    )?;

    let response: Vec<EventResponse> = events
        .into_iter()
        .map(|e| EventResponse {
            id: e.id,
            source: e.source.to_string(),
            event_type: e.event_type,
            severity: e.severity.map(|s| format!("{:?}", s).to_lowercase()),
            summary: e.summary,
            timestamp: e.timestamp,
            classification: e.classification.as_str().to_string(),
            notified: e.notified,
            created_at: e.created_at,
            payload: if include_payload { Some(e.payload) } else { None },
        })
        .collect();

    Ok(Json(response))
}

#[derive(Debug, Serialize)]
pub struct CountResponse {
    pub count: i64,
}

async fn count_events(
    State(state): State<Arc<FullAppState>>,
    jar: CookieJar,
    Query(query): Query<ListEventsQuery>,
) -> Result<Json<CountResponse>, AppError> {
    require_auth(&jar, &state.db)?;
    count_events_impl(&state.db, query)
}

async fn count_events_legacy(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListEventsQuery>,
) -> Result<Json<CountResponse>, AppError> {
    count_events_impl(&state.db, query)
}

fn count_events_impl(
    db: &Database,
    query: ListEventsQuery,
) -> Result<Json<CountResponse>, AppError> {
    let classifications = query.classifications();
    let event_types = query.event_types();
    let event_type_refs: Vec<&str> = event_types.iter().map(|s| s.as_str()).collect();

    let count = db.count_events(
        &classifications,
        &event_type_refs,
        query.search.as_deref(),
    )?;

    Ok(Json(CountResponse { count }))
}

#[derive(Debug, Serialize)]
pub struct EventTypeResponse {
    pub event_type: String,
    pub count: i64,
    pub latest_timestamp: i64,
    pub classification: String,
}

async fn list_event_types(
    State(state): State<Arc<FullAppState>>,
    jar: CookieJar,
) -> Result<Json<Vec<EventTypeResponse>>, AppError> {
    require_auth(&jar, &state.db)?;
    list_event_types_impl(&state.db)
}

async fn list_event_types_legacy(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<EventTypeResponse>>, AppError> {
    list_event_types_impl(&state.db)
}

fn list_event_types_impl(db: &Database) -> Result<Json<Vec<EventTypeResponse>>, AppError> {
    let summaries = db.get_event_type_summary()?;

    let response: Vec<EventTypeResponse> = summaries
        .into_iter()
        .map(|s| EventTypeResponse {
            event_type: s.event_type,
            count: s.count,
            latest_timestamp: s.latest_timestamp,
            classification: s.classification.as_str().to_string(),
        })
        .collect();

    Ok(Json(response))
}

#[derive(Debug, Serialize)]
pub struct PayloadResponse {
    pub payload: serde_json::Value,
}

async fn get_event_payload(
    State(state): State<Arc<FullAppState>>,
    jar: CookieJar,
    axum::extract::Path(event_id): axum::extract::Path<String>,
) -> Result<Json<PayloadResponse>, AppError> {
    require_auth(&jar, &state.db)?;
    get_event_payload_impl(&state.db, &event_id)
}

async fn get_event_payload_legacy(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(event_id): axum::extract::Path<String>,
) -> Result<Json<PayloadResponse>, AppError> {
    get_event_payload_impl(&state.db, &event_id)
}

fn get_event_payload_impl(db: &Database, event_id: &str) -> Result<Json<PayloadResponse>, AppError> {
    let payload = db.get_event_payload(event_id)?
        .ok_or(AppError::NotFound)?;

    Ok(Json(PayloadResponse { payload }))
}

// ============================================================================
// Rules API
// ============================================================================

#[derive(Debug, Serialize)]
pub struct RuleResponse {
    pub event_type: String,
    pub classification: String,
}

async fn list_rules(
    State(state): State<Arc<FullAppState>>,
    jar: CookieJar,
) -> Result<Json<Vec<RuleResponse>>, AppError> {
    require_auth(&jar, &state.db)?;
    list_rules_impl(&state.db)
}

async fn list_rules_legacy(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<RuleResponse>>, AppError> {
    list_rules_impl(&state.db)
}

fn list_rules_impl(db: &Database) -> Result<Json<Vec<RuleResponse>>, AppError> {
    let rules = db.get_all_rules()?;

    let response: Vec<RuleResponse> = rules
        .into_iter()
        .map(|(event_type, classification)| RuleResponse {
            event_type,
            classification: classification.as_str().to_string(),
        })
        .collect();

    Ok(Json(response))
}

#[derive(Debug, Deserialize)]
pub struct SetRuleRequest {
    pub event_type: String,
    pub classification: String,
}

async fn set_rule(
    State(state): State<Arc<FullAppState>>,
    jar: CookieJar,
    Json(req): Json<SetRuleRequest>,
) -> Result<Json<RuleResponse>, AppError> {
    require_auth(&jar, &state.db)?;
    set_rule_impl(&state.db, req)
}

async fn set_rule_legacy(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SetRuleRequest>,
) -> Result<Json<RuleResponse>, AppError> {
    set_rule_impl(&state.db, req)
}

fn set_rule_impl(db: &Database, req: SetRuleRequest) -> Result<Json<RuleResponse>, AppError> {
    let classification = Classification::from_str(&req.classification)
        .ok_or_else(|| AppError::BadRequest("Invalid classification".to_string()))?;

    db.set_rule(&req.event_type, classification)?;

    Ok(Json(RuleResponse {
        event_type: req.event_type,
        classification: classification.as_str().to_string(),
    }))
}

async fn delete_rule(
    State(state): State<Arc<FullAppState>>,
    jar: CookieJar,
    axum::extract::Path(event_type): axum::extract::Path<String>,
) -> Result<StatusCode, AppError> {
    require_auth(&jar, &state.db)?;
    delete_rule_impl(&state.db, &event_type)
}

async fn delete_rule_legacy(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(event_type): axum::extract::Path<String>,
) -> Result<StatusCode, AppError> {
    delete_rule_impl(&state.db, &event_type)
}

fn delete_rule_impl(db: &Database, event_type: &str) -> Result<StatusCode, AppError> {
    let deleted = db.delete_rule(event_type)?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
}

// ============================================================================
// Stats API
// ============================================================================

#[derive(Debug, Serialize)]
pub struct StatsResponse {
    pub total_events: i64,
    pub unclassified_types: i64,
    pub notify_types: i64,
    pub ignored_types: i64,
}

async fn get_stats(
    State(state): State<Arc<FullAppState>>,
    jar: CookieJar,
) -> Result<Json<StatsResponse>, AppError> {
    require_auth(&jar, &state.db)?;
    get_stats_impl(&state.db)
}

async fn get_stats_legacy(
    State(state): State<Arc<AppState>>,
) -> Result<Json<StatsResponse>, AppError> {
    get_stats_impl(&state.db)
}

fn get_stats_impl(db: &Database) -> Result<Json<StatsResponse>, AppError> {
    let summaries = db.get_event_type_summary()?;

    let total_events: i64 = summaries.iter().map(|s| s.count).sum();
    let unclassified_types = summaries
        .iter()
        .filter(|s| s.classification == Classification::Unclassified)
        .count() as i64;
    let notify_types = summaries
        .iter()
        .filter(|s| s.classification == Classification::Notify)
        .count() as i64;
    let ignored_types = summaries
        .iter()
        .filter(|s| s.classification == Classification::Ignored)
        .count() as i64;

    Ok(Json(StatsResponse {
        total_events,
        unclassified_types,
        notify_types,
        ignored_types,
    }))
}

// ============================================================================
// Notifications API
// ============================================================================

#[derive(Debug, Serialize)]
pub struct NotificationLogResponse {
    pub id: i64,
    pub event_id: Option<String>,
    pub event_type: Option<String>,
    pub event_summary: Option<String>,
    pub status: String,
    pub error_message: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Deserialize)]
pub struct NotificationHistoryQuery {
    limit: Option<usize>,
}

async fn get_notification_history(
    State(state): State<Arc<FullAppState>>,
    jar: CookieJar,
    Query(query): Query<NotificationHistoryQuery>,
) -> Result<Json<Vec<NotificationLogResponse>>, AppError> {
    require_auth(&jar, &state.db)?;

    let limit = query.limit.unwrap_or(50);
    let history = state.db.get_notification_history(limit)?;

    let response: Vec<NotificationLogResponse> = history
        .into_iter()
        .map(|entry| NotificationLogResponse {
            id: entry.id,
            event_id: entry.event_id,
            event_type: entry.event_type,
            event_summary: entry.event_summary,
            status: entry.status,
            error_message: entry.error_message,
            created_at: entry.created_at,
        })
        .collect();

    Ok(Json(response))
}

#[derive(Debug, Serialize)]
pub struct NotificationStatusResponse {
    pub configured: bool,
}

async fn get_notification_status(
    State(state): State<Arc<FullAppState>>,
    jar: CookieJar,
) -> Result<Json<NotificationStatusResponse>, AppError> {
    require_auth(&jar, &state.db)?;

    Ok(Json(NotificationStatusResponse {
        configured: state.telegram.is_some(),
    }))
}

#[derive(Debug, Serialize)]
pub struct TestNotificationResponse {
    pub success: bool,
    pub error: Option<String>,
}

async fn send_test_notification(
    State(state): State<Arc<FullAppState>>,
    jar: CookieJar,
) -> Result<Json<TestNotificationResponse>, AppError> {
    require_auth(&jar, &state.db)?;

    let telegram = state.telegram.as_ref()
        .ok_or_else(|| AppError::BadRequest("Telegram not configured".to_string()))?;

    match crate::processor::send_test_notification(&state.db, &telegram.token, &telegram.chat_id).await {
        Ok(()) => Ok(Json(TestNotificationResponse {
            success: true,
            error: None,
        })),
        Err(e) => Ok(Json(TestNotificationResponse {
            success: false,
            error: Some(e.to_string()),
        })),
    }
}

// ============================================================================
// Error handling
// ============================================================================

#[derive(Debug)]
pub enum AppError {
    Database(rusqlite::Error),
    BadRequest(String),
    NotFound,
    Unauthorized(String),
    Internal(String),
}

impl From<rusqlite::Error> for AppError {
    fn from(err: rusqlite::Error) -> Self {
        AppError::Database(err)
    }
}

impl From<webauthn_rs::prelude::WebauthnError> for AppError {
    fn from(err: webauthn_rs::prelude::WebauthnError) -> Self {
        AppError::BadRequest(format!("WebAuthn error: {}", err))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match self {
            AppError::Database(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            ),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            AppError::NotFound => (StatusCode::NOT_FOUND, "Not found".to_string()),
            AppError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, msg),
            AppError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
        };

        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}
