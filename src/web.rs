//! Web server for devaipod control plane
//!
//! This module provides:
//! - Token-based authentication for API access
//! - Podman socket proxy at `/api/podman/*`
//! - Agent view at `/_devaipod/agent/{name}/` (vendored opencode UI with injected back button)
//! - Workspace recreate at `POST /api/devaipod/pods/{name}/recreate`
//! - Git and opencode-info endpoints for workspace containers
//! - Static file serving for web UI

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::prelude::*;
use color_eyre::eyre::{bail, Context, Result};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use tokio::net::UnixStream;
use tower::ServiceExt;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};

use crate::advisor;
use crate::podman::{get_container_socket, host_for_pod_services};

/// Path to the token file when using podman/Kubernetes secrets (highest priority).
const TOKEN_SECRET_PATH: &str = "/run/secrets/devaipod-web-token";

/// Default directory for persistent state when using the devaipod-state volume.
/// Override with DEVAIPOD_STATE_DIR. Token is stored at {state_dir}/web-token.
const DEFAULT_STATE_DIR: &str = "/var/lib/devaipod";

/// Filename for the web auth token inside the state directory.
const STATE_TOKEN_FILENAME: &str = "web-token";

fn state_token_path() -> std::path::PathBuf {
    let dir = std::env::var("DEVAIPOD_STATE_DIR").unwrap_or_else(|_| DEFAULT_STATE_DIR.to_string());
    std::path::PathBuf::from(dir).join(STATE_TOKEN_FILENAME)
}

/// Cookie name for attributing root-level opencode API requests and /assets/* to a pod.
/// The opencode app uses window.location.origin, so it requests /session, /rpc, /assets/* at root;
/// we set this cookie when loading the agent page; with it we serve opencode assets at /assets/*.
const DEVAIPOD_AGENT_POD_COOKIE: &str = "DEVAIPOD_AGENT_POD";
const DEVAIPOD_AUTH_COOKIE: &str = "DEVAIPOD_AUTH";

/// Extract a raw cookie value by name from the request headers.
fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            let prefix = format!("{name}=");
            s.split(';').find_map(|pair| {
                let pair = pair.trim();
                pair.starts_with(&prefix)
                    .then(|| pair[prefix.len()..].to_string())
            })
        })
}

/// Get pod name from DEVAIPOD_AGENT_POD cookie if present (URL-decoded).
fn pod_name_from_cookie(headers: &HeaderMap) -> Option<String> {
    cookie_value(headers, DEVAIPOD_AGENT_POD_COOKIE).map(|s| {
        urlencoding::decode(&s)
            .unwrap_or(std::borrow::Cow::Borrowed(s.as_str()))
            .into_owned()
    })
}

/// Normalize pod name: ensure it has the "devaipod-" prefix.
fn normalize_pod_name(name: &str) -> String {
    if name.starts_with("devaipod-") {
        name.to_string()
    } else {
        format!("devaipod-{name}")
    }
}

/// Generate a cryptographically secure random token
///
/// Returns 32 random bytes encoded as URL-safe base64 (no padding).
/// This provides 256 bits of entropy, suitable for authentication tokens.
pub fn generate_token() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let bytes: [u8; 32] = rng.random();
    BASE64_URL_SAFE_NO_PAD.encode(bytes)
}

/// Load token from secrets file, state volume, or generate a new one
///
/// Priority: (1) `/run/secrets/devaipod-web-token` (podman secret / Kubernetes),
/// (2) state dir `DEVAIPOD_STATE_DIR/web-token` (default `/var/lib/devaipod/web-token` when
/// the devaipod-state volume is mounted). If a new token is generated and the state dir exists,
/// it is written there so it persists across restarts.
pub fn load_or_generate_token() -> String {
    // 1. Podman/Kubernetes secret (highest priority)
    if let Ok(token) = std::fs::read_to_string(TOKEN_SECRET_PATH) {
        let trimmed = token.trim();
        if !trimmed.is_empty() {
            tracing::debug!("Loaded token from {}", TOKEN_SECRET_PATH);
            return trimmed.to_string();
        }
    }

    // 2. State volume path (devaipod-state mounted at DEVAIPOD_STATE_DIR)
    let state_path = state_token_path();
    if let Ok(token) = std::fs::read_to_string(&state_path) {
        let trimmed = token.trim();
        if !trimmed.is_empty() {
            tracing::debug!("Loaded token from {}", state_path.display());
            return trimmed.to_string();
        }
    }

    // 3. Generate and persist to state dir if it exists
    let token = generate_token();
    if let Some(parent) = state_path.parent() {
        if parent.exists() {
            if let Err(e) = std::fs::write(&state_path, &token) {
                tracing::warn!("Could not persist token to {}: {}", state_path.display(), e);
            } else {
                tracing::debug!("Generated and saved token to {}", state_path.display());
            }
        }
    } else {
        tracing::debug!("Generated new authentication token");
    }
    token
}

/// State of a background launch spawned by the web UI.
///
/// Completed launches are removed from the map immediately (the pod becomes
/// visible via normal podman polling). Failed launches stay until dismissed.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state")]
enum LaunchState {
    /// Subprocess is still running.
    #[serde(rename = "launching")]
    Launching,
    /// Subprocess failed with an error message.
    #[serde(rename = "failed")]
    Failed { error: String },
}

/// In-flight launches tracked by the web server, keyed by pod name.
type LaunchMap = Arc<tokio::sync::Mutex<HashMap<String, LaunchState>>>;

/// Shared state for the web server
#[derive(Clone)]
pub(crate) struct AppState {
    /// Authentication token for API access
    token: String,
    /// Path to the podman/docker socket (None if not available at startup)
    socket_path: Option<PathBuf>,
    /// Path to control-plane static files (e.g. dist/index.html)
    static_dir: String,
    /// Background launch states so the UI can track in-flight launches.
    launches: LaunchMap,
}

/// Query parameters for token authentication
#[derive(Debug, Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

/// Authentication middleware
///
/// Validates requests by checking (in order):
/// 1. `DEVAIPOD_AUTH` cookie (set by the login endpoint)
/// 2. `Authorization: Bearer ...` header
/// 3. `?token=...` query parameter
///
/// Returns 401 Unauthorized if none is present or valid.
async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    Query(query): Query<TokenQuery>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    // Check auth cookie (primary method — set by /_devaipod/login)
    if let Some(token) = cookie_value(&headers, DEVAIPOD_AUTH_COOKIE) {
        if token == state.token {
            return Ok(next.run(request).await);
        }
    }

    // Check Authorization header
    if let Some(auth_header) = headers.get(header::AUTHORIZATION) {
        if let Ok(auth_str) = auth_header.to_str() {
            if let Some(token) = auth_str.strip_prefix("Bearer ") {
                if token == state.token {
                    return Ok(next.run(request).await);
                }
            }
        }
    }

    // Check query parameter (legacy / one-off use)
    if let Some(ref token) = query.token {
        if token == &state.token {
            return Ok(next.run(request).await);
        }
    }

    Err(StatusCode::UNAUTHORIZED)
}

