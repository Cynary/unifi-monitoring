//! Authentication module - WebAuthn/Passkey authentication handlers

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use axum_extra::extract::CookieJar;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc, time::Instant};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};
use url::Url;
use webauthn_rs::prelude::*;

use crate::db::Database;

use super::AppError;

// ============================================================================
// Types
// ============================================================================

/// Challenge with timestamp for expiry
#[derive(Clone)]
pub struct TimestampedChallenge<T> {
    pub challenge: T,
    pub created_at: Instant,
}

/// WebAuthn challenge storage for registration (with timestamps)
pub type RegChallengeStore = Arc<Mutex<HashMap<String, TimestampedChallenge<PasskeyRegistration>>>>;

/// WebAuthn challenge storage for authentication (with timestamps)
pub type AuthChallengeStore = Arc<Mutex<HashMap<String, TimestampedChallenge<PasskeyAuthentication>>>>;

/// Extended app state with WebAuthn config
#[derive(Clone)]
pub struct AuthState {
    pub db: Database,
    pub webauthn: Arc<Webauthn>,
    pub reg_challenges: RegChallengeStore,
    pub auth_challenges: AuthChallengeStore,
    /// Whether to set Secure flag on cookies (true for HTTPS)
    pub use_secure_cookies: bool,
}

/// Authentication status response
#[derive(Serialize)]
pub struct AuthStatusResponse {
    pub authenticated: bool,
    pub has_passkeys: bool,
    pub needs_setup: bool,
}

/// Registration start request
#[derive(Deserialize)]
pub struct RegisterStartRequest {
    /// Setup token (for first passkey) or invite token (for additional passkeys)
    pub token: Option<String>,
    /// Passkey name (e.g., "MacBook Pro", "iPhone")
    pub name: Option<String>,
}

/// Registration start response
#[derive(Serialize)]
pub struct RegisterStartResponse {
    pub challenge: CreationChallengeResponse,
    pub challenge_id: String,
}

/// Registration finish request
#[derive(Deserialize)]
pub struct RegisterFinishRequest {
    pub challenge_id: String,
    pub credential: RegisterPublicKeyCredential,
    pub name: Option<String>,
}

/// Login start response
#[derive(Serialize)]
pub struct LoginStartResponse {
    pub challenge: RequestChallengeResponse,
    pub challenge_id: String,
}

/// Login finish request
#[derive(Deserialize)]
pub struct LoginFinishRequest {
    pub challenge_id: String,
    pub credential: PublicKeyCredential,
}

/// Login/Register success response
#[derive(Serialize)]
pub struct AuthSuccessResponse {
    pub success: bool,
}

/// Passkey info for UI
#[derive(Serialize)]
pub struct PasskeyResponse {
    pub id: String,
    pub name: Option<String>,
    pub created_at: i64,
}

/// Invite token response
#[derive(Serialize)]
pub struct InviteTokenResponse {
    pub token: String,
    pub expires_in_secs: i64,
}

// ============================================================================
// WebAuthn Configuration
// ============================================================================

/// Create WebAuthn instance from RP configuration
pub fn create_webauthn(rp_id: &str, rp_origin: &Url) -> Result<Webauthn, WebauthnError> {
    WebauthnBuilder::new(rp_id, rp_origin)?
        .rp_name("UniFi Monitor")
        .build()
}

// ============================================================================
// Session Cookie Management
// ============================================================================

const SESSION_COOKIE_NAME: &str = "unifi_session";
const SESSION_EXPIRY_DAYS: i64 = 30;
const INVITE_TOKEN_EXPIRY_SECS: i64 = 300; // 5 minutes
const CHALLENGE_EXPIRY_SECS: u64 = 300; // 5 minutes

/// Extract session ID from cookies and validate it
pub fn validate_session_from_cookies(jar: &CookieJar, db: &Database) -> Option<String> {
    jar.get(SESSION_COOKIE_NAME)
        .and_then(|cookie| {
            let session_id = cookie.value();
            match db.validate_session(session_id) {
                Ok(true) => Some(session_id.to_string()),
                Ok(false) => {
                    debug!("Invalid or expired session");
                    None
                }
                Err(e) => {
                    warn!("Error validating session: {}", e);
                    None
                }
            }
        })
}

