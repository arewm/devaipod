//! Web server for devaipod control plane
//!
//! This module provides:
//! - Token-based authentication for API access
//! - Podman socket proxy at `/api/podman/*`
//! - Git status endpoints for workspace containers
//! - Static file serving for web UI

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
    Json, Router,
};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::prelude::*;
use color_eyre::eyre::{bail, Context, Result};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use tokio::net::UnixStream;
use tower_http::services::ServeDir;

use crate::podman::get_container_socket;

/// Path to the token file in /run/secrets
const TOKEN_SECRET_PATH: &str = "/run/secrets/devaipod-web-token";

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

/// Load token from secrets file or generate a new one
///
/// Checks `/run/secrets/devaipod-web-token` first (for Kubernetes/container secrets).
/// If not found, generates a new random token.
pub fn load_or_generate_token() -> String {
    // Try to read from secrets file first
    if let Ok(token) = std::fs::read_to_string(TOKEN_SECRET_PATH) {
        let trimmed = token.trim();
        if !trimmed.is_empty() {
            tracing::debug!("Loaded token from {}", TOKEN_SECRET_PATH);
            return trimmed.to_string();
        }
    }

    // Generate a new token
    let token = generate_token();
    tracing::debug!("Generated new authentication token");
    token
}

/// Shared state for the web server
#[derive(Clone)]
struct AppState {
    /// Authentication token for API access
    token: String,
    /// Path to the podman/docker socket (None if not available at startup)
    socket_path: Option<PathBuf>,
}

/// Query parameters for token authentication
#[derive(Debug, Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