/// Proxy handler for podman socket
///
/// Forwards all requests under `/api/podman/*` to the podman unix socket.
/// The path after `/api/podman` is used as the request path to podman.
///
/// Example: `GET /api/podman/v1.0.0/containers/json` ->
///          `GET /v1.0.0/containers/json` on the socket
async fn podman_proxy(
    State(state): State<Arc<AppState>>,
    Path(path): Path<String>,
    request: Request,
) -> Result<Response, StatusCode> {
    // Get socket path, trying to discover it if not known at startup
    let socket_path = match &state.socket_path {
        Some(p) => p.clone(),
        None => {
            // Try to find socket now (it might have become available)
            get_container_socket().map_err(|e| {
                tracing::error!("No podman socket available: {}", e);
                StatusCode::SERVICE_UNAVAILABLE
            })?
        }
    };

    // Connect to the unix socket
    let stream = UnixStream::connect(&socket_path).await.map_err(|e| {
        tracing::error!("Failed to connect to podman socket: {}", e);
        StatusCode::BAD_GATEWAY
    })?;

    let io = TokioIo::new(stream);

    // Create HTTP client over unix socket
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(|e| {
            tracing::error!("Handshake with podman socket failed: {}", e);
            StatusCode::BAD_GATEWAY
        })?;

    // Spawn connection handler
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            tracing::error!("Connection to podman socket failed: {}", e);
        }
    });

    // Build the request to send to podman
    let (parts, body) = request.into_parts();

    // Reconstruct the URI with the path after /api/podman
    let uri = format!("/{}", path);

    let mut builder = hyper::Request::builder()
        .method(parts.method)
        .uri(&uri)
        // Podman expects a Host header even over unix socket
        .header(header::HOST, "localhost");

    // Copy headers (except Host which we set above)
    for (key, value) in parts.headers.iter() {
        if key != header::HOST {
            builder = builder.header(key, value);
        }
    }

    let proxy_request = builder.body(body).map_err(|e| {
        tracing::error!("Failed to build proxy request: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Send request and get response
    let response = sender.send_request(proxy_request).await.map_err(|e| {
        tracing::error!("Failed to send request to podman: {}", e);
        StatusCode::BAD_GATEWAY
    })?;

    // Convert the response
    let (parts, body) = response.into_parts();
    let body = Body::new(body);

    Ok(Response::from_parts(parts, body))
}

/// Get opencode connection info for a pod
///
/// Returns (published_port, api_password) for the pod's opencode server.
async fn get_pod_opencode_info(pod_name: &str) -> Result<(u16, String), StatusCode> {
    use bollard::Docker;

    let socket_path = get_container_socket().map_err(|e| {
        tracing::error!("Failed to get container socket: {}", e);
        StatusCode::SERVICE_UNAVAILABLE
    })?;

    let docker = Docker::connect_with_unix(
        &format!("unix://{}", socket_path.display()),
        120,
        bollard::API_DEFAULT_VERSION,
    )
    .map_err(|e| {
        tracing::error!("Failed to connect to container socket: {}", e);
        StatusCode::SERVICE_UNAVAILABLE
    })?;

    // Get pod info to find the API password from labels
    // The password is stored in the pod's label: io.devaipod.api-password
    let pod_info = docker
        .inspect_container(&format!("{}-agent", pod_name), None)
        .await
        .map_err(|e| {
            tracing::error!("Failed to inspect agent container for {}: {}", pod_name, e);
            StatusCode::NOT_FOUND
        })?;

    // Get the published port for opencode
    let ports = pod_info
        .network_settings
        .as_ref()
        .and_then(|ns| ns.ports.as_ref())
        .ok_or_else(|| {
            tracing::error!("No port mappings found for {}", pod_name);
            StatusCode::NOT_FOUND
        })?;

    let opencode_port = crate::pod::OPENCODE_PORT;
    let port_key = format!("{}/tcp", opencode_port);
    let bindings = ports
        .get(&port_key)
        .and_then(|b| b.as_ref())
        .ok_or_else(|| {
            tracing::error!("Port {} not published for {}", opencode_port, pod_name);
            StatusCode::NOT_FOUND
        })?;

    let host_port = bindings
        .first()
        .and_then(|b| b.host_port.as_ref())
        .and_then(|p| p.parse::<u16>().ok())
        .ok_or_else(|| {
            tracing::error!("Could not parse host port for {}", pod_name);
            StatusCode::NOT_FOUND
        })?;

    // Get the API password from pod labels
    // We need to inspect the pod itself, not the container
    let pod_inspect_output = std::process::Command::new("podman")
        .args([
            "pod",
            "inspect",
            pod_name,
            "--format",
            "{{index .Labels \"io.devaipod.api-password\"}}",
        ])
        .output()
        .map_err(|e| {
            tracing::error!("Failed to run podman pod inspect: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let api_password = String::from_utf8_lossy(&pod_inspect_output.stdout)
        .trim()
        .to_string();

    if api_password.is_empty() {
        tracing::error!("No API password found in pod labels for {}", pod_name);
        return Err(StatusCode::NOT_FOUND);
    }

    Ok((host_port, api_password))
}

/// Response for opencode info endpoint
#[derive(Debug, Serialize)]
struct OpencodeInfoResponse {
    /// URL to access the opencode web UI directly
    url: String,
    /// Published port on localhost
    port: u16,
    /// Whether the pod is accessible
    accessible: bool,
    /// Most recent session info (if any sessions exist)
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_session: Option<LatestSessionInfo>,
}

/// Info about the most recent session, used by the control plane to navigate
/// directly to it (avoiding the empty new-session view).
#[derive(Debug, Serialize)]
struct LatestSessionInfo {
    id: String,
    directory: String,
}

/// JSON body for API errors (e.g. 404) so the frontend can show a clear message
#[derive(Debug, Serialize)]
struct ApiErrorBody {
    error: String,
}

/// Path to vendored opencode web UI
const OPENCODE_UI_PATH: &str = "/usr/share/devaipod/opencode";

/// Opencode API path segments; used in tests and documentation.
/// Root-level requests with the agent cookie are proxied via the catch-all fallback.
#[cfg(test)]
const OPENCODE_API_SEGMENTS: &[&str] = &[
    "rpc", "event", "session", "global", "path", "project", "provider", "auth",
];

/// Get opencode connection info for a pod
///
/// Returns the direct URL to access the opencode web UI.
/// On 404 returns JSON body so the frontend can show "Pod or agent not found".
async fn opencode_info(
    Path(name): Path<String>,
) -> Result<Json<OpencodeInfoResponse>, (StatusCode, Json<ApiErrorBody>)> {
    let pod_name = normalize_pod_name(&name);

    let (port, _password) = get_pod_opencode_info(&pod_name).await.map_err(|code| {
        let msg = if code == StatusCode::NOT_FOUND {
            "Pod or agent not found (agent container may not be running or port not published)"
                .to_string()
        } else {
            code.to_string()
        };
        (code, Json(ApiErrorBody { error: msg }))
    })?;

    // Build URL for the opencode web UI
    let url = format!("http://127.0.0.1:{}/", port);

    // Fetch the most recent session so the control plane can navigate directly to it
    let latest_session = fetch_latest_session(port).await;

    Ok(Json(OpencodeInfoResponse {
        url,
        port,
        accessible: true,
        latest_session,
    }))
}

/// Fetch the most recent session from a pod's opencode backend.
///
/// Calls GET /session on the pod's opencode server and returns the session with
/// the most recent `time.updated` timestamp.  Returns None on any error
/// (non-fatal: the control plane just won't deep-link into a session).
async fn fetch_latest_session(port: u16) -> Option<LatestSessionInfo> {
    let host = host_for_pod_services();

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;

    let resp = client
        .get(format!("http://{}:{}/session", host, port))
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let sessions: Vec<serde_json::Value> = resp.json().await.ok()?;

    // Find the session with the most recent updated time
    sessions
        .iter()
        .filter_map(|s| {
            let id = s.get("id")?.as_str()?;
            let dir = s.get("directory")?.as_str()?;
            let updated = s.get("time")?.get("updated")?.as_u64()?;
            Some((id, dir, updated))
        })
        .max_by_key(|&(_, _, updated)| updated)
        .map(|(id, dir, _)| LatestSessionInfo {
            id: id.to_string(),
            directory: dir.to_string(),
        })
}

/// Proxy handler for opencode server root path
///
/// Handles `/api/devaipod/pods/{name}/opencode` (no trailing path)
///
/// NOTE: This proxy has limited usefulness because the opencode web UI
/// fetches assets from absolute paths (e.g., `/assets/index.js`). When
/// accessed through this proxy path, those asset requests go to devaipod's
/// root, not back through the proxy. For now, use the `/opencode-info`
/// endpoint to get a direct URL to the pod's opencode server instead.
async fn opencode_proxy_root(
    Path(name): Path<String>,
    request: Request,
) -> Result<Response, StatusCode> {
    opencode_proxy_impl(name, String::new(), request).await
}

/// Proxy handler for opencode server with path
///
/// Forwards requests to a pod's opencode server.
/// The opencode server itself proxies non-API paths to app.opencode.ai for the web UI.
///
/// Path: `/api/devaipod/pods/{name}/opencode/{*path}`
async fn opencode_proxy(
    Path((name, path)): Path<(String, String)>,
    request: Request,
) -> Result<Response, StatusCode> {
    opencode_proxy_impl(name, path, request).await
}

/// Implementation of opencode proxy
async fn opencode_proxy_impl(
    name: String,
    path: String,
    request: Request,
) -> Result<Response, StatusCode> {
    let pod_name = normalize_pod_name(&name);

    // Get the pod's opencode connection info.
    // Map NOT_FOUND (pod/agent not running or port not published) to SERVICE_UNAVAILABLE
    // so 404 is reserved for "route not found"; integration tests assert root proxy route exists (not 404).
    let (port, _password) = get_pod_opencode_info(&pod_name).await.map_err(|code| {
        if code == StatusCode::NOT_FOUND {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            code
        }
    })?;

    // When in container mode we use host gateway (e.g. host.containers.internal) so we
    // reach ports published on the host; avoids --network host and works on macOS.
    let host = host_for_pod_services();

    tracing::debug!(
        "Proxying to opencode for pod {} on {}:{}, path: {}",
        pod_name,
        host,
        port,
        path
    );

    proxy_to_upstream(&host, port, path, request).await
}

/// Low-level HTTP proxy: connect to `host:port`, forward `request`,
/// and return the response with the HTTP version normalized to 1.1.
///
/// Supports HTTP Upgrade (WebSocket): when the request contains an `Upgrade` header,
/// the proxy negotiates the upgrade with upstream and then bidirectionally copies
/// raw bytes between the client and upstream connections.
///
/// Separated from `opencode_proxy_impl` so it can be tested with a mock server
/// (the caller handles pod discovery; this function handles the TCP connection).
async fn proxy_to_upstream(
    host: &str,
    port: u16,
    path: String,
    request: Request,
) -> Result<Response, StatusCode> {
    // Connect to the published opencode port (host:port)
    let stream = tokio::net::TcpStream::connect(format!("{}:{}", host, port))
        .await
        .map_err(|e| {
            tracing::error!(
                "Failed to connect to opencode server at {}:{}: {}",
                host,
                port,
                e
            );
            StatusCode::BAD_GATEWAY
        })?;

    let io = TokioIo::new(stream);

    // Check if this is an HTTP Upgrade request (e.g. WebSocket for /pty/*)
    let is_upgrade = request.headers().get(header::UPGRADE).is_some();

    // Create HTTP client
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(|e| {
            tracing::error!("Handshake with opencode server failed: {}", e);
            StatusCode::BAD_GATEWAY
        })?;

    // Spawn connection handler. For upgrade requests, use with_upgrades()
    // so hyper keeps the connection alive after the 101 response.
    if is_upgrade {
        tokio::spawn(async move {
            if let Err(e) = conn.with_upgrades().await {
                // Upgrade connections normally close cleanly
                tracing::debug!("Upgrade connection closed: {}", e);
            }
        });
    } else {
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::error!("Connection to opencode server failed: {}", e);
            }
        });
    }

    // Build the upstream URI
    let mut uri = if path.is_empty() || path == "/" {
        "/".to_string()
    } else if path.starts_with('/') {
        path
    } else {
        format!("/{}", path)
    };

    // Preserve query string (e.g. /file?path=src/main.rs, /pty/.../connect?directory=...)
    if let Some(query) = request.uri().query() {
        uri.push('?');
        uri.push_str(query);
    }

    // Decompose the request. For upgrade requests, we reconstruct a Request
    // from the parts so hyper::upgrade::on() can extract the upgrade future.
    let (parts, body) = request.into_parts();

    let mut builder = hyper::Request::builder()
        .method(parts.method.clone())
        .uri(&uri)
        .header(header::HOST, format!("{}:{}", host, port));

    // Copy headers (except Host which we set)
    for (key, value) in parts.headers.iter() {
        if key != header::HOST {
            builder = builder.header(key, value);
        }
    }

    let proxy_request = builder.body(body).map_err(|e| {
        tracing::error!("Failed to build opencode proxy request: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Send request and get response
    let upstream_response = sender.send_request(proxy_request).await.map_err(|e| {
        tracing::error!("Failed to send request to opencode: {}", e);
        StatusCode::BAD_GATEWAY
    })?;

    // Handle HTTP Upgrade (WebSocket) responses
    if is_upgrade && upstream_response.status() == StatusCode::SWITCHING_PROTOCOLS {
        // Build the 101 response to send back to the client.
        // Reconstruct a Request from the decomposed parts so hyper can
        // extract the upgrade future (it's stored in the extensions).
        let mut response_builder = Response::builder().status(StatusCode::SWITCHING_PROTOCOLS);
        for (key, value) in upstream_response.headers() {
            response_builder = response_builder.header(key, value);
        }

        // Reconstruct the inbound request from parts — the upgrade future
        // lives in `parts.extensions` and survives the round-trip.
        let inbound_request = Request::from_parts(parts, Body::empty());

        // Spawn a task to bridge the upgraded connections once both sides
        // have completed the upgrade handshake
        tokio::spawn(async move {
            let client_upgraded = hyper::upgrade::on(inbound_request).await;
            let upstream_upgraded = hyper::upgrade::on(upstream_response).await;

            match (client_upgraded, upstream_upgraded) {
                (Ok(client), Ok(upstream)) => {
                    let mut client_io = TokioIo::new(client);
                    let mut upstream_io = TokioIo::new(upstream);
                    if let Err(e) =
                        tokio::io::copy_bidirectional(&mut client_io, &mut upstream_io).await
                    {
                        tracing::debug!("WebSocket proxy connection closed: {}", e);
                    }
                }
                (Err(e), _) => {
                    tracing::error!("Client upgrade failed: {}", e);
                }
                (_, Err(e)) => {
                    tracing::error!("Upstream upgrade failed: {}", e);
                }
            }
        });

        return response_builder.body(Body::empty()).map_err(|e| {
            tracing::error!("Failed to build upgrade response: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        });
    }

    // Normal (non-upgrade) response: normalize HTTP version for SSE/chunked support
    let (mut parts, body) = upstream_response.into_parts();
    parts.version = hyper::Version::HTTP_11;
    let body = Body::new(body);

    Ok(Response::from_parts(parts, body))
}

/// Get the published host port for the pod-api sidecar container.
///
/// The pod-api container listens on `POD_API_PORT` (8090) inside the pod.
/// Since we publish that port when creating the pod, we can discover the
/// host-mapped port by inspecting the infra container's port bindings —
/// the same way we discover the opencode port.
async fn get_pod_api_port(pod_name: &str) -> Result<u16, StatusCode> {
    use bollard::Docker;

    let socket_path = get_container_socket().map_err(|e| {
        tracing::error!("Failed to get container socket: {}", e);
        StatusCode::SERVICE_UNAVAILABLE
    })?;

    let docker = Docker::connect_with_unix(
        &format!("unix://{}", socket_path.display()),
        120,
        bollard::API_DEFAULT_VERSION,
    )
    .map_err(|e| {
        tracing::error!("Failed to connect to container socket: {}", e);
        StatusCode::SERVICE_UNAVAILABLE
    })?;

    // Inspect the agent container to find the published port mapping.
    // All containers in the pod share the same network namespace, so port
    // bindings appear on any container — we use the agent container since
    // get_pod_opencode_info already does so.
    let info = docker
        .inspect_container(&format!("{pod_name}-agent"), None)
        .await
        .map_err(|e| {
            tracing::error!("Failed to inspect agent container for {pod_name}: {e}");
            StatusCode::NOT_FOUND
        })?;

    let ports = info
        .network_settings
        .as_ref()
        .and_then(|ns| ns.ports.as_ref())
        .ok_or_else(|| {
            tracing::error!("No port mappings found for {pod_name}");
            StatusCode::NOT_FOUND
        })?;

    let api_port = crate::pod::POD_API_PORT;
    let port_key = format!("{api_port}/tcp");
    let bindings = ports
        .get(&port_key)
        .and_then(|b| b.as_ref())
        .ok_or_else(|| {
            tracing::error!("Port {api_port} not published for {pod_name}");
            StatusCode::NOT_FOUND
        })?;

    bindings
        .first()
        .and_then(|b| b.host_port.as_ref())
        .and_then(|p| p.parse::<u16>().ok())
        .ok_or_else(|| {
            tracing::error!("Could not parse host port for pod-api of {pod_name}");
            StatusCode::NOT_FOUND
        })
}

/// Catch-all proxy handler for the per-pod API sidecar.
///
/// Routes `/api/devaipod/pods/{name}/pod-api/{*path}` to the pod-api container
/// at `http://{host}:{published_port}/{path}`.
async fn pod_api_proxy(
    Path((name, path)): Path<(String, String)>,
    request: Request,
) -> Result<Response, StatusCode> {
    let pod_name = normalize_pod_name(&name);

    let port = get_pod_api_port(&pod_name).await.map_err(|code| {
        if code == StatusCode::NOT_FOUND {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            code
        }
    })?;

    let host = host_for_pod_services();

    tracing::debug!(
        "Proxying to pod-api for pod {} on {}:{}, path: {}",
        pod_name,
        host,
        port,
        path
    );

    proxy_to_upstream(&host, port, path, request).await
}

/// Helper: proxy a request to a pod's pod-api sidecar at the given path.
async fn proxy_to_pod_api(
    name: &str,
    upstream_path: String,
    request: Request,
) -> Result<Response, StatusCode> {
    let pod_name = normalize_pod_name(name);
    let port = get_pod_api_port(&pod_name).await.map_err(|code| {
        if code == StatusCode::NOT_FOUND {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            code
        }
    })?;
    let host = host_for_pod_services();
    proxy_to_upstream(&host, port, upstream_path, request).await
}

/// Proxy PTY root requests (`GET /pty`, `POST /pty`) to the pod-api sidecar.
async fn pty_pod_api_proxy_root(
    Path(name): Path<String>,
    request: Request,
) -> Result<Response, StatusCode> {
    proxy_to_pod_api(&name, "pty".to_string(), request).await
}

/// Proxy PTY sub-path requests (`GET/PUT/DELETE /pty/{id}`, `GET /pty/{id}/connect`)
/// to the pod-api sidecar.
async fn pty_pod_api_proxy(
    Path((name, rest)): Path<(String, String)>,
    request: Request,
) -> Result<Response, StatusCode> {
    proxy_to_pod_api(&name, format!("pty/{rest}"), request).await
}

/// Return a long-lived SSE stream that sends periodic keepalive comments.
/// The opencode SDK (global-sdk) calls GET /global/event as a streaming SSE connection;
/// if we close the response immediately the SDK reconnects in a tight loop, flooding the
/// console with "event stream error". A keepalive stream keeps the connection open.
fn sse_keepalive_stream(comment: &str) -> Body {
    let initial = format!(": {comment}\n\n");
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, std::io::Error>>(2);
    tokio::spawn(async move {
        if tx.send(Ok(initial)).await.is_err() {
            return;
        }
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            if tx.send(Ok(": keepalive\n\n".to_string())).await.is_err() {
                return;
            }
        }
    });
    Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx))
}

/// Whether a path is an SSE event-stream endpoint (global-sdk, event listener).
fn is_event_stream_path(path: &str) -> bool {
    path == "event" || path.starts_with("event/") || path == "global" || path.starts_with("global/")
}

/// Build a 200 OK SSE keepalive response with the given comment.
fn sse_keepalive_response(comment: &str) -> Result<Response, StatusCode> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(sse_keepalive_stream(comment))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// Middleware that logs every HTTP request and response (method, URI, status, duration, content-type).
/// Query parameters are stripped to avoid leaking tokens into logs.
async fn request_trace(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let start = std::time::Instant::now();
    let response = next.run(request).await;
    let latency = start.elapsed();
    let ct = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");
    tracing::info!(
        %method,
        path = %path,
        status = %response.status(),
        content_type = ct,
        version = ?response.version(),
        latency_ms = latency.as_millis(),
    );
    response
}

/// Frontend error report sent by the injected console.error interceptor.
#[derive(Deserialize)]
struct FrontendErrorReport {
    message: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    stack: String,
    #[serde(default)]
    context: String,
}

/// Receives frontend error reports POSTed by the injected console.error interceptor.
/// Logs them server-side so they appear in `RUST_LOG=devaipod=debug` output alongside
/// request traces, making it possible to correlate client and server events.
async fn frontend_error_report(Json(report): Json<FrontendErrorReport>) -> StatusCode {
    tracing::warn!(
        url = %report.url,
        context = %report.context,
        stack = %report.stack,
        "[frontend] {}",
        report.message,
    );
    StatusCode::NO_CONTENT
}

/// Proxy opencode API requests that hit the root (e.g. /session, /rpc, /global/event).
/// The opencode app uses window.location.origin, so it requests these at root; we attribute
/// the request to a pod via the DEVAIPOD_AGENT_POD cookie set when loading the agent page.
///
/// For event-stream paths (SSE), errors are returned as keepalive streams instead of HTTP
/// error codes, because the opencode global-sdk reconnects aggressively on non-200 responses.
async fn opencode_root_proxy(request: Request) -> Result<Response, StatusCode> {
    let path = request.uri().path().trim_start_matches('/').to_string();
    if path.is_empty() {
        return Err(StatusCode::NOT_FOUND);
    }
    let pod_name = match pod_name_from_cookie(request.headers()) {
        Some(n) => n,
        None => {
            if is_event_stream_path(&path) {
                return sse_keepalive_response("no agent context");
            }
            tracing::debug!(
                "Root opencode API request without {} cookie",
                DEVAIPOD_AGENT_POD_COOKIE
            );
            return Err(StatusCode::BAD_REQUEST);
        }
    };
    match opencode_proxy_impl(pod_name, path.clone(), request).await {
        Ok(resp) => Ok(resp),
        Err(status) => {
            if is_event_stream_path(&path) {
                tracing::debug!("SSE proxy failed ({status}), returning keepalive stream");
                sse_keepalive_response("agent not ready")
            } else {
                Err(status)
            }
        }
    }
}

/// Fallback handler: if the DEVAIPOD_AGENT_POD cookie is set, proxy the request to the
/// opencode backend for that pod. Otherwise, serve from the control-plane static directory
/// or the vendored opencode UI directory (for root-level files like oc-theme-preload.js,
/// favicons, etc. that the opencode index.html references with absolute paths).
///
/// SSE event-stream paths (e.g. /event, /global/event) without a cookie get an SSE
/// keepalive response instead of 404, so the opencode global-sdk doesn't error loop.
async fn opencode_or_static_fallback(
    State(state): State<Arc<AppState>>,
    request: Request,
) -> Response {
    let has_cookie = pod_name_from_cookie(request.headers()).is_some();
    let path = request.uri().path();
    let trimmed_path = path.trim_start_matches('/');

    // SSE paths without a cookie: return a keepalive stream so the SDK doesn't error-loop
    if !has_cookie && is_event_stream_path(trimmed_path) {
        return sse_keepalive_response("no agent context").unwrap_or_else(|status| {
            Response::builder()
                .status(status)
                .body(Body::empty())
                .unwrap()
        });
    }

    // If the path looks like a static file (has a file extension), check the vendored
    // opencode UI directory first, then the old control-plane static directory.
    if has_file_extension(trimmed_path) {
        let opencode_req = Request::builder()
            .uri(request.uri().clone())
            .body(Body::empty())
            .unwrap();
        let resp = ServeDir::new(OPENCODE_UI_PATH)
            .oneshot(opencode_req)
            .await
            .unwrap()
            .into_response();
        if resp.status() != StatusCode::NOT_FOUND {
            return resp;
        }
        // Also check the old control-plane static directory.
        let static_req = Request::builder()
            .uri(request.uri().clone())
            .body(Body::empty())
            .unwrap();
        let resp = ServeDir::new(&state.static_dir)
            .oneshot(static_req)
            .await
            .unwrap()
            .into_response();
        if resp.status() != StatusCode::NOT_FOUND {
            return resp;
        }
    }

    // The root path "/" is handled by an explicit route (serve_spa_page), so
    // it won't reach here.  The cookie persists across navigation so the
    // browser still sends it when the user navigates away from an agent session.
    // The opencode API never uses "/" — its endpoints are /session, /rpc, etc.
    let is_root = trimmed_path.is_empty();

    if has_cookie && !is_root {
        match opencode_root_proxy(request).await {
            Ok(resp) => resp,
            Err(status) => Response::builder()
                .status(status)
                .body(Body::empty())
                .unwrap(),
        }
    } else {
        // For any non-file, non-API path (e.g. /pods, /some-dir/session/123),
        // serve the opencode SPA so client-side routing can handle it.
        match opencode_index_with_script().await {
            Ok(resp) => resp,
            Err(status) => Response::builder()
                .status(status)
                .body(Body::empty())
                .unwrap(),
        }
    }
}

/// Check if a path has a file extension (e.g. "foo.js", "bar.css").
fn has_file_extension(path: &str) -> bool {
    path.rsplit_once('/')
        .map_or(path, |(_dir, file)| file)
        .contains('.')
}

/// Script injected right after <head> in the opencode index.html.
///
/// Intercepts console.error/warn and POSTs them to the server for correlation
/// with request traces.  Suppresses the harmless "[global-sdk] event stream
/// error" emitted when an SSE fetch() is aborted during page navigation.
///
/// Note: localStorage scoping per pod is handled at build time by patching
/// the opencode SPA's persist system (see `patches/opencode-scope-localstorage.patch`).
const DEVAIPOD_HEAD_SCRIPT: &str = r#"<script>(function(){
var _err=console.error;var _warn=console.warn;
function report(level,args){
  try{
    var msg=Array.prototype.map.call(args,function(a){
      return typeof a==='object'?JSON.stringify(a):String(a);
    }).join(' ');
    var stack='';try{throw new Error();}catch(e){stack=e.stack||'';}
    fetch('/_devaipod/frontend-error',{method:'POST',
      headers:{'Content-Type':'application/json'},
      body:JSON.stringify({message:'['+level+'] '+msg,url:location.href,stack:stack,
        context:navigator.userAgent})
    }).catch(function(){});
  }catch(e){}
}
console.error=function(){
  var first=arguments[0];
  if(typeof first==='string'&&first.indexOf('[global-sdk] event stream error')===0)return;
  _err.apply(console,arguments);report('error',arguments);
};
console.warn=function(){_warn.apply(console,arguments);report('warn',arguments);};
window.addEventListener('unhandledrejection',function(e){
  report('unhandledrejection',[e.reason]);
});
window.__DEVAIPOD__=true;
})()</script>"#;

/// Wrapper HTML page that embeds the opencode SPA in a full-screen iframe with a
/// thin navigation bar. This avoids the fragile HTML/CSS rewriting that was previously
/// used to inject a back button into the opencode index.html.  The opencode SPA runs
/// unmodified inside the iframe at the same origin, so its API calls (/session, /rpc,
/// /global/event, etc.) are handled by the cookie-based fallback proxy.
fn agent_iframe_wrapper(name: &str) -> String {
    let escaped_name = name
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    format!(
        r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1.0">
<title>devaipod - {escaped_name}</title>
<style>
*{{margin:0;padding:0;box-sizing:border-box}}
html,body{{height:100%;overflow:hidden;background:#1c1717}}
#dbar{{height:44px;display:flex;align-items:center;padding:0 12px;
  background:#1c1717;border-bottom:1px solid rgba(255,255,255,0.12)}}
#dbar a{{color:#e8e2e2;text-decoration:none;font-size:14px;font-weight:500;
  font-family:Inter,system-ui,sans-serif;
  padding:6px 14px;border-radius:6px;
  background:rgba(255,255,255,0.08);border:1px solid rgba(255,255,255,0.15);
  transition:background 0.15s,border-color 0.15s}}
#dbar a:hover{{background:rgba(255,255,255,0.14);border-color:rgba(255,255,255,0.25)}}
iframe{{width:100%;height:calc(100% - 44px);border:none}}
</style></head><body>
<div id="dbar"><a id="db" href="/">&#8592; Pods</a></div>
<iframe id="oc" src="/_devaipod/opencode-ui" allow="clipboard-read; clipboard-write"></iframe>
<script>(function(){{
var t=sessionStorage.getItem('devaipod_token');
if(t)document.getElementById('db').href='/?token='+encodeURIComponent(t);
// Deep-link the iframe to the most recent session.
// The control plane passes session route as a hash: #/<base64dir>/session/<id>.
// After the SPA loads, we navigate it by dispatching the opencode deep-link
// custom event with a direct URL.  Same-origin iframe access.
var route=window.location.hash.slice(1);
if(route){{var f=document.getElementById('oc');
f.addEventListener('load',function(){{try{{
f.contentWindow.history.pushState(null,'',route);
f.contentWindow.dispatchEvent(new PopStateEvent('popstate'));
}}catch(e){{}}}},{{once:true}})}}
}})()
</script></body></html>"#
    )
}

/// Redirect /_devaipod/agent/{name} to /_devaipod/agent/{name}/ and set the agent cookie.
async fn agent_wrapper(Path(name): Path<String>) -> Result<Response, StatusCode> {
    let location = format!("/_devaipod/agent/{}/", urlencoding::encode(&name));
    let cookie_value = format!(
        "{}={}; Path=/; SameSite=Lax; Max-Age=86400",
        DEVAIPOD_AGENT_POD_COOKIE,
        urlencoding::encode(&name)
    );
    Ok(Response::builder()
        .status(StatusCode::TEMPORARY_REDIRECT)
        .header(header::LOCATION, location)
        .header(header::SET_COOKIE, cookie_value)
        .body(Body::empty())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?)
}

/// Serve the iframe wrapper page for a specific agent.
/// The opencode SPA runs unmodified inside the iframe; the wrapper provides
/// the "back to pods" navigation bar. Also sets the agent cookie so that
/// subsequent requests from the iframe (API calls, assets) are routed correctly.
async fn agent_ui_root(Path(name): Path<String>) -> Response {
    let cookie_value = format!(
        "{}={}; Path=/; SameSite=Lax; Max-Age=86400",
        DEVAIPOD_AGENT_POD_COOKIE,
        urlencoding::encode(&name)
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::SET_COOKIE, cookie_value)
        .body(Body::from(agent_iframe_wrapper(&name)))
        .unwrap()
}

/// Handler for agent UI sub-paths (static assets under /_devaipod/agent/{name}/).
/// With the iframe approach, the opencode SPA's API calls go to the root origin
/// (handled by the cookie-based fallback proxy), so this only serves static files.
async fn agent_ui_handler(
    Path((_name, path)): Path<(String, String)>,
    _request: Request,
) -> Result<Response, StatusCode> {
    serve_opencode_static(&path).await
}

/// Read the opencode SPA's index.html and inject the `DEVAIPOD_HEAD_SCRIPT`.
/// Shared by `serve_opencode_raw_ui` (iframe) and `serve_pods_page` (top-level /pods).
async fn opencode_index_with_script() -> Result<Response, StatusCode> {
    let ui_path = std::path::Path::new(OPENCODE_UI_PATH).join("index.html");
    let content = tokio::fs::read(&ui_path).await.map_err(|e| {
        tracing::error!("Failed to read opencode index.html: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let html = String::from_utf8_lossy(&content);
    let html = html.replacen("<head>", &format!("<head>\n{DEVAIPOD_HEAD_SCRIPT}"), 1);
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(html))
        .unwrap())
}

/// Serve the raw opencode UI (index.html) for loading inside the agent iframe.
/// Only the error interceptor script is injected; no URL rewriting or back button.
async fn serve_opencode_raw_ui() -> Result<Response, StatusCode> {
    opencode_index_with_script().await
}

/// Redirect `/` to `/pods`.
async fn redirect_to_pods() -> Response {
    Response::builder()
        .status(StatusCode::TEMPORARY_REDIRECT)
        .header(header::LOCATION, "/pods")
        .body(Body::empty())
        .unwrap()
}

/// Login endpoint: validates the token and sets an HttpOnly auth cookie.
/// This is the initial entry point — the startup URL points here.
async fn login(
    State(state): State<Arc<AppState>>,
    Query(query): Query<TokenQuery>,
) -> Response {
    let valid = query.token.as_ref().is_some_and(|t| t == &state.token);
    if !valid {
        return Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("Invalid or missing token"))
            .unwrap();
    }
    let cookie = format!(
        "{}={}; HttpOnly; SameSite=Lax; Path=/; Max-Age=86400",
        DEVAIPOD_AUTH_COOKIE,
        state.token
    );
    Response::builder()
        .status(StatusCode::TEMPORARY_REDIRECT)
        .header(header::LOCATION, "/pods")
        .header(header::SET_COOKIE, cookie)
        .body(Body::empty())
        .unwrap()
}

/// Serve the opencode SPA with the devaipod head script injected.
/// The SPA handles client-side routing internally.
async fn serve_spa_page() -> Result<Response, StatusCode> {
    opencode_index_with_script().await
}

/// Serve the old control-plane UI at /_devaipod/oldui.
async fn serve_old_ui(State(state): State<Arc<AppState>>) -> Result<Response, StatusCode> {
    let index_path = std::path::Path::new(&state.static_dir).join("index.html");
    let content = tokio::fs::read(&index_path).await.map_err(|e| {
        tracing::error!("Failed to read old UI index.html: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(content))
        .unwrap())
}

/// Serve /assets/* from the vendored opencode UI directory.
///
/// The control-plane UI (`dist/index.html`) uses inline styles and has no `/assets/`
/// subdirectory. All `/assets/*` requests come from the opencode SPA, so we always
/// serve from `OPENCODE_UI_PATH`. The cookie is only needed to distinguish which
/// pod's opencode *backend* to proxy API calls to (handled by the fallback handler),
/// not for static asset serving.
async fn serve_root_assets(Path(path): Path<String>) -> Result<Response, StatusCode> {
    let opencode_path = if path.is_empty() {
        "assets".to_string()
    } else {
        format!("assets/{}", path)
    };
    serve_opencode_static(&opencode_path).await
}

/// Serve a static file from the vendored opencode UI directory.
///
/// Uses `tower_http::services::ServeDir` for mime type detection, path traversal
/// protection, and SPA fallback (index.html for paths without file extensions).
async fn serve_opencode_static(path: &str) -> Result<Response, StatusCode> {
    let index_html = format!("{}/index.html", OPENCODE_UI_PATH);
    let serve_dir = ServeDir::new(OPENCODE_UI_PATH).fallback(ServeFile::new(&index_html));

    // Build a GET request for the path
    let uri = format!("/{}", path.trim_start_matches('/'));
    let request = Request::builder()
        .uri(&uri)
        .body(Body::empty())
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    // ServeDir with a fallback has error type Infallible, so unwrap is safe.
    Ok(serve_dir.oneshot(request).await.unwrap().into_response())
}

/// Request body for run endpoint
#[derive(Debug, Deserialize)]
struct RunRequest {
    source: Option<String>,
    task: Option<String>,
    name: Option<String>,
    /// Override the container image (skip devcontainer.json build)
    image: Option<String>,
    /// Service-gator scopes (e.g. "github:org/repo", "github:org/*:write")
    #[serde(default)]
    service_gator_scopes: Vec<String>,
    /// Custom service-gator container image
    service_gator_image: Option<String>,
    /// Suppress default write service-gator scopes
    #[serde(default)]
    service_gator_ro: bool,
    /// Additional MCP servers to attach (name=url format)
    #[serde(default)]
    mcp_servers: Vec<String>,
}

/// Response for run endpoint
#[derive(Debug, Serialize)]
struct RunResponse {
    /// Whether the operation succeeded
    success: bool,
    /// The workspace name (short name without devaipod- prefix)
    workspace: String,
    /// Status message
    message: String,
    /// Launch status for async launches ("launching", "completed", "failed")
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    /// Full pod name (with devaipod- prefix)
    #[serde(skip_serializing_if = "Option::is_none")]
    pod_name: Option<String>,
}

/// Compute a pod name for a web-initiated launch.
///
/// Uses the same sanitization and unique-suffix logic as `main.rs::make_pod_name`
/// so the name we return to the UI matches what `devaipod run --name` will create.
fn compute_pod_name(req: &RunRequest) -> String {
    if let Some(ref name) = req.name {
        // If the user gave an explicit name, normalize it.
        normalize_pod_name(name)
    } else {
        // Derive from source URL: strip scheme, take last path component as project name.
        let project = req
            .source
            .as_deref()
            .and_then(|s| s.rsplit('/').next())
            .map(|s| s.trim_end_matches(".git"))
            .filter(|s| !s.is_empty())
            .unwrap_or("workspace");
        crate::make_pod_name(project)
    }
}

/// Run a new devaipod workspace (non-blocking)
///
/// Computes the pod name up-front, spawns `devaipod run` in the background,
/// and returns immediately with `{"status":"launching", "pod_name":"..."}`.
/// The frontend polls `/api/devaipod/launches` to detect failures.
async fn run_workspace(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RunRequest>,
) -> Result<Json<RunResponse>, StatusCode> {
    let pod_name = compute_pod_name(&req);
    let short_name = pod_name
        .strip_prefix("devaipod-")
        .unwrap_or(&pod_name)
        .to_string();

    let mut cmd = tokio::process::Command::new("devaipod");
    cmd.arg("run");

    // Add source if provided
    if let Some(ref source) = req.source {
        cmd.arg(source);
    }

    // Add task if provided (as positional argument after source)
    if let Some(ref task) = req.task {
        cmd.arg(task);
    }

    // Always pass --name so the pod name matches what we told the UI.
    cmd.args(["--name", &pod_name]);

    if let Some(ref image) = req.image {
        cmd.args(["--image", image]);
    }

    for scope in &req.service_gator_scopes {
        cmd.args(["--service-gator", scope]);
    }

    if let Some(ref gator_image) = req.service_gator_image {
        cmd.args(["--service-gator-image", gator_image]);
    }

    if req.service_gator_ro {
        cmd.arg("--service-gator-ro");
    }

    for mcp in &req.mcp_servers {
        cmd.args(["--mcp", mcp]);
    }

    // Prevent stdin reads from blocking the server process
    cmd.stdin(std::process::Stdio::null());

    tracing::info!("Running devaipod (async): {:?}", cmd);

    // Guard against duplicate launches (e.g. double-submit)
    {
        let mut launches = state.launches.lock().await;
        if launches.contains_key(&pod_name) {
            tracing::warn!("Duplicate launch rejected for {}", pod_name);
            return Err(StatusCode::CONFLICT);
        }
        launches.insert(pod_name.clone(), LaunchState::Launching);
    }

    // Spawn the subprocess in the background.
    let launches = state.launches.clone();
    let pod_name_bg = pod_name.clone();
    tokio::spawn(async move {
        let result = cmd.output().await;
        let mut map = launches.lock().await;
        match result {
            Ok(output) if output.status.success() => {
                tracing::info!("devaipod run completed for {}", pod_name_bg);
                // Remove completed entries immediately; the pod is now visible
                // in podman and the UI will pick it up via normal polling.
                map.remove(&pod_name_bg);
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let msg = stderr.trim().to_string();
                tracing::error!("devaipod run failed for {}: {}", pod_name_bg, msg);
                map.insert(
                    pod_name_bg.clone(),
                    LaunchState::Failed {
                        error: if msg.is_empty() {
                            format!("Process exited with {}", output.status)
                        } else {
                            msg
                        },
                    },
                );
            }
            Err(e) => {
                tracing::error!("Failed to execute devaipod run for {}: {}", pod_name_bg, e);
                map.insert(
                    pod_name_bg.clone(),
                    LaunchState::Failed {
                        error: format!("Failed to execute: {}", e),
                    },
                );
            }
        }
    });

    Ok(Json(RunResponse {
        success: true,
        workspace: short_name,
        message: "Launching workspace in background".to_string(),
        status: Some("launching".to_string()),
        pod_name: Some(pod_name),
    }))
}

/// Return current launch states.
///
/// The UI polls this to discover failures (and to show "launching" indicators
/// before the pod appears in podman). Completed entries are removed eagerly
/// in the spawn callback above; failed entries are kept until the UI
/// acknowledges them via DELETE.
async fn list_launches(State(state): State<Arc<AppState>>) -> Json<HashMap<String, LaunchState>> {
    let map = state.launches.lock().await;
    Json(map.clone())
}

/// Dismiss (remove) a launch entry so it stops showing in the UI.
async fn dismiss_launch(
    State(state): State<Arc<AppState>>,
    Path(pod_name): Path<String>,
) -> StatusCode {
    let mut map = state.launches.lock().await;
    map.remove(&pod_name);
    StatusCode::NO_CONTENT
}

/// Request body for advisor launch endpoint
#[derive(Debug, Deserialize)]
struct AdvisorLaunchRequest {
    /// Optional task for the advisor (e.g. "check my GitHub issues")
    task: Option<String>,
}

/// Launch or check the advisor pod
///
/// If the advisor pod already exists and is running, returns its status.
/// If it doesn't exist, creates it with the appropriate image and MCP config.
async fn launch_advisor(
    Json(req): Json<AdvisorLaunchRequest>,
) -> Result<Json<RunResponse>, StatusCode> {
    // Check if advisor pod already exists
    let check = std::process::Command::new("podman")
        .args([
            "pod",
            "inspect",
            "devaipod-advisor",
            "--format",
            "{{.State}}",
        ])
        .output();

    if let Ok(output) = check {
        if output.status.success() {
            let state = String::from_utf8_lossy(&output.stdout)
                .trim()
                .to_lowercase();
            if state == "running" {
                // Advisor already running — if task provided, send it
                if let Some(ref task) = req.task {
                    let mut cmd = tokio::process::Command::new("devaipod");
                    cmd.args(["opencode", "advisor", "send", task]);
                    let _ = cmd.output().await;
                }
                return Ok(Json(RunResponse {
                    success: true,
                    workspace: "advisor".to_string(),
                    message: "Advisor is already running".to_string(),
                    status: None,
                    pod_name: None,
                }));
            } else {
                // Advisor exists but stopped — start it
                let start = tokio::process::Command::new("podman")
                    .args(["pod", "start", "devaipod-advisor"])
                    .output()
                    .await;
                if let Ok(o) = start {
                    if o.status.success() {
                        return Ok(Json(RunResponse {
                            success: true,
                            workspace: "advisor".to_string(),
                            message: "Advisor pod started".to_string(),
                            status: None,
                            pod_name: None,
                        }));
                    }
                }
            }
        }
    }

    // Advisor doesn't exist — create it via `devaipod advisor`.
    // This reuses the CLI command which handles dotfiles fallback,
    // image selection, and MCP setup internally. We pass --no-attach
    // (via an env var) so it doesn't block trying to attach.
    let mut cmd = tokio::process::Command::new("devaipod");
    cmd.arg("advisor");
    // Prevent the advisor command from trying to attach to the pod
    // (which would block the web handler indefinitely). Setting
    // DEVAIPOD_NO_ATTACH=1 is checked by cmd_advisor.
    cmd.env("DEVAIPOD_NO_ATTACH", "1");
    if let Some(ref task) = req.task {
        cmd.arg(task);
    }

    tracing::info!("Launching advisor: {:?}", cmd);

    let output = cmd.output().await.map_err(|e| {
        tracing::error!("Failed to launch advisor: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::error!("Advisor launch failed: {}", stderr);
        return Ok(Json(RunResponse {
            success: false,
            workspace: String::new(),
            message: format!("Failed to launch advisor: {}", stderr.trim()),
            status: None,
            pod_name: None,
        }));
    }

    Ok(Json(RunResponse {
        success: true,
        workspace: "advisor".to_string(),
        message: "Advisor pod created".to_string(),
        status: None,
        pod_name: None,
    }))
}

/// Advisor status response
#[derive(Debug, Serialize)]
struct AdvisorStatusResponse {
    /// Whether the advisor pod exists
    exists: bool,
    /// Pod state: "running", "stopped", or "not_found"
    state: String,
}

async fn advisor_status() -> Result<Json<AdvisorStatusResponse>, StatusCode> {
    let check = std::process::Command::new("podman")
        .args([
            "pod",
            "inspect",
            "devaipod-advisor",
            "--format",
            "{{.State}}",
        ])
        .output();

    match check {
        Ok(output) if output.status.success() => {
            let state = String::from_utf8_lossy(&output.stdout)
                .trim()
                .to_lowercase();
            Ok(Json(AdvisorStatusResponse {
                exists: true,
                state,
            }))
        }
        _ => Ok(Json(AdvisorStatusResponse {
            exists: false,
            state: "not_found".to_string(),
        })),
    }
}

/// List draft proposals from the advisor
///
/// Reads the draft store JSON file and returns all proposals as a JSON array.
/// Returns an empty array if the file doesn't exist or can't be parsed.
async fn list_proposals_api() -> Json<serde_json::Value> {
    let proposals = tokio::task::spawn_blocking(|| {
        advisor::DraftStore::load(std::path::Path::new(advisor::DRAFTS_PATH))
            .map(|s| s.proposals)
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default();

    Json(serde_json::to_value(&proposals).unwrap_or_default())
}

/// Dismiss a draft proposal by ID
///
/// Updates the proposal's status to Dismissed and persists the change.
async fn dismiss_proposal(Path(id): Path<String>) -> Result<Json<serde_json::Value>, StatusCode> {
    tokio::task::spawn_blocking(move || {
        let path = std::path::Path::new(advisor::DRAFTS_PATH);
        let mut store = advisor::DraftStore::load(path).map_err(|_| StatusCode::NOT_FOUND)?;
        store
            .update_status(&id, advisor::ProposalStatus::Dismissed)
            .ok_or(StatusCode::NOT_FOUND)?;
        store
            .save(path)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        Ok::<_, StatusCode>(())
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)??;

    Ok(Json(serde_json::json!({"success": true})))
}

/// Recreate a workspace (delete and recreate with same repo)
///
/// Runs `devaipod rebuild <pod_name>`. Requires pod to have
/// io.devaipod.repo label.
async fn recreate_workspace(
    Path(name): Path<String>,
) -> Result<Json<RunResponse>, (StatusCode, Json<ApiErrorBody>)> {
    let pod_name = normalize_pod_name(&name);

    tracing::info!("Recreating workspace: {}", pod_name);

    let output = tokio::process::Command::new("devaipod")
        .arg("rebuild")
        .arg(&pod_name)
        .output()
        .await
        .map_err(|e| {
            tracing::error!("Failed to execute devaipod rebuild: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiErrorBody {
                    error: format!("Failed to run rebuild: {}", e),
                }),
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::error!("devaipod rebuild failed: {}", stderr);
        let msg = stderr.trim();
        let status =
            if msg.contains("no repository label") || msg.contains("Cannot determine source") {
                StatusCode::BAD_REQUEST
            } else if msg.contains("not found") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
        return Err((
            status,
            Json(ApiErrorBody {
                error: msg.to_string(),
            }),
        ));
    }

    let short_name = pod_name.strip_prefix("devaipod-").unwrap_or(&pod_name);
    Ok(Json(RunResponse {
        success: true,
        workspace: short_name.to_string(),
        message: format!("Workspace '{}' recreated successfully", short_name),
        status: None,
        pod_name: None,
    }))
}

/// Extract workspace name from devaipod run output
///
/// Looks for the short workspace name (without devaipod- prefix) in the output.
/// Currently unused (run_workspace computes the name upfront) but kept for
/// potential use by other callers.
#[allow(dead_code)]
fn extract_workspace_name(output: &str) -> Option<String> {
    // Look for patterns like "devaipod-foo-abc123" or just the workspace name in output
    for line in output.lines() {
        // Check if line contains a pod name
        if let Some(start) = line.find("devaipod-") {
            // Extract the full pod name
            let rest = &line[start..];
            // Pod names are alphanumeric with hyphens, terminated by whitespace or quote
            let end = rest
                .find(|c: char| !c.is_alphanumeric() && c != '-')
                .unwrap_or(rest.len());
            let pod_name = &rest[..end];
            // Strip the prefix and return
            return Some(
                pod_name
                    .strip_prefix("devaipod-")
                    .unwrap_or(pod_name)
                    .to_string(),
            );
        }
    }
    None
}

/// Response for agent status endpoint
#[derive(Debug, Serialize)]
struct AgentStatusResponse {
    activity: String,
    status_line: Option<String>,
    current_tool: Option<String>,
    recent_output: Vec<String>,
    last_message_ts: Option<i64>,
    session_count: usize,
}

/// Maximum number of output lines to return for agent status
const AGENT_STATUS_MAX_LINES: usize = 3;

/// Get agent status for a pod
///
/// Queries the pod's opencode server to determine the agent's current state.
/// Returns a valid `AgentStatusResponse` even on errors (with activity set to
/// "Stopped" or "Unknown") so the frontend always gets a usable response.
async fn agent_status(Path(name): Path<String>) -> Json<AgentStatusResponse> {
    let pod_name = normalize_pod_name(&name);

    let stopped = AgentStatusResponse {
        activity: "Stopped".to_string(),
        status_line: None,
        current_tool: None,
        recent_output: vec![],
        last_message_ts: None,
        session_count: 0,
    };
    let unknown = AgentStatusResponse {
        activity: "Unknown".to_string(),
        status_line: None,
        current_tool: None,
        recent_output: vec![],
        last_message_ts: None,
        session_count: 0,
    };

    let (port, password) = match get_pod_opencode_info(&pod_name).await {
        Ok(info) => info,
        Err(_) => return Json(stopped),
    };

    let host = host_for_pod_services();

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return Json(unknown),
    };

    // Fetch sessions
    let sessions_resp = match client
        .get(format!("http://{}:{}/session", host, port))
        .basic_auth("opencode", Some(&password))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        _ => return Json(unknown),
    };

    let sessions: Vec<serde_json::Value> = match sessions_resp.json().await {
        Ok(s) => s,
        Err(_) => return Json(unknown),
    };

    let session_count = sessions.len();

    if sessions.is_empty() {
        return Json(AgentStatusResponse {
            activity: "Idle".to_string(),
            status_line: Some("Waiting for input...".to_string()),
            current_tool: None,
            recent_output: vec![],
            last_message_ts: None,
            session_count: 0,
        });
    }

    // Find the root session (no parentID or null parentID)
    let root_session = sessions.iter().find(|s| {
        s.get("parentID").is_none() || s.get("parentID").map(|p| p.is_null()).unwrap_or(false)
    });

    let session_id = match root_session
        .and_then(|s| s.get("id"))
        .and_then(|id| id.as_str())
    {
        Some(id) => id.to_string(),
        None => return Json(unknown),
    };

    // Fetch recent messages
    let messages_resp = match client
        .get(format!(
            "http://{}:{}/session/{}/message?limit=5",
            host, port, session_id
        ))
        .basic_auth("opencode", Some(&password))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        _ => return Json(unknown),
    };

    let messages: Vec<serde_json::Value> = match messages_resp.json().await {
        Ok(m) => m,
        Err(_) => return Json(unknown),
    };

    // Derive agent state from messages (same logic as tui.rs)
    let (activity, status_line, current_tool, recent_output, last_message_ts) =
        derive_agent_status_from_messages(&messages);

    Json(AgentStatusResponse {
        activity,
        status_line,
        current_tool,
        recent_output,
        last_message_ts,
        session_count,
    })
}

/// Derive agent status fields from opencode session messages.
///
/// This reimplements the core logic from `tui.rs::derive_agent_state_from_messages`
/// directly, returning the fields needed for `AgentStatusResponse`.
fn derive_agent_status_from_messages(
    messages: &[serde_json::Value],
) -> (
    String,         // activity
    Option<String>, // status_line
    Option<String>, // current_tool
    Vec<String>,    // recent_output
    Option<i64>,    // last_message_ts
) {
    if messages.is_empty() {
        return ("Unknown".to_string(), None, None, vec![], None);
    }

    // Find the last assistant message
    let last_assistant = messages.iter().rev().find(|msg| {
        msg.get("info")
            .and_then(|i| i.get("role"))
            .and_then(|r| r.as_str())
            == Some("assistant")
    });

    let Some(last_assistant) = last_assistant else {
        return ("Unknown".to_string(), None, None, vec![], None);
    };

    let info = match last_assistant.get("info") {
        Some(i) => i,
        None => return ("Unknown".to_string(), None, None, vec![], None),
    };

    let parts = last_assistant
        .get("parts")
        .and_then(|p| p.as_array())
        .map(|arr| arr.as_slice())
        .unwrap_or(&[]);

    // Extract recent output from parts
    let recent_output = {
        let mut lines = Vec::new();
        for part in parts {
            let part_type = part.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match part_type {
                "text" => {
                    if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                        for line in text.lines().rev().take(AGENT_STATUS_MAX_LINES) {
                            let truncated = if line.chars().count() > 80 {
                                let s: String = line.chars().take(77).collect();
                                format!("{s}...")
                            } else {
                                line.to_string()
                            };
                            if !truncated.trim().is_empty() {
                                lines.push(truncated);
                            }
                            if lines.len() >= AGENT_STATUS_MAX_LINES {
                                break;
                            }
                        }
                    }
                }
                "tool" => {
                    if let Some(tool_name) = part.get("name").and_then(|n| n.as_str()) {
                        let status = part
                            .get("state")
                            .and_then(|s| s.get("status"))
                            .and_then(|s| s.as_str())
                            .unwrap_or("running");
                        lines.push(format!("\u{2192} {tool_name}: {status}"));
                    }
                }
                _ => {}
            }
            if lines.len() >= AGENT_STATUS_MAX_LINES {
                break;
            }
        }
        lines.reverse();
        lines
    };

    // Extract current tool (first incomplete tool)
    let current_tool = parts.iter().find_map(|part| {
        if part.get("type").and_then(|t| t.as_str()) == Some("tool") {
            let status = part
                .get("state")
                .and_then(|s| s.get("status"))
                .and_then(|s| s.as_str());
            if status != Some("completed") && status != Some("error") {
                return part.get("name").and_then(|n| n.as_str()).map(String::from);
            }
        }
        None
    });

    // Build status line from first text part
    let status_line = parts.iter().find_map(|part| {
        if part.get("type").and_then(|t| t.as_str()) == Some("text") {
            part.get("text").and_then(|t| t.as_str()).map(|text| {
                let first_line = text.lines().next().unwrap_or("");
                if first_line.chars().count() > 60 {
                    let s: String = first_line.chars().take(57).collect();
                    format!("{s}...")
                } else {
                    first_line.to_string()
                }
            })
        } else {
            None
        }
    });

    // Determine activity
    let activity = if info.get("time").and_then(|t| t.get("completed")).is_none() {
        "Working"
    } else {
        let has_incomplete_tool = parts.iter().any(|part| {
            if part.get("type").and_then(|t| t.as_str()) == Some("tool") {
                let status = part
                    .get("state")
                    .and_then(|s| s.get("status"))
                    .and_then(|s| s.as_str());
                status != Some("completed") && status != Some("error")
            } else {
                false
            }
        });

        if has_incomplete_tool {
            "Working"
        } else {
            let finish = info.get("finish").and_then(|f| f.as_str()).unwrap_or("");
            if finish == "tool-calls" {
                "Working"
            } else {
                "Idle"
            }
        }
    };

    // Extract the most recent message timestamp
    let last_message_ts = messages
        .iter()
        .filter_map(|msg| {
            msg.get("info").and_then(|info| {
                info.get("time").and_then(|time| {
                    time.get("completed")
                        .or_else(|| time.get("created"))
                        .and_then(|t| t.as_i64())
                })
            })
        })
        .max();

    (
        activity.to_string(),
        status_line,
        current_tool,
        recent_output,
        last_message_ts,
    )
}

/// Response for git status endpoint
#[derive(Debug, Serialize)]
struct GitStatusResponse {
    /// Exit code from git command
    exit_code: i64,
    /// Raw output from git status --porcelain
    output: String,
    /// Parsed list of changed files
    files: Vec<GitStatusFile>,
}

/// A single file from git status output
#[derive(Debug, Serialize)]
struct GitStatusFile {
    /// Status code (e.g., "M", "A", "??")
    status: String,
    /// File path
    path: String,
}

/// Response for git diff endpoint
#[derive(Debug, Serialize)]
struct GitDiffResponse {
    /// Exit code from git command
    exit_code: i64,
    /// Raw diff output
    diff: String,
}

/// Response for git commits endpoint
#[derive(Debug, Serialize)]
struct GitCommitsResponse {
    /// Exit code from git command
    exit_code: i64,
    /// List of recent commits
    commits: Vec<GitCommit>,
}

/// A single commit from git log output
#[derive(Debug, Serialize)]
struct GitCommit {
    /// Short commit hash
    hash: String,
    /// Commit message (first line)
    message: String,
}

/// Query parameters for the git log endpoint
#[derive(Debug, Deserialize)]
struct GitLogQuery {
    /// Base ref for range (optional)
    base: Option<String>,
    /// Head ref for range (optional)
    head: Option<String>,
}

/// A detailed commit entry from git log
#[derive(Debug, Serialize, Deserialize)]
struct GitLogEntry {
    /// Full commit SHA
    sha: String,
    /// Short commit SHA
    short_sha: String,
    /// Commit message (full, not just first line)
    message: String,
    /// Author name
    author: String,
    /// Author email
    author_email: String,
    /// Commit timestamp (ISO 8601)
    timestamp: String,
    /// Parent commit SHAs
    parents: Vec<String>,
}

/// Response for the git log endpoint
#[derive(Debug, Serialize)]
struct GitLogResponse {
    commits: Vec<GitLogEntry>,
}

/// Get git status for a workspace pod
///
/// Runs `git status --porcelain` in the workspace container and returns
/// parsed results as JSON.
async fn git_status(Path(name): Path<String>) -> Result<Json<GitStatusResponse>, StatusCode> {
    let container_name = format!("devaipod-{}-workspace", name);

    let (exit_code, stdout, _stderr) =
        exec_in_container(&container_name, &["git", "status", "--porcelain"])
            .await
            .map_err(|e| {
                tracing::error!("Failed to run git status in {}: {}", container_name, e);
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

    let output = String::from_utf8_lossy(&stdout).to_string();

    // Parse the porcelain output
    let files: Vec<GitStatusFile> = output
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            // Format is: XY PATH or XY ORIG -> PATH for renames
            let status = line.chars().take(2).collect::<String>().trim().to_string();
            let path = line.chars().skip(3).collect::<String>();
            GitStatusFile { status, path }
        })
        .collect();

    Ok(Json(GitStatusResponse {
        exit_code,
        output,
        files,
    }))
}

/// Get git diff for a workspace pod
///
/// Runs `git diff HEAD` in the workspace container.
async fn git_diff(Path(name): Path<String>) -> Result<Json<GitDiffResponse>, StatusCode> {
    let container_name = format!("devaipod-{}-workspace", name);

    let (exit_code, stdout, _stderr) = exec_in_container(&container_name, &["git", "diff", "HEAD"])
        .await
        .map_err(|e| {
            tracing::error!("Failed to run git diff in {}: {}", container_name, e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let diff = String::from_utf8_lossy(&stdout).to_string();

    Ok(Json(GitDiffResponse { exit_code, diff }))
}

/// Get recent git commits for a workspace pod
///
/// Runs `git log --oneline -20` in the workspace container.
async fn git_commits(Path(name): Path<String>) -> Result<Json<GitCommitsResponse>, StatusCode> {
    let container_name = format!("devaipod-{}-workspace", name);

    let (exit_code, stdout, _stderr) =
        exec_in_container(&container_name, &["git", "log", "--oneline", "-20"])
            .await
            .map_err(|e| {
                tracing::error!("Failed to run git log in {}: {}", container_name, e);
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

    let output = String::from_utf8_lossy(&stdout);

    // Parse the oneline output
    let commits: Vec<GitCommit> = output
        .lines()
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            // Format is: HASH MESSAGE
            let mut parts = line.splitn(2, ' ');
            let hash = parts.next()?.to_string();
            let message = parts.next().unwrap_or("").to_string();
            Some(GitCommit { hash, message })
        })
        .collect();

    Ok(Json(GitCommitsResponse { exit_code, commits }))
}

/// Validate that a string looks like a safe git ref (no shell metacharacters).
fn is_valid_git_ref(s: &str) -> bool {
    !s.is_empty()
        && !s.contains("..")
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '/' | '.' | '_' | '~' | '^'))
}

/// Get structured git log for an agent pod
///
/// Runs `git log` in the workspace container with a structured format string,
/// supporting optional `base` and `head` query parameters for range filtering.
///
/// Use workspace container — it has the agent remote and is trusted.
/// Agent commits are available after `git fetch agent`.
async fn git_log(
    Path(name): Path<String>,
    Query(params): Query<GitLogQuery>,
) -> Result<Json<GitLogResponse>, (StatusCode, String)> {
    // Validate ref parameters if provided
    if let Some(ref base) = params.base {
        if !is_valid_git_ref(base) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Invalid git ref for 'base': {base}"),
            ));
        }
    }
    if let Some(ref head) = params.head {
        if !is_valid_git_ref(head) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Invalid git ref for 'head': {head}"),
            ));
        }
    }

    // Use workspace container — it has the agent remote and is trusted.
    // Agent commits are available after `git fetch agent`.
    let container_name = format!("devaipod-{name}-workspace");

    // Use %x00 (NUL) as field separator, %x1e (record separator) between commits.
    // Fields: full SHA, short SHA, subject+body, author name, author email,
    //         author date ISO, parent hashes (space-separated).
    let format_arg =
        "--format=%H%x00%h%x00%s%n%b%x00%an%x00%ae%x00%aI%x00%P%x1e".to_string();
    let range_arg: String;

    let mut cmd: Vec<&str> = vec!["git", "log", &format_arg];

    match (&params.base, &params.head) {
        (Some(base), Some(head)) => {
            range_arg = format!("{base}..{head}");
            cmd.push(&range_arg);
            cmd.push("-500");
        }
        (None, Some(head)) => {
            cmd.push(head.as_str());
            cmd.push("-50");
        }
        (Some(_base), None) => {
            return Err((
                StatusCode::BAD_REQUEST,
                "'base' requires 'head' to also be specified".to_string(),
            ));
        }
        // Default: fetch latest agent commits then show them.
        // The fetch is fast (local volume mount, no network) and ensures
        // the user always sees the agent's current state.
        (None, None) => {
            if let Err(e) = exec_in_container(&container_name, &["git", "fetch", "agent"]).await {
                tracing::warn!("git fetch agent in {container_name} failed: {e}");
                // Non-fatal: agent remote may not exist yet for fresh pods.
            }
            cmd.push("agent/HEAD");
            cmd.push("-50");
        }
    }

    let (exit_code, stdout, stderr) =
        exec_in_container(&container_name, &cmd)
            .await
            .map_err(|e| {
                tracing::error!("Failed to run git log in {container_name}: {e}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to exec in container: {e}"),
                )
            })?;

    if exit_code != 0 {
        let stderr_text = String::from_utf8_lossy(&stderr);
        // If the ref doesn't exist yet (e.g. agent remote has no commits),
        // return an empty commit list rather than a 500.
        if stderr_text.contains("unknown revision") || stderr_text.contains("bad default revision") {
            return Ok(Json(GitLogResponse {
                commits: Vec::new(),
            }));
        }
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("git log failed: {}", stderr_text),
        ));
    }

    let output = String::from_utf8_lossy(&stdout);

    // If git produced no output, return empty list (e.g. empty repo or empty range)
    if output.trim().is_empty() {
        return Ok(Json(GitLogResponse {
            commits: Vec::new(),
        }));
    }

    let commits: Vec<GitLogEntry> = output
        .split('\x1e')
        .filter(|record| !record.trim().is_empty())
        .filter_map(|record| {
            let fields: Vec<&str> = record.trim().splitn(7, '\0').collect();
            if fields.len() < 7 {
                tracing::warn!(
                    "Skipping malformed git log record ({} fields): {:?}",
                    fields.len(),
                    record
                );
                return None;
            }
            Some(GitLogEntry {
                sha: fields[0].to_string(),
                short_sha: fields[1].to_string(),
                message: fields[2].trim().to_string(),
                author: fields[3].to_string(),
                author_email: fields[4].to_string(),
                timestamp: fields[5].to_string(),
                parents: fields[6]
                    .split_whitespace()
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect(),
            })
        })
        .collect();

    Ok(Json(GitLogResponse { commits }))
}