/// Create a session cookie
fn create_session_cookie(session_id: &str, secure: bool) -> axum_extra::extract::cookie::Cookie<'static> {
    use axum_extra::extract::cookie::Cookie;
    Cookie::build((SESSION_COOKIE_NAME, session_id.to_string()))
        .path("/")
        .http_only(true)
        .secure(secure)
        .same_site(axum_extra::extract::cookie::SameSite::Strict)
        .max_age(time::Duration::days(SESSION_EXPIRY_DAYS))
        .build()
}

/// Create a cookie that clears the session
fn clear_session_cookie(secure: bool) -> axum_extra::extract::cookie::Cookie<'static> {
    use axum_extra::extract::cookie::Cookie;
    Cookie::build((SESSION_COOKIE_NAME, ""))
        .path("/")
        .http_only(true)
        .secure(secure)
        .max_age(time::Duration::seconds(0))
        .build()
}

/// Clean up expired challenges from both stores
pub async fn cleanup_expired_challenges(
    reg_challenges: &RegChallengeStore,
    auth_challenges: &AuthChallengeStore,
) {
    let expiry = std::time::Duration::from_secs(CHALLENGE_EXPIRY_SECS);
    let now = Instant::now();

    // Cleanup registration challenges
    {
        let mut challenges = reg_challenges.lock().await;
        let before = challenges.len();
        challenges.retain(|_, v| now.duration_since(v.created_at) < expiry);
        let removed = before - challenges.len();
        if removed > 0 {
            debug!("Cleaned up {} expired registration challenges", removed);
        }
    }

    // Cleanup authentication challenges
    {
        let mut challenges = auth_challenges.lock().await;
        let before = challenges.len();
        challenges.retain(|_, v| now.duration_since(v.created_at) < expiry);
        let removed = before - challenges.len();
        if removed > 0 {
            debug!("Cleaned up {} expired authentication challenges", removed);
        }
    }
}

// ============================================================================
// Handlers
// ============================================================================

/// GET /api/auth/status - Check authentication status
pub async fn auth_status(
    State(state): State<Arc<AuthState>>,
    jar: CookieJar,
) -> Result<Json<AuthStatusResponse>, AppError> {
    let has_passkeys = state.db.has_any_passkeys()?;
    let authenticated = validate_session_from_cookies(&jar, &state.db).is_some();
    let needs_setup = !has_passkeys;

    Ok(Json(AuthStatusResponse {
        authenticated,
        has_passkeys,
        needs_setup,
    }))
}

/// POST /api/auth/register/start - Start passkey registration
pub async fn register_start(
    State(state): State<Arc<AuthState>>,
    jar: CookieJar,
    Json(req): Json<RegisterStartRequest>,
) -> Result<Json<RegisterStartResponse>, AppError> {
    let has_passkeys = state.db.has_any_passkeys()?;
    let is_authenticated = validate_session_from_cookies(&jar, &state.db).is_some();

    // Authorization check:
    // - If no passkeys exist, must provide valid setup token
    // - If passkeys exist and not authenticated, must provide valid invite token
    // - If authenticated, can register without token
    if !is_authenticated {
        let token = req.token.as_deref().ok_or_else(|| {
            AppError::Unauthorized("Token required for registration".to_string())
        })?;

        if !has_passkeys {
            // First passkey - check setup token
            if !state.db.validate_setup_token(token)? {
                return Err(AppError::Unauthorized("Invalid setup token".to_string()));
            }
        } else {
            // Additional passkey - check invite token (consumes it)
            if !state.db.validate_invite_token(token)? {
                return Err(AppError::Unauthorized("Invalid or expired invite token".to_string()));
            }
        }
    }

    // Generate a unique user ID for WebAuthn (we use a fixed one since single-user)
    let user_id = Uuid::new_v4();
    let user_name = "admin";

    // Get existing credentials to exclude
    let existing_passkeys = state.db.get_all_passkeys()?;
    let exclude_credentials: Vec<CredentialID> = existing_passkeys
        .iter()
        .filter_map(|(_id, cred_bytes)| {
            let cred_json = std::str::from_utf8(cred_bytes).ok()?;
            let passkey: Passkey = serde_json::from_str(cred_json).ok()?;
            Some(passkey.cred_id().clone())
        })
        .collect();

    // Start registration
    let (ccr, reg_state) = state.webauthn.start_passkey_registration(
        user_id,
        user_name,
        user_name,
        Some(exclude_credentials),
    )?;

    // Store challenge with timestamp
    let challenge_id = Uuid::new_v4().to_string();
    {
        let mut challenges = state.reg_challenges.lock().await;
        challenges.insert(challenge_id.clone(), TimestampedChallenge {
            challenge: reg_state,
            created_at: Instant::now(),
        });
    }

    debug!(challenge_id = %challenge_id, "Registration challenge created");

    Ok(Json(RegisterStartResponse {
        challenge: ccr,
        challenge_id,
    }))
}

