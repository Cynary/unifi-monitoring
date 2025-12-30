//! Web server module - Axum-based API and UI server

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    routing::{delete, get, post},
    Json, Router,
};
use futures_util::stream::Stream;
use serde::{Deserialize, Serialize};
use std::{convert::Infallible, sync::Arc};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};
use tracing::info;

use crate::db::{Classification, Database};

/// Event sent via SSE to frontend
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
    pub payload: serde_json::Value,
}

/// Shared application state
#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub sse_tx: broadcast::Sender<SseEvent>,
}

/// Create the web server router
pub fn create_router(state: AppState, static_dir: Option<&str>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let api_router = Router::new()
        // Health check
        .route("/api/health", get(health))
        // Events API
        .route("/api/events", get(list_events))
        .route("/api/events/count", get(count_events))
        .route("/api/events/types", get(list_event_types))
        .route("/api/events/stream", get(event_stream))
        // Rules API
        .route("/api/rules", get(list_rules))
        .route("/api/rules", post(set_rule))
        .route("/api/rules/{event_type}", delete(delete_rule))
        // Stats
        .route("/api/stats", get(get_stats))
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

/// Start the web server
pub async fn start_server(state: AppState, addr: &str, static_dir: Option<&str>) -> anyhow::Result<()> {
    let router = create_router(state, static_dir);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("Web server listening on {}", addr);
    axum::serve(listener, router).await?;
    Ok(())
}

// ============================================================================
// Health endpoint
// ============================================================================

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

// ============================================================================
// SSE Event Stream
// ============================================================================

async fn event_stream(
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
    pub payload: serde_json::Value,
}

async fn list_events(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListEventsQuery>,
) -> Result<Json<Vec<EventResponse>>, AppError> {
    let classifications = query.classifications();
    let event_types = query.event_types();
    let event_type_refs: Vec<&str> = event_types.iter().map(|s| s.as_str()).collect();

    let events = state.db.query_events(
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
            payload: e.payload,
        })
        .collect();

    Ok(Json(response))
}

#[derive(Debug, Serialize)]
pub struct CountResponse {
    pub count: i64,
}

async fn count_events(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListEventsQuery>,
) -> Result<Json<CountResponse>, AppError> {
    let classifications = query.classifications();
    let event_types = query.event_types();
    let event_type_refs: Vec<&str> = event_types.iter().map(|s| s.as_str()).collect();

    let count = state.db.count_events(
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
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<EventTypeResponse>>, AppError> {
    let summaries = state.db.get_event_type_summary()?;

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

// ============================================================================
// Rules API
// ============================================================================

#[derive(Debug, Serialize)]
pub struct RuleResponse {
    pub event_type: String,
    pub classification: String,
}

async fn list_rules(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<RuleResponse>>, AppError> {
    let rules = state.db.get_all_rules()?;

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
    State(state): State<Arc<AppState>>,
    Json(req): Json<SetRuleRequest>,
) -> Result<Json<RuleResponse>, AppError> {
    let classification = Classification::from_str(&req.classification)
        .ok_or_else(|| AppError::BadRequest("Invalid classification".to_string()))?;

    state.db.set_rule(&req.event_type, classification)?;

    Ok(Json(RuleResponse {
        event_type: req.event_type,
        classification: classification.as_str().to_string(),
    }))
}

async fn delete_rule(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(event_type): axum::extract::Path<String>,
) -> Result<StatusCode, AppError> {
    let deleted = state.db.delete_rule(&event_type)?;
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
    State(state): State<Arc<AppState>>,
) -> Result<Json<StatsResponse>, AppError> {
    let summaries = state.db.get_event_type_summary()?;

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
// Error handling
// ============================================================================

#[derive(Debug)]
pub enum AppError {
    Database(rusqlite::Error),
    BadRequest(String),
    NotFound,
}

impl From<rusqlite::Error> for AppError {
    fn from(err: rusqlite::Error) -> Self {
        AppError::Database(err)
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
        };

        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}