/// Query parameters for the git diff-range endpoint
#[derive(Debug, Deserialize)]
struct GitDiffRangeQuery {
    /// Base commit SHA (required)
    base: String,
    /// Head commit SHA (required)
    head: String,
}

/// A single file's diff in structured form, compatible with opencode's FileDiff type
#[derive(Debug, Serialize)]
struct FileDiff {
    /// File path
    file: String,
    /// File content before (at base commit). Empty string for added files.
    before: String,
    /// File content after (at head commit). Empty string for deleted files.
    after: String,
    /// Number of lines added
    additions: u32,
    /// Number of lines deleted
    deletions: u32,
    /// Change status: "added", "deleted", or "modified"
    status: &'static str,
}

/// Response for the git diff-range endpoint
#[derive(Debug, Serialize)]
struct GitDiffRangeResponse {
    files: Vec<FileDiff>,
}

/// Maximum number of files allowed in a diff-range response.
/// Large changesets would require too many per-file exec calls.
const DIFF_RANGE_MAX_FILES: usize = 100;

/// Get structured per-file diffs for a commit range in a workspace pod.
///
/// Returns before/after file content, addition/deletion counts, and change
/// status for each file changed between `base` and `head`. Compatible with
/// the opencode SDK's `FileDiff` type.
///
/// Use workspace container — it has the agent remote and is trusted.
/// Agent commits are available after `git fetch agent`.
async fn git_diff_range(
    Path(name): Path<String>,
    Query(params): Query<GitDiffRangeQuery>,
) -> Result<Json<GitDiffRangeResponse>, (StatusCode, String)> {
    // Validate refs
    if !is_valid_git_ref(&params.base) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Invalid git ref for 'base': {}", params.base),
        ));
    }
    if !is_valid_git_ref(&params.head) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Invalid git ref for 'head': {}", params.head),
        ));
    }

    // Use workspace container — it has the agent remote and is trusted.
    // Agent commits are available after `git fetch agent`.
    let container_name = format!("devaipod-{name}-workspace");

    // Step 1+2: Get changed files with statuses and numstat in one pass each.
    // --no-renames treats renames as delete+add which simplifies handling.
    let name_status_cmd = [
        "git",
        "diff",
        "--name-status",
        "--no-renames",
        &params.base,
        &params.head,
    ];
    let numstat_cmd = [
        "git",
        "diff",
        "--numstat",
        "--no-renames",
        &params.base,
        &params.head,
    ];

    let (ns_result, num_result) = tokio::join!(
        exec_in_container(&container_name, &name_status_cmd),
        exec_in_container(&container_name, &numstat_cmd),
    );

    let (ns_exit, ns_stdout, ns_stderr) = ns_result.map_err(|e| {
        tracing::error!("Failed to run git diff --name-status in {container_name}: {e}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to exec in container: {e}"),
        )
    })?;

    if ns_exit != 0 {
        let stderr_text = String::from_utf8_lossy(&ns_stderr);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("git diff --name-status failed: {}", stderr_text),
        ));
    }

    let (num_exit, num_stdout, num_stderr) = num_result.map_err(|e| {
        tracing::error!("Failed to run git diff --numstat in {container_name}: {e}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to exec in container: {e}"),
        )
    })?;

    if num_exit != 0 {
        let stderr_text = String::from_utf8_lossy(&num_stderr);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("git diff --numstat failed: {}", stderr_text),
        ));
    }

    let ns_output = String::from_utf8_lossy(&ns_stdout);
    let num_output = String::from_utf8_lossy(&num_stdout);

    // If name-status produced nothing, return empty
    if ns_output.trim().is_empty() {
        return Ok(Json(GitDiffRangeResponse { files: Vec::new() }));
    }

    // Parse --name-status: each line is "STATUS\tFILENAME"
    let mut file_statuses: Vec<(&str, &'static str)> = Vec::new(); // (path, status)
    for line in ns_output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(2, '\t');
        let status_char = parts.next().unwrap_or("").trim();
        let file_path = match parts.next() {
            Some(p) => p.trim(),
            None => continue,
        };
        let status: &'static str = match status_char {
            "A" => "added",
            "D" => "deleted",
            _ => "modified",
        };
        file_statuses.push((file_path, status));
    }

    if file_statuses.len() > DIFF_RANGE_MAX_FILES {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "Too many changed files ({}, max {})",
                file_statuses.len(),
                DIFF_RANGE_MAX_FILES
            ),
        ));
    }

    // Parse --numstat: each line is "ADDS\tDELS\tFILENAME"
    // Binary files show "-\t-\tFILENAME"
    let mut numstat_map: std::collections::HashMap<&str, (u32, u32)> =
        std::collections::HashMap::new();
    for line in num_output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.splitn(3, '\t').collect();
        if fields.len() < 3 {
            continue;
        }
        let adds = fields[0].parse::<u32>().unwrap_or(0);
        let dels = fields[1].parse::<u32>().unwrap_or(0);
        numstat_map.insert(fields[2].trim(), (adds, dels));
    }

    // Step 3: Fetch before/after content for all files in two batched execs
    // (one for base, one for head) instead of per-file execs.  Each exec
    // runs a shell loop that outputs file contents separated by a NUL byte
    // delimiter.  This reduces 2N exec calls to 2, which matters because
    // each exec has ~200-500ms overhead through the podman VM on macOS.
    const FILE_SEPARATOR: &str = "\x00DEVAIPOD_FILE_SEP\x00";

    let base_files: Vec<&str> = file_statuses
        .iter()
        .filter(|(_, status)| *status != "added")
        .map(|(path, _)| *path)
        .collect();
    let head_files: Vec<&str> = file_statuses
        .iter()
        .filter(|(_, status)| *status != "deleted")
        .map(|(path, _)| *path)
        .collect();

    // Build a shell script that cats each file at the given ref, separated
    // by the delimiter.  Using printf to avoid echo interpreting escapes.
    fn build_batch_script(ref_name: &str, files: &[&str], sep: &str) -> String {
        let mut script = String::new();
        for (i, file) in files.iter().enumerate() {
            if i > 0 {
                script.push_str(&format!("printf '{sep}';")); // delimiter between files
            }
            script.push_str(&format!(
                "git show '{ref_name}:{file}' 2>/dev/null || true;",
            ));
        }
        script
    }

    let (base_contents, head_contents) = tokio::join!(
        async {
            if base_files.is_empty() {
                return std::collections::HashMap::new();
            }
            let script = build_batch_script(&params.base, &base_files, FILE_SEPARATOR);
            match exec_in_container(&container_name, &["sh", "-c", &script]).await {
                Ok((_code, stdout, _stderr)) => {
                    let output = String::from_utf8(stdout)
                        .unwrap_or_else(|e| String::from_utf8_lossy(&e.into_bytes()).into_owned());
                    let parts: Vec<&str> = output.split(FILE_SEPARATOR).collect();
                    let mut map = std::collections::HashMap::new();
                    for (i, file) in base_files.iter().enumerate() {
                        map.insert(
                            file.to_string(),
                            parts.get(i).unwrap_or(&"").to_string(),
                        );
                    }
                    map
                }
                Err(e) => {
                    tracing::error!("Failed to batch-fetch base content: {e}");
                    std::collections::HashMap::new()
                }
            }
        },
        async {
            if head_files.is_empty() {
                return std::collections::HashMap::new();
            }
            let script = build_batch_script(&params.head, &head_files, FILE_SEPARATOR);
            match exec_in_container(&container_name, &["sh", "-c", &script]).await {
                Ok((_code, stdout, _stderr)) => {
                    let output = String::from_utf8(stdout)
                        .unwrap_or_else(|e| String::from_utf8_lossy(&e.into_bytes()).into_owned());
                    let parts: Vec<&str> = output.split(FILE_SEPARATOR).collect();
                    let mut map = std::collections::HashMap::new();
                    for (i, file) in head_files.iter().enumerate() {
                        map.insert(
                            file.to_string(),
                            parts.get(i).unwrap_or(&"").to_string(),
                        );
                    }
                    map
                }
                Err(e) => {
                    tracing::error!("Failed to batch-fetch head content: {e}");
                    std::collections::HashMap::new()
                }
            }
        },
    );

    let mut files = Vec::with_capacity(file_statuses.len());
    for &(file_path, status) in &file_statuses {
        let (adds, dels) = numstat_map.get(file_path).copied().unwrap_or((0, 0));
        files.push(FileDiff {
            file: file_path.to_string(),
            before: base_contents.get(file_path).cloned().unwrap_or_default(),
            after: head_contents.get(file_path).cloned().unwrap_or_default(),
            additions: adds,
            deletions: dels,
            status,
        });
    }

    Ok(Json(GitDiffRangeResponse { files }))
}