/// POST /api/auth/register/finish - Complete passkey registration
pub async fn register_finish(
    State(state): State<Arc<AuthState>>,
    jar: CookieJar,
    Json(req): Json<RegisterFinishRequest>,
) -> Result<(CookieJar, Json<AuthSuccessResponse>), AppError> {
    // Get and remove challenge (check expiry)
    let reg_state = {
        let mut challenges = state.reg_challenges.lock().await;
        let timestamped = challenges.remove(&req.challenge_id)
            .ok_or_else(|| AppError::BadRequest("Invalid or expired challenge".to_string()))?;

        // Check if challenge has expired
        if timestamped.created_at.elapsed().as_secs() > CHALLENGE_EXPIRY_SECS {
            return Err(AppError::BadRequest("Challenge has expired".to_string()));
        }
        timestamped.challenge
    };

    // Complete registration
    let passkey = state.webauthn.finish_passkey_registration(&req.credential, &reg_state)?;

    // Serialize credential as JSON (more compatible than bincode)
    let cred_json = serde_json::to_string(&passkey)
        .map_err(|e| AppError::Internal(format!("Failed to serialize passkey: {}", e)))?;
    let cred_bytes = cred_json.as_bytes().to_vec();

    // Generate credential ID for storage
    let cred_id = URL_SAFE_NO_PAD.encode(passkey.cred_id());

    // Store passkey
    let name = req.name.as_deref();
    state.db.store_passkey(&cred_id, &cred_bytes, name)?;

    info!(cred_id = %cred_id, name = ?name, "Passkey registered");

    // Delete setup token if this was the first passkey
    state.db.delete_setup_token()?;

    // Create session
    let session_id = state.db.create_session(SESSION_EXPIRY_DAYS)?;
    let jar = jar.add(create_session_cookie(&session_id, state.use_secure_cookies));

    Ok((jar, Json(AuthSuccessResponse { success: true })))
}

/// POST /api/auth/login/start - Start passkey authentication
pub async fn login_start(
    State(state): State<Arc<AuthState>>,
) -> Result<Json<LoginStartResponse>, AppError> {
    // Get all passkeys
    let passkeys = state.db.get_all_passkeys()?;
    if passkeys.is_empty() {
        return Err(AppError::BadRequest("No passkeys registered".to_string()));
    }

    // Deserialize passkeys from JSON
    let credentials: Vec<Passkey> = passkeys
        .iter()
        .filter_map(|(_, cred_bytes)| {
            let cred_json = std::str::from_utf8(cred_bytes).ok()?;
            serde_json::from_str(cred_json).ok()
        })
        .collect();

    if credentials.is_empty() {
        return Err(AppError::Internal("Failed to load passkeys".to_string()));
    }

    // Start authentication
    let (rcr, auth_state) = state.webauthn.start_passkey_authentication(&credentials)?;

    // Store challenge with timestamp
    let challenge_id = Uuid::new_v4().to_string();
    {
        let mut challenges = state.auth_challenges.lock().await;
        challenges.insert(challenge_id.clone(), TimestampedChallenge {
            challenge: auth_state,
            created_at: Instant::now(),
        });
    }

    debug!(challenge_id = %challenge_id, "Authentication challenge created");

    Ok(Json(LoginStartResponse {
        challenge: rcr,
        challenge_id,
    }))
}

