use reqwest::{
    cookie::{CookieStore, Jar},
    Client,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, info, instrument};
use url::Url;

use super::error::UnifiError;
use super::types::UnifiConfig;

/// Authenticated session with UniFi console
#[derive(Debug)]
pub struct UnifiSession {
    /// HTTP client with cookie jar for session management
    pub client: Client,

    /// Cookie jar (kept for WebSocket auth)
    cookie_jar: Arc<Jar>,

    /// CSRF token for subsequent requests
    pub csrf_token: String,

    /// Configuration used to create this session
    pub config: UnifiConfig,
}

/// Response from bootstrap endpoint
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapResponse {
    /// Last update ID for WebSocket connection
    pub last_update_id: String,

    /// NVR information
    pub nvr: Option<NvrInfo>,

    /// Cameras (we don't need details, just confirming connection works)
    #[serde(default)]
    pub cameras: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NvrInfo {
    pub id: String,
    pub name: String,
    pub version: String,
}

/// Login request body
#[derive(Debug, Serialize)]
struct LoginRequest<'a> {
    username: &'a str,
    password: &'a str,
    token: &'a str,
    #[serde(rename = "rememberMe")]
    remember_me: bool,
}

impl UnifiSession {
    /// Create a new authenticated session with the UniFi console
    #[instrument(skip(config), fields(host = %config.host))]
    pub async fn login(config: UnifiConfig) -> Result<Self, UnifiError> {
        // Create cookie jar for session management
        let jar = Arc::new(Jar::default());

        let client = Client::builder()
            .cookie_provider(jar.clone())
            .danger_accept_invalid_certs(!config.verify_ssl)
            .build()?;

        let base_url = config.base_url();

        // UniFi OS doesn't require CSRF token for initial login
        // Just POST directly to the login endpoint
        let login_url = format!("{}/api/auth/login", base_url);
        let login_req = LoginRequest {
            username: &config.username,
            password: &config.password,
            token: "",
            remember_me: true,
        };

        debug!("Sending login request to {}", login_url);
        let resp = client
            .post(&login_url)
            .header("Content-Type", "application/json")
            .json(&login_req)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(UnifiError::AuthFailed(format!(
                "Login failed with status {}: {}",
                status, body
            )));
        }

        // Get CSRF token from response headers
        let csrf_token = resp
            .headers()
            .get("x-csrf-token")
            .or_else(|| resp.headers().get("x-updated-csrf-token"))
            .and_then(|v| v.to_str().ok())
            .map(String::from)
            .unwrap_or_default();

        debug!("Got CSRF token: {}", if csrf_token.is_empty() { "(empty)" } else { &csrf_token[..20.min(csrf_token.len())] });

        info!("Successfully authenticated with UniFi console");

        Ok(Self {
            client,
            cookie_jar: jar,
            csrf_token,
            config,
        })
    }

    /// Get cookies as a header value for WebSocket connections
    pub fn get_cookie_header(&self) -> String {
        let url = Url::parse(&self.config.base_url()).unwrap();
        self.cookie_jar
            .cookies(&url)
            .map(|c| c.to_str().unwrap_or("").to_string())
            .unwrap_or_default()
    }

    /// Get the Protect bootstrap data (includes lastUpdateId for WebSocket)
    #[instrument(skip(self))]
    pub async fn get_protect_bootstrap(&self) -> Result<BootstrapResponse, UnifiError> {
        let url = format!("{}/proxy/protect/api/bootstrap", self.config.base_url());

        debug!("Fetching Protect bootstrap");
        let resp = self
            .client
            .get(&url)
            .header("x-csrf-token", &self.csrf_token)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(UnifiError::InvalidResponse(format!(
                "Bootstrap failed with status {}: {}",
                status, body
            )));
        }

        let bootstrap: BootstrapResponse = resp.json().await?;
        info!(
            last_update_id = %bootstrap.last_update_id,
            cameras = bootstrap.cameras.len(),
            "Got Protect bootstrap"
        );

        Ok(bootstrap)
    }

    /// Make an authenticated GET request
    pub async fn get(&self, path: &str) -> Result<reqwest::Response, UnifiError> {
        let url = format!("{}{}", self.config.base_url(), path);
        let resp = self
            .client
            .get(&url)
            .header("x-csrf-token", &self.csrf_token)
            .send()
            .await?;
        Ok(resp)
    }

    /// Fetch historical events from the Network controller
    /// Returns events from newest to oldest
    #[instrument(skip(self))]
    pub async fn get_network_events(&self, limit: Option<u32>) -> Result<Vec<serde_json::Value>, UnifiError> {
        // UDM devices use /proxy/network prefix
        let url = format!(
            "{}/proxy/network/api/s/default/stat/event",
            self.config.base_url()
        );

        debug!("Fetching network events from {}", url);

        let mut req = self.client.get(&url);
        req = req.header("x-csrf-token", &self.csrf_token);

        // Add limit parameter if specified
        if let Some(limit) = limit {
            req = req.query(&[("_limit", limit.to_string())]);
        }

        let resp = req.send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(UnifiError::InvalidResponse(format!(
                "Events fetch failed with status {}: {}",
                status, body
            )));
        }

        #[derive(Deserialize)]
        struct EventsResponse {
            data: Vec<serde_json::Value>,
        }

        let events: EventsResponse = resp.json().await?;
        info!(count = events.data.len(), "Fetched network events");

        Ok(events.data)
    }

    /// Fetch system logs/events
    #[instrument(skip(self))]
    pub async fn get_system_events(&self, limit: Option<u32>) -> Result<Vec<serde_json::Value>, UnifiError> {
        // Try the system logs endpoint
        let url = format!(
            "{}/api/system/logs",
            self.config.base_url()
        );

        debug!("Fetching system events from {}", url);

        let mut req = self.client.get(&url);
        req = req.header("x-csrf-token", &self.csrf_token);

        if let Some(limit) = limit {
            req = req.query(&[("limit", limit.to_string())]);
        }

        let resp = req.send().await?;

        if !resp.status().is_success() {
            // Try alternative endpoint
            let alt_url = format!(
                "{}/proxy/network/api/s/default/stat/alarm",
                self.config.base_url()
            );
            debug!("Trying alternative endpoint: {}", alt_url);

            let resp = self.client
                .get(&alt_url)
                .header("x-csrf-token", &self.csrf_token)
                .send()
                .await?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(UnifiError::InvalidResponse(format!(
                    "System events fetch failed with status {}: {}",
                    status, body
                )));
            }

            #[derive(Deserialize)]
            struct EventsResponse {
                data: Vec<serde_json::Value>,
            }

            let events: EventsResponse = resp.json().await?;
            return Ok(events.data);
        }

        // Try to parse as array or object with data field
        let body = resp.text().await?;
        if let Ok(events) = serde_json::from_str::<Vec<serde_json::Value>>(&body) {
            return Ok(events);
        }

        #[derive(Deserialize)]
        struct EventsResponse {
            data: Option<Vec<serde_json::Value>>,
            logs: Option<Vec<serde_json::Value>>,
        }

        let parsed: EventsResponse = serde_json::from_str(&body)?;
        Ok(parsed.data.or(parsed.logs).unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_base_url() {
        let config = UnifiConfig::new("192.168.1.1", "admin", "password");
        assert_eq!(config.base_url(), "https://192.168.1.1");
    }
}