#[derive(Debug, Serialize)]
struct GitFetchResponse {
    /// Whether new commits were fetched
    success: bool,
    /// Human-readable summary
    message: String,
}

/// Fetch latest commits from the agent remote
///
/// Runs `git fetch agent` in the workspace container to pull
/// the agent's latest commits for review.
async fn git_fetch_agent(
    Path(name): Path<String>,
) -> Result<Json<GitFetchResponse>, (StatusCode, String)> {
    let container_name = format!("devaipod-{name}-workspace");

    let (exit_code, _stdout, stderr) =
        exec_in_container(&container_name, &["git", "fetch", "agent"])
            .await
            .map_err(|e| {
                tracing::error!("Failed to run git fetch agent in {container_name}: {e}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to exec in container: {e}"),
                )
            })?;

    if exit_code != 0 {
        let stderr_text = String::from_utf8_lossy(&stderr);
        return Ok(Json(GitFetchResponse {
            success: false,
            message: format!("git fetch agent failed: {}", stderr_text),
        }));
    }

    Ok(Json(GitFetchResponse {
        success: true,
        message: "Fetched latest agent commits".to_string(),
    }))
}

#[derive(Debug, Deserialize)]
struct GitPushRequest {
    branch: String,
}

#[derive(Debug, Serialize)]
struct GitPushResponse {
    success: bool,
    message: String,
}