/// Authentication middleware
///
/// Validates requests by checking:
/// 1. `?token=...` query parameter
/// 2. `Authorization: Bearer ...` header
///
/// Returns 401 Unauthorized if neither is present or valid.
async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    Query(query): Query<TokenQuery>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    // Check query parameter first
    if let Some(ref token) = query.token {
        if token == &state.token {
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
/// Returns (published_port, api_password) for the pod's auth proxy.
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

    // Get the published port for 4097 (auth proxy)
    let ports = pod_info
        .network_settings
        .as_ref()
        .and_then(|ns| ns.ports.as_ref())
        .ok_or_else(|| {
            tracing::error!("No port mappings found for {}", pod_name);
            StatusCode::NOT_FOUND
        })?;

    let port_key = "4097/tcp";
    let bindings = ports
        .get(port_key)
        .and_then(|b| b.as_ref())
        .ok_or_else(|| {
            tracing::error!("Port 4097 not published for {}", pod_name);
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
}

/// Path to vendored opencode web UI
const OPENCODE_UI_PATH: &str = "/usr/share/devaipod/opencode";

/// Get opencode connection info for a pod
///
/// Returns the direct URL to access the opencode web UI.
/// The URL includes a token query parameter for authentication.
async fn opencode_info(Path(name): Path<String>) -> Result<Json<OpencodeInfoResponse>, StatusCode> {
    // Normalize pod name (add devaipod- prefix if not present)
    let pod_name = if name.starts_with("devaipod-") {
        name.clone()
    } else {
        format!("devaipod-{}", name)
    };

    let (port, password) = get_pod_opencode_info(&pod_name).await?;

    // Build URL with token query parameter
    // The auth proxy accepts ?token=PASSWORD and sets a session cookie
    // This allows browsers to load dynamic ES modules (which can't use Basic Auth)
    let url = format!("http://127.0.0.1:{}/?token={}", port, password);

    Ok(Json(OpencodeInfoResponse {
        url,
        port,
        accessible: true,
    }))
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
/// Forwards requests to a pod's opencode server via its auth proxy.
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
    // Normalize pod name (add devaipod- prefix if not present)
    let pod_name = if name.starts_with("devaipod-") {
        name.clone()
    } else {
        format!("devaipod-{}", name)
    };

    // Get the pod's opencode connection info
    let (port, password) = get_pod_opencode_info(&pod_name).await?;

    tracing::debug!(
        "Proxying to opencode for pod {} on port {}, path: {}",
        pod_name,
        port,
        path
    );

    // Connect to localhost:port (the published auth proxy port)
    let stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{}", port))
        .await
        .map_err(|e| {
            tracing::error!("Failed to connect to opencode auth proxy: {}", e);
            StatusCode::BAD_GATEWAY
        })?;

    let io = TokioIo::new(stream);

    // Create HTTP client
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(|e| {
            tracing::error!("Handshake with opencode server failed: {}", e);
            StatusCode::BAD_GATEWAY
        })?;

    // Spawn connection handler
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            tracing::error!("Connection to opencode server failed: {}", e);
        }
    });

    // Build the request
    let (parts, body) = request.into_parts();

    // Use root path if path is empty, otherwise prepend /
    let uri = if path.is_empty() || path == "/" {
        "/".to_string()
    } else if path.starts_with('/') {
        path
    } else {
        format!("/{}", path)
    };

    // Build Basic Auth header
    let auth = BASE64_STANDARD.encode(format!("opencode:{}", password));

    let mut builder = hyper::Request::builder()
        .method(parts.method)
        .uri(&uri)
        .header(header::HOST, format!("127.0.0.1:{}", port))
        .header(header::AUTHORIZATION, format!("Basic {}", auth));

    // Copy headers (except Host and Authorization which we set)
    for (key, value) in parts.headers.iter() {
        if key != header::HOST && key != header::AUTHORIZATION {
            builder = builder.header(key, value);
        }
    }

    let proxy_request = builder.body(body).map_err(|e| {
        tracing::error!("Failed to build opencode proxy request: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Send request and get response
    let response = sender.send_request(proxy_request).await.map_err(|e| {
        tracing::error!("Failed to send request to opencode: {}", e);
        StatusCode::BAD_GATEWAY
    })?;

    // Convert the response
    let (parts, body) = response.into_parts();
    let body = Body::new(body);

    Ok(Response::from_parts(parts, body))
}

/// Handler for agent UI routes
///
/// Serves the vendored opencode web UI for workspace agents.
/// Routes:
/// - Static assets (index.html, assets/*, *.js, favicon.*) -> serve from /usr/share/devaipod/opencode/
/// - API paths (rpc, event, session) -> proxy to workspace's opencode server
async fn agent_ui_handler(
    Path((name, path)): Path<(String, String)>,
    request: Request,
) -> Result<Response, StatusCode> {
    // Determine if this is an API path that should be proxied
    let api_paths = ["rpc", "event", "session"];
    let is_api = api_paths
        .iter()
        .any(|p| path == *p || path.starts_with(&format!("{}/", p)));

    if is_api {
        // Proxy to workspace's opencode server
        opencode_proxy_impl(name, path, request).await
    } else {
        // Serve static file from vendored opencode UI
        serve_opencode_static(&name, &path).await
    }
}

/// Handler for agent UI root path (no trailing path)
async fn agent_ui_root(Path(name): Path<String>) -> Result<Response, StatusCode> {
    serve_opencode_static(&name, "").await
}

/// Serve a static file from the vendored opencode UI directory
async fn serve_opencode_static(name: &str, path: &str) -> Result<Response, StatusCode> {
    use tokio::fs;

    let ui_path = std::path::Path::new(OPENCODE_UI_PATH);

    // Check if the UI directory exists
    if !ui_path.exists() {
        tracing::error!(
            "Opencode UI not found at {}. Install the devaipod-opencode package.",
            OPENCODE_UI_PATH
        );
        return Err(StatusCode::NOT_FOUND);
    }

    // Determine the file to serve
    // Empty path or paths without extension -> index.html
    // Otherwise, serve the requested file
    let file_path = if path.is_empty() || path == "/" {
        ui_path.join("index.html")
    } else {
        // Clean the path to prevent directory traversal
        let clean_path = path.trim_start_matches('/');
        ui_path.join(clean_path)
    };

    // Verify the resolved path is within the UI directory
    let ui_canonical = ui_path
        .canonicalize()
        .unwrap_or_else(|_| ui_path.to_path_buf());
    // For files that exist, verify they're within bounds
    if file_path.exists() {
        if let Ok(resolved) = file_path.canonicalize() {
            if !resolved.starts_with(&ui_canonical) {
                return Err(StatusCode::BAD_REQUEST);
            }
        }
    }

    // Try to read the file
    let content = match fs::read(&file_path).await {
        Ok(content) => content,
        Err(e) => {
            // For SPA routing: if file not found and doesn't look like a static asset,
            // serve index.html so client-side routing can handle it
            if e.kind() == std::io::ErrorKind::NotFound {
                let path_str = path.trim_start_matches('/');
                let has_extension = path_str.contains('.') && !path_str.ends_with('/');

                if !has_extension {
                    // Try serving index.html for SPA routes
                    match fs::read(ui_path.join("index.html")).await {
                        Ok(content) => content,
                        Err(_) => {
                            tracing::error!(
                                "index.html not found for agent {} at {:?}",
                                name,
                                ui_path.join("index.html")
                            );
                            return Err(StatusCode::NOT_FOUND);
                        }
                    }
                } else {
                    tracing::debug!("Static file not found: {:?}", file_path);
                    return Err(StatusCode::NOT_FOUND);
                }
            } else {
                tracing::error!("Failed to read file {:?}: {}", file_path, e);
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
        }
    };

    // Determine content type from file extension
    let content_type = match file_path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        _ => "application/octet-stream",
    };

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "public, max-age=3600")
        .body(Body::from(content))
        .unwrap())
}

/// Request body for run endpoint
#[derive(Debug, Deserialize)]
struct RunRequest {
    /// Source: git URL, local path, or issue/PR URL (optional, defaults to dotfiles)
    source: Option<String>,
    /// Task description for the AI agent
    task: Option<String>,
    /// Explicit pod name (optional, auto-generated if not provided)
    name: Option<String>,
    /// Attach to agent after starting (ignored for web API, included for parity)
    #[serde(default)]
    #[allow(dead_code)]
    attach: bool,
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
}

/// Run a new devaipod workspace
///
/// Shells out to `devaipod run` with the provided arguments.
/// Returns the workspace name on success.
async fn run_workspace(Json(req): Json<RunRequest>) -> Result<Json<RunResponse>, StatusCode> {
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

    // Add explicit name if provided
    if let Some(ref name) = req.name {
        cmd.args(["--name", name]);
    }

    tracing::info!("Running devaipod: {:?}", cmd);

    let output = cmd.output().await.map_err(|e| {
        tracing::error!("Failed to execute devaipod run: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::error!("devaipod run failed: {}", stderr);
        return Ok(Json(RunResponse {
            success: false,
            workspace: String::new(),
            message: format!("Failed to create workspace: {}", stderr.trim()),
        }));
    }

    // Parse the output to extract the workspace name
    // The output typically contains lines like:
    // "Created pod 'devaipod-repo-abc123'"
    // or
    // "devaipod-repo-abc123"
    let stdout = String::from_utf8_lossy(&output.stdout);
    let workspace = extract_workspace_name(&stdout).unwrap_or_else(|| {
        // If we can't parse it, use the explicit name if provided
        req.name.clone().unwrap_or_default()
    });

    Ok(Json(RunResponse {
        success: true,
        workspace: workspace.clone(),
        message: format!("Workspace '{}' created successfully", workspace),
    }))
}

/// Extract workspace name from devaipod run output
///
/// Looks for the short workspace name (without devaipod- prefix) in the output.
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

/// Get git status for a workspace pod
///
/// Runs `git status --porcelain` in the workspace container and returns
/// parsed results as JSON.
async fn git_status(Path(name): Path<String>) -> Result<Json<GitStatusResponse>, StatusCode> {
    let container_name = format!("{}-workspace", name);

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
    let container_name = format!("{}-workspace", name);

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
    let container_name = format!("{}-workspace", name);

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
pub async fn run_web_server(port: u16, token: String) -> Result<()> {
    // Try to get the podman socket path, but don't fail if not found
    // (allows server to start for static file serving even without podman)
    let socket_path = get_container_socket().ok();

    if socket_path.is_none() {
        tracing::warn!(
            "No podman socket found. Podman API proxy will return 503 until socket is available."
        );
    }

    let state = Arc::new(AppState {
        token: token.clone(),
        socket_path,
    });

    // Build the API router with authentication
    let api_router = Router::new()
        // Podman socket proxy - capture the rest of the path
        // axum 0.8 uses {*path} syntax for wildcard captures
        .route(
            "/podman/{*path}",
            get(podman_proxy)
                .post(podman_proxy)
                .put(podman_proxy)
                .delete(podman_proxy),
        )
        // OpenCode proxy - forwards to pod's opencode server (which serves web UI from app.opencode.ai)
        // Two routes: one for root path, one for sub-paths
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
        // OpenCode info endpoint - returns direct URL to access opencode
        .route("/devaipod/pods/{name}/opencode-info", get(opencode_info))
        // Git endpoints for workspace pods
        .route("/devaipod/pods/{name}/git/status", get(git_status))
        .route("/devaipod/pods/{name}/git/diff", get(git_diff))
        .route("/devaipod/pods/{name}/git/commits", get(git_commits))
        // Run endpoint - create a new workspace
        .route("/devaipod/run", post(run_workspace))
        // Apply auth middleware to all API routes
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .with_state(state.clone());

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

    // Build the main router
    // Static files are served without authentication (fallback)
    let app = Router::new()
        // Health check endpoint (no auth required)
        .route("/health", get(|| async { "ok" }))
        .nest("/api", api_router)
        // Agent UI routes - serve vendored opencode web UI and proxy API calls
        // These routes are NOT authenticated (the opencode server handles auth via cookies)
        .route("/agent/{name}", get(agent_ui_root))
        .route("/agent/{name}/", get(agent_ui_root))
        .route(
            "/agent/{name}/{*path}",
            get(agent_ui_handler)
                .post(agent_ui_handler)
                .put(agent_ui_handler)
                .delete(agent_ui_handler)
                .patch(agent_ui_handler),
        )
        // Serve static files from dist/ directory as fallback
        .fallback_service(ServeDir::new(static_dir));

    // Bind to localhost only
    let addr = format!("127.0.0.1:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind to {}", addr))?;

    // Print startup message with URL including token
    let url = format!("http://127.0.0.1:{}/?token={}", port, token);
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
        // Test the logic used in agent_ui_handler to determine API vs static paths
        let api_paths = ["rpc", "event", "session"];

        // API paths should be detected
        let is_api = |path: &str| {
            api_paths
                .iter()
                .any(|p| path == *p || path.starts_with(&format!("{}/", p)))
        };

        assert!(is_api("rpc"));
        assert!(is_api("event"));
        assert!(is_api("session"));
        assert!(is_api("rpc/some/path"));

        // Static paths should not be detected as API
        assert!(!is_api(""));
        assert!(!is_api("index.html"));
        assert!(!is_api("assets/main.js"));
        assert!(!is_api("favicon.ico"));
        assert!(!is_api("some/other/path"));
    }
}