/// POST /api/auth/login/finish - Complete passkey authentication
pub async fn login_finish(
    State(state): State<Arc<AuthState>>,
    jar: CookieJar,
    Json(req): Json<LoginFinishRequest>,
) -> Result<(CookieJar, Json<AuthSuccessResponse>), AppError> {
    // Get and remove challenge (check expiry)
    let auth_state = {
        let mut challenges = state.auth_challenges.lock().await;
        let timestamped = challenges.remove(&req.challenge_id)
            .ok_or_else(|| AppError::BadRequest("Invalid or expired challenge".to_string()))?;

        // Check if challenge has expired
        if timestamped.created_at.elapsed().as_secs() > CHALLENGE_EXPIRY_SECS {
            return Err(AppError::BadRequest("Challenge has expired".to_string()));
        }
        timestamped.challenge
    };

    // Complete authentication
    let _auth_result = state.webauthn.finish_passkey_authentication(&req.credential, &auth_state)?;

    info!("Passkey authentication successful");

    // Create session
    let session_id = state.db.create_session(SESSION_EXPIRY_DAYS)?;
    let jar = jar.add(create_session_cookie(&session_id, state.use_secure_cookies));

    Ok((jar, Json(AuthSuccessResponse { success: true })))
}

/// POST /api/auth/logout - Log out
pub async fn logout(
    State(state): State<Arc<AuthState>>,
    jar: CookieJar,
) -> Result<(CookieJar, Json<AuthSuccessResponse>), AppError> {
    // Delete session if exists
    if let Some(session_id) = validate_session_from_cookies(&jar, &state.db) {
        state.db.delete_session(&session_id)?;
    }

    let jar = jar.add(clear_session_cookie(state.use_secure_cookies));
    Ok((jar, Json(AuthSuccessResponse { success: true })))
}

/// GET /api/auth/passkeys - List passkeys (authenticated)
pub async fn list_passkeys(
    State(state): State<Arc<AuthState>>,
    jar: CookieJar,
) -> Result<Json<Vec<PasskeyResponse>>, AppError> {
    // Require authentication
    if validate_session_from_cookies(&jar, &state.db).is_none() {
        return Err(AppError::Unauthorized("Not authenticated".to_string()));
    }

    let passkeys = state.db.list_passkeys()?;
    let response: Vec<PasskeyResponse> = passkeys
        .into_iter()
        .map(|p| PasskeyResponse {
            id: p.id,
            name: p.name,
            created_at: p.created_at,
        })
        .collect();

    Ok(Json(response))
}

/// DELETE /api/auth/passkeys/:id - Delete a passkey (authenticated)
pub async fn delete_passkey(
    State(state): State<Arc<AuthState>>,
    jar: CookieJar,
    Path(passkey_id): Path<String>,
) -> Result<StatusCode, AppError> {
    // Require authentication
    if validate_session_from_cookies(&jar, &state.db).is_none() {
        return Err(AppError::Unauthorized("Not authenticated".to_string()));
    }

    // Don't allow deleting the last passkey
    let passkeys = state.db.list_passkeys()?;
    if passkeys.len() <= 1 {
        return Err(AppError::BadRequest("Cannot delete the last passkey".to_string()));
    }

    let deleted = state.db.delete_passkey(&passkey_id)?;
    if deleted {
        info!(passkey_id = %passkey_id, "Passkey deleted");
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
}

/// POST /api/auth/invite - Create an invite token (authenticated)
pub async fn create_invite(
    State(state): State<Arc<AuthState>>,
    jar: CookieJar,
) -> Result<Json<InviteTokenResponse>, AppError> {
    // Require authentication
    if validate_session_from_cookies(&jar, &state.db).is_none() {
        return Err(AppError::Unauthorized("Not authenticated".to_string()));
    }

    let token = state.db.create_invite_token(INVITE_TOKEN_EXPIRY_SECS)?;
    info!("Invite token created");

    Ok(Json(InviteTokenResponse {
        token,
        expires_in_secs: INVITE_TOKEN_EXPIRY_SECS,
    }))
}

// ============================================================================
// Auth Middleware Helper
// ============================================================================

/// Check if request is authenticated (for use in route handlers)
pub fn require_auth(jar: &CookieJar, db: &Database) -> Result<String, AppError> {
    validate_session_from_cookies(jar, db)
        .ok_or_else(|| AppError::Unauthorized("Not authenticated".to_string()))
}