/// Push a branch to origin from the workspace
///
/// Used after human approves agent commits — the workspace has GH_TOKEN
/// and can push directly.
async fn git_push(
    Path(name): Path<String>,
    Json(body): Json<GitPushRequest>,
) -> Result<Json<GitPushResponse>, (StatusCode, String)> {
    if !is_valid_git_ref(&body.branch) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Invalid branch name: {}", body.branch),
        ));
    }

    let container_name = format!("devaipod-{name}-workspace");

    let (exit_code, _stdout, stderr) =
        exec_in_container(&container_name, &["git", "push", "origin", &body.branch])
            .await
            .map_err(|e| {
                tracing::error!("Failed to run git push in {container_name}: {e}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to exec in container: {e}"),
                )
            })?;

    if exit_code != 0 {
        let stderr_text = String::from_utf8_lossy(&stderr);
        return Ok(Json(GitPushResponse {
            success: false,
            message: format!("git push failed: {}", stderr_text),
        }));
    }

    Ok(Json(GitPushResponse {
        success: true,
        message: format!("Pushed branch '{}' to origin", body.branch),
    }))
}

/// Execute a command in a container and return output
///
/// Uses bollard to execute the command and capture stdout/stderr.
async fn exec_in_container(container: &str, cmd: &[&str]) -> Result<(i64, Vec<u8>, Vec<u8>)> {
    use bollard::exec::{CreateExecOptions, StartExecResults};
    use bollard::Docker;
    use futures_util::StreamExt;

    let socket_path = get_container_socket()?;
    let docker = Docker::connect_with_unix(
        &format!("unix://{}", socket_path.display()),
        120,
        bollard::API_DEFAULT_VERSION,
    )
    .context("Failed to connect to container socket")?;

    let exec = docker
        .create_exec(
            container,
            CreateExecOptions {
                cmd: Some(cmd.to_vec()),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                ..Default::default()
            },
        )
        .await
        .context("Failed to create exec")?;

    let result = docker
        .start_exec(&exec.id, None)
        .await
        .context("Failed to start exec")?;

    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();

    match result {
        StartExecResults::Attached { mut output, .. } => {
            while let Some(chunk) = output.next().await {
                match chunk {
                    Ok(bollard::container::LogOutput::StdOut { message }) => {
                        stdout_buf.extend_from_slice(&message);
                    }
                    Ok(bollard::container::LogOutput::StdErr { message }) => {
                        stderr_buf.extend_from_slice(&message);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("Exec output error: {}", e);
                    }
                }
            }
        }
        StartExecResults::Detached => {}
    }

    let inspect = docker
        .inspect_exec(&exec.id)
        .await
        .context("Failed to inspect exec")?;

    let exit_code = inspect.exit_code.unwrap_or(-1);
    Ok((exit_code, stdout_buf, stderr_buf))
}

/// Run the web server
///
/// Starts an HTTP server on the specified port with:
/// - Token-based authentication on `/api/*` routes
/// - Podman socket proxy at `/api/podman/*`
/// - Git endpoints at `/api/devaipod/pods/{name}/git/*`
/// - Static file serving from `dist/` directory
///
/// # Arguments
///
/// * `port` - TCP port to bind to (on 127.0.0.1 only)
/// * `token` - Authentication token for API access
///
/// # Errors
///
/// Returns an error if the server fails to bind to the port.
/// Podman socket availability is checked lazily when proxying requests.

/// Build the web app router for a given token, socket path, and static dir.
///
/// Exposed for tests so we can hit the router with in-process requests (fast)
/// without starting a server or container.
pub(crate) fn build_app(token: String, socket_path: Option<PathBuf>, static_dir: &str) -> Router {
    let state = Arc::new(AppState {
        token: token.clone(),
        socket_path,
        static_dir: static_dir.to_string(),
        launches: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
    });

    // Build the API router with authentication
    let api_router = Router::new()
        .route(
            "/podman/{*path}",
            get(podman_proxy)
                .post(podman_proxy)
                .put(podman_proxy)
                .delete(podman_proxy),
        )
        .route(
            "/devaipod/pods/{name}/opencode",
            get(opencode_proxy_root)
                .post(opencode_proxy_root)
                .put(opencode_proxy_root)
                .delete(opencode_proxy_root),
        )
        .route(
            "/devaipod/pods/{name}/opencode/{*path}",
            get(opencode_proxy)
                .post(opencode_proxy)
                .put(opencode_proxy)
                .delete(opencode_proxy),
        )
        .route("/devaipod/pods/{name}/opencode-info", get(opencode_info))
        .route("/devaipod/pods/{name}/agent-status", get(agent_status))
        .route("/devaipod/pods/{name}/git/status", get(git_status))
        .route("/devaipod/pods/{name}/git/diff", get(git_diff))
        .route("/devaipod/pods/{name}/git/commits", get(git_commits))
        .route("/devaipod/pods/{name}/git/log", get(git_log))
        .route("/devaipod/pods/{name}/git/diff-range", get(git_diff_range))
        .route("/devaipod/pods/{name}/git/fetch-agent", post(git_fetch_agent))
        .route("/devaipod/pods/{name}/git/push", post(git_push))
        .route(
            "/devaipod/pods/{name}/pod-api/{*path}",
            get(pod_api_proxy)
                .post(pod_api_proxy)
                .put(pod_api_proxy)
                .delete(pod_api_proxy),
        )
        .route("/devaipod/run", post(run_workspace))
        .route("/devaipod/launches", get(list_launches))
        .route(
            "/devaipod/launches/{pod_name}",
            axum::routing::delete(dismiss_launch),
        )
        .route("/devaipod/advisor/launch", post(launch_advisor))
        .route("/devaipod/advisor/status", get(advisor_status))
        .route("/devaipod/proposals", get(list_proposals_api))
        .route("/devaipod/proposals/{id}/dismiss", post(dismiss_proposal))
        .route("/devaipod/pods/{name}/recreate", post(recreate_workspace))
        // PTY: proxy to the pod-api sidecar (direct PTY, no exec overhead)
        .route(
            "/devaipod/pods/{name}/pty",
            get(pty_pod_api_proxy_root).post(pty_pod_api_proxy_root),
        )
        .route(
            "/devaipod/pods/{name}/pty/{*rest}",
            get(pty_pod_api_proxy)
                .put(pty_pod_api_proxy)
                .delete(pty_pod_api_proxy),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .with_state(state.clone());

    // CORS: allow webviews and cross-origin requests (e.g. IDE embedding control plane at 127.0.0.1:8080)
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/_devaipod/health", get(|| async { "ok" }))
        .route("/_devaipod/login", get(login))
        .route("/_devaipod/frontend-error", post(frontend_error_report))
        // MCP endpoint for advisor tools — outside auth since the advisor
        // pod connects without a bearer token. Only exposes pod metadata
        // and a local draft-proposals JSON file.
        // TODO: add lightweight auth (e.g. shared secret) for production use
        .route("/api/devaipod/mcp", post(crate::mcp::handle_mcp))
        .nest("/api", api_router)
        .route("/", get(redirect_to_pods))
        .route("/pods", get(serve_spa_page))
        .route("/_devaipod/oldui", get(serve_old_ui))
        .route("/_devaipod/opencode-ui", get(serve_opencode_raw_ui))
        .route("/_devaipod/agent/{name}", get(agent_wrapper))
        .route("/_devaipod/agent/{name}/", get(agent_ui_root))
        .route(
            "/_devaipod/agent/{name}/{*path}",
            get(agent_ui_handler)
                .post(agent_ui_handler)
                .put(agent_ui_handler)
                .delete(agent_ui_handler)
                .patch(agent_ui_handler),
        )
        // /assets/*: always serve from vendored opencode UI (control-plane UI uses inline styles)
        .route("/assets", get(serve_root_assets))
        .route("/assets/{*path}", get(serve_root_assets))
        // Catch-all fallback: if the agent cookie is set, proxy to the opencode backend;
        // otherwise serve the SPA for client-side routing. This avoids maintaining a
        // hardcoded list of opencode API routes (the SPA uses window.location.origin
        // for all its API calls: /session, /global/event, /agent, /config, etc.).
        .fallback(opencode_or_static_fallback)
        .layer(middleware::from_fn(request_trace))
        .layer(cors)
        .with_state(state)
}

pub async fn run_web_server(port: u16, token: String) -> Result<()> {
    // Try to get the podman socket path, but don't fail if not found
    // (allows server to start for static file serving even without podman)
    let socket_path = get_container_socket().ok();

    if socket_path.is_none() {
        tracing::warn!(
            "No podman socket found. Podman API proxy will return 503 until socket is available."
        );
    }

    // Find static files directory
    // Try installed location first, then fall back to development location
    let static_dir = if std::path::Path::new("/usr/share/devaipod/dist").exists() {
        "/usr/share/devaipod/dist"
    } else if std::path::Path::new("dist").exists() {
        "dist"
    } else {
        bail!(
            "No static files directory found. Expected /usr/share/devaipod/dist (installed) or ./dist (development).\n\
             The dist/ directory contains the web UI frontend files."
        );
    };
    tracing::info!("Serving static files from: {}", static_dir);

    let app = build_app(token.clone(), socket_path, static_dir);

    // Bind to [::] which accepts both IPv4 and IPv6 connections via dual-stack
    // (the Linux default).  Browsers typically try IPv6 first when resolving
    // "localhost", so binding IPv4-only would cause connection resets.
    let addr = format!("[::]:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind to {}", addr))?;

    // Print startup message with URL including token
    let url = format!("http://127.0.0.1:{}/_devaipod/login?token={}", port, token);
    tracing::info!("Web server started at {}", url);
    println!("Control plane URL: {}", url);

    // Run the server
    axum::serve(listener, app)
        .await
        .context("Web server error")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::Request;
    use tower::util::ServiceExt;

    /// Fast in-process test: GET /_devaipod/agent/{name} must return 307 redirect
    /// to /_devaipod/agent/{name}/ and set the DEVAIPOD_AGENT_POD cookie.
    #[tokio::test]
    async fn test_agent_redirect() {
        let temp = tempfile::tempdir().expect("temp dir");
        let static_dir = temp.path().to_str().expect("path");
        let app = build_app("test-token".into(), None, static_dir);

        let req = Request::builder()
            .uri("/_devaipod/agent/test-pod")
            .body(Body::empty())
            .expect("request");
        let res = app.oneshot(req).await.expect("oneshot");
        let status = res.status();
        let headers = res.headers().clone();
        let _body = to_bytes(res.into_body(), usize::MAX).await.expect("body");

        assert_eq!(
            status.as_u16(),
            307,
            "GET /_devaipod/agent/test-pod must return 307 redirect; got {}",
            status
        );
        let location = headers
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .expect("Location header must be set");
        assert_eq!(
            location, "/_devaipod/agent/test-pod/",
            "Location must redirect to trailing-slash path"
        );

        let set_cookie = headers
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .expect("Set-Cookie header must be set");
        assert!(
            set_cookie.contains("DEVAIPOD_AGENT_POD="),
            "Set-Cookie must include DEVAIPOD_AGENT_POD; got: {}",
            set_cookie
        );
        assert!(
            set_cookie.contains("test-pod"),
            "Set-Cookie must include pod name; got: {}",
            set_cookie
        );
    }

    /// Test the iframe wrapper HTML generation.
    #[test]
    fn test_agent_iframe_wrapper() {
        let html = agent_iframe_wrapper("test-pod");
        assert!(html.contains("test-pod"), "wrapper must include pod name");
        assert!(
            html.contains("/_devaipod/opencode-ui"),
            "wrapper must contain iframe src to opencode-ui"
        );
        assert!(html.contains("Pods"), "wrapper must have back-to-pods link");
        assert!(
            html.contains("devaipod_token"),
            "wrapper must read token from sessionStorage"
        );

        // HTML-escaping
        let html = agent_iframe_wrapper("<script>alert(1)</script>");
        assert!(
            !html.contains("<script>alert"),
            "pod name must be HTML-escaped"
        );
        assert!(
            html.contains("&lt;script&gt;"),
            "angle brackets must be escaped"
        );
    }

    #[test]
    fn test_generate_token() {
        let token1 = generate_token();
        let token2 = generate_token();

        // Tokens should be different
        assert_ne!(token1, token2);

        // Tokens should be 43 characters (32 bytes base64url encoded without padding)
        assert_eq!(token1.len(), 43);
        assert_eq!(token2.len(), 43);

        // Tokens should be valid base64url
        assert!(BASE64_URL_SAFE_NO_PAD.decode(&token1).is_ok());
        assert!(BASE64_URL_SAFE_NO_PAD.decode(&token2).is_ok());
    }

    #[test]
    fn test_load_or_generate_token_generates_when_no_file() {
        // When the secrets file doesn't exist, a token should be generated
        let token = load_or_generate_token();
        assert_eq!(token.len(), 43);
    }

    #[test]
    fn test_parse_git_status_line() {
        // Test parsing a modified file
        let line = " M src/main.rs";
        let status = line.chars().take(2).collect::<String>().trim().to_string();
        let path = line.chars().skip(3).collect::<String>();
        assert_eq!(status, "M");
        assert_eq!(path, "src/main.rs");

        // Test parsing an untracked file
        let line = "?? new_file.txt";
        let status = line.chars().take(2).collect::<String>().trim().to_string();
        let path = line.chars().skip(3).collect::<String>();
        assert_eq!(status, "??");
        assert_eq!(path, "new_file.txt");
    }

    #[test]
    fn test_parse_git_log_line() {
        let line = "abc1234 Fix the bug in parser";
        let mut parts = line.splitn(2, ' ');
        let hash = parts.next().unwrap().to_string();
        let message = parts.next().unwrap_or("").to_string();
        assert_eq!(hash, "abc1234");
        assert_eq!(message, "Fix the bug in parser");
    }

    #[test]
    fn test_agent_ui_api_path_detection() {
        // Test the logic used in agent_ui_handler to determine API vs static paths (OPENCODE_API_SEGMENTS)
        let is_api = |path: &str| {
            OPENCODE_API_SEGMENTS
                .iter()
                .any(|p| path == *p || path.starts_with(&format!("{}/", p)))
        };

        assert!(is_api("rpc"));
        assert!(is_api("event"));
        assert!(is_api("session"));
        assert!(is_api("global"));
        assert!(is_api("path"));
        assert!(is_api("project"));
        assert!(is_api("provider"));
        assert!(is_api("auth"));
        assert!(is_api("rpc/some/path"));

        // Static paths should not be detected as API
        assert!(!is_api(""));
        assert!(!is_api("index.html"));
        assert!(!is_api("assets/main.js"));
        assert!(!is_api("favicon.ico"));
        assert!(!is_api("some/other/path"));
    }

    /// Verify proxy_to_upstream upgrades HTTP/1.0 responses to HTTP/1.1.
    ///
    /// Browsers require HTTP/1.1 for SSE (chunked transfer encoding). This test
    /// starts a mock HTTP/1.0 server, proxies through proxy_to_upstream, and
    /// asserts the response version is upgraded.
    #[tokio::test]
    async fn test_proxy_upgrades_http10_to_http11() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().unwrap().port();

        // Mock server: accept one connection, read the request, reply with HTTP/1.0
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let mut buf = vec![0u8; 4096];
            let n = sock.read(&mut buf).await.expect("read");
            assert!(n > 0, "should receive request bytes");

            let response = b"HTTP/1.0 200 OK\r\n\
                content-type: text/event-stream\r\n\
                cache-control: no-cache\r\n\
                connection: keep-alive\r\n\
                \r\n\
                : mock-keepalive\n\n";
            sock.write_all(response).await.expect("write");
            drop(sock);
        });

        let request = Request::builder()
            .uri("/global/event")
            .body(Body::empty())
            .expect("request");

        let response = proxy_to_upstream("127.0.0.1", port, "global/event".into(), request)
            .await
            .expect("proxy_to_upstream should succeed");

        assert_eq!(
            response.version(),
            hyper::Version::HTTP_11,
            "proxy must upgrade HTTP/1.0 response to HTTP/1.1"
        );
        assert_eq!(response.status(), StatusCode::OK);

        let ct = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ct.contains("text/event-stream"),
            "Content-Type must be preserved; got: {ct}"
        );
    }

    /// Verify proxy_to_upstream also upgrades non-SSE HTTP/1.0 responses.
    #[tokio::test]
    async fn test_proxy_upgrades_http10_json_response() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await.expect("read");
            let body = r#"{"ok":true}"#;
            let response = format!(
                "HTTP/1.0 200 OK\r\n\
                 content-type: application/json\r\n\
                 content-length: {}\r\n\
                 \r\n\
                 {}",
                body.len(),
                body
            );
            sock.write_all(response.as_bytes()).await.expect("write");
            drop(sock);
        });

        let request = Request::builder()
            .uri("/session")
            .body(Body::empty())
            .expect("request");

        let response = proxy_to_upstream("127.0.0.1", port, "session".into(), request)
            .await
            .expect("proxy should succeed");

        assert_eq!(
            response.version(),
            hyper::Version::HTTP_11,
            "JSON response must also be upgraded to HTTP/1.1"
        );
        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), 1024)
            .await
            .expect("read body");
        assert!(
            String::from_utf8_lossy(&body).contains("ok"),
            "body must be forwarded"
        );
    }

    /// Verify proxy_to_upstream preserves query string (required for /file?path=..., /find/file?path=..., etc.).
    #[tokio::test]
    async fn test_proxy_preserves_query_string() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::sync::oneshot;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = oneshot::channel::<String>();

        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let mut buf = vec![0u8; 4096];
            let n = sock.read(&mut buf).await.expect("read");
            let request = String::from_utf8_lossy(&buf[..n]);
            let first_line = request.lines().next().unwrap_or("").to_string();
            let _ = tx.send(first_line);
            let response = "HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n";
            sock.write_all(response.as_bytes()).await.expect("write");
            drop(sock);
        });

        let request = Request::builder()
            .uri("/file?path=src/main.rs")
            .body(Body::empty())
            .expect("request");

        let _ = proxy_to_upstream("127.0.0.1", port, "file".into(), request)
            .await
            .expect("proxy should succeed");

        let request_line = rx.await.expect("mock should send request line");
        assert!(
            request_line.contains("path="),
            "proxy must forward query string; request line: {}",
            request_line
        );
        assert!(
            request_line.contains("src"),
            "proxy must preserve path param value; request line: {}",
            request_line
        );
    }

    /// Document and assert the URL rewrite patterns used for agent UI (HTML and CSS).
    /// If you add or change patterns, update docs/audit-agent-ui-rewriting.md and this test.
    #[test]
    fn test_agent_ui_rewrite_patterns() {
        let base = "/agent/example-pod/";

        // HTML patterns (index.html)
        let html = r#"<script src="/assets/x.js"></script><link href="/assets/y.css">"#;
        let rewritten_html = html
            .replace(" src=\"/", &format!(" src=\"{base}"))
            .replace(" href=\"/", &format!(" href=\"{base}"));
        assert!(
            rewritten_html.contains(base),
            "HTML src/href must be rewritten"
        );
        assert!(
            !rewritten_html.contains("src=\"/assets/"),
            "HTML must not leave bare src=\"/"
        );

        // CSS patterns (fonts and assets)
        let css = r#"url("/assets/font.woff2") url('/x.woff2') url(/unquoted.woff2) url( "/spaced.woff2")"#;
        let rewritten_css = css
            .replace("url(\"/", &format!("url(\"{base}"))
            .replace("url('/", &format!("url('{base}"))
            .replace("url( \"/", &format!("url( \"{base}"))
            .replace("url( '/", &format!("url( '{base}"))
            .replace("url(/", &format!("url({base}"));
        assert!(rewritten_css.contains(base), "CSS url() must be rewritten");
        assert!(
            !rewritten_css.contains("url(\"/assets/"),
            "CSS must not leave bare url(\"/"
        );
    }
}
