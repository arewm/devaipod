//! Web terminal (PTY) sessions for workspace containers
//!
//! Provides PTY terminal access to workspace containers via a web API that
//! mimics opencode's `/pty/*` API shape. Each session runs a command (default
//! `/bin/bash`) inside a workspace container using `bollard`'s exec API with
//! TTY mode enabled.
//!
//! Sessions support multiple concurrent WebSocket clients. Output is buffered
//! in a ring buffer (up to 2 MB) so new clients can replay history.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use bollard::exec::{CreateExecOptions, ResizeExecOptions, StartExecResults};
use bollard::Docker;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::sync::{broadcast, RwLock};

use crate::podman::get_container_socket;
use crate::web::AppState;

/// Maximum size of the output ring buffer per session (2 MB).
const MAX_RING_BUFFER_BYTES: usize = 2 * 1024 * 1024;

/// Capacity of the broadcast channel per session.
const BROADCAST_CAPACITY: usize = 256;

/// How long to keep exited sessions before cleanup (5 minutes).
const EXITED_SESSION_TTL: std::time::Duration = std::time::Duration::from_secs(5 * 60);

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

/// PTY session info returned by most endpoints.
#[derive(Debug, Serialize, Clone)]
pub struct PtyInfo {
    /// Unique session ID (e.g. `"pty_abc123"`).
    pub id: String,
    /// Human-readable title.
    pub title: String,
    /// Command that was executed.
    pub command: String,
    /// Arguments passed to the command.
    pub args: Vec<String>,
    /// Working directory inside the container.
    pub cwd: String,
    /// `"running"` or `"exited"`.
    pub status: String,
    /// PID of the exec process (not available via bollard, always `None`).
    pub pid: Option<u64>,
}

/// Request body for `POST /pty`.
#[derive(Debug, Deserialize)]
pub struct PtyCreateInput {
    /// Command to run (default: `/bin/bash`).
    pub command: Option<String>,
    /// Arguments for the command.
    pub args: Option<Vec<String>>,
    /// Working directory inside the container.
    pub cwd: Option<String>,
    /// Human-readable title for the session.
    pub title: Option<String>,
    /// Additional environment variables.
    pub env: Option<HashMap<String, String>>,
}

/// Request body for `PUT /pty/:id`.
#[derive(Debug, Deserialize)]
pub struct PtyUpdateInput {
    /// New title for the session.
    pub title: Option<String>,
    /// Resize the terminal.
    pub size: Option<PtySize>,
}

/// Terminal dimensions.
#[derive(Debug, Deserialize)]
pub struct PtySize {
    pub rows: u16,
    pub cols: u16,
}

/// Query parameters for the WebSocket connect endpoint.
#[derive(Debug, Deserialize)]
struct ConnectQuery {
    /// Byte offset to replay from. Defaults to 0 (full replay).
    cursor: Option<u64>,
}

// ---------------------------------------------------------------------------
// Session internals
// ---------------------------------------------------------------------------

/// Mutable output state for a single PTY session, behind its own lock so that
/// the output reader task does not require a write lock on the global sessions map.
struct SessionOutput {
    /// Ring buffer of output bytes (VecDeque for O(1) front drain).
    ring_buffer: VecDeque<u8>,
    /// Total bytes written since session start (monotonically increasing cursor).
    cursor: u64,
    /// Session status: "running" or "exited".
    status: String,
    /// When the session exited (for cleanup TTL). `None` while running.
    exited_at: Option<std::time::Instant>,
}

/// Internal state for a single PTY session.
struct PtySession {
    /// Metadata returned via the REST API (title, command, args, cwd; status is
    /// derived from `output` at read time).
    info: PtyInfo,
    /// The bollard exec ID.
    exec_id: String,
    /// Container name this session runs in.
    container: String,
    /// Per-session mutable output state (ring buffer, cursor, status).
    output: Arc<tokio::sync::Mutex<SessionOutput>>,
    /// Broadcast channel sender for streaming output to WebSocket clients.
    output_tx: broadcast::Sender<(Vec<u8>, u64)>,
    /// Sender half of the channel used to forward WebSocket input to the exec stdin.
    /// `None` once the exec process has exited or stdin is closed.
    stdin_tx: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
}

impl PtySession {
    /// Return a snapshot of the session info with the current status from the
    /// output lock. Caller must already hold the sessions read lock but NOT the
    /// output lock (this method acquires it).
    async fn info(&self) -> PtyInfo {
        let output = self.output.lock().await;
        let mut info = self.info.clone();
        info.status = output.status.clone();
        info
    }
}

// ---------------------------------------------------------------------------
// Session manager
// ---------------------------------------------------------------------------

/// Manages PTY sessions across all pods.
///
/// Internally wraps an `Arc` so it is cheap to clone.
#[derive(Clone)]
pub struct PtySessionManager {
    sessions: Arc<RwLock<HashMap<String, PtySession>>>,
}

impl PtySessionManager {
    /// Create a new, empty session manager.
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

/// Resolve the workspace container name for a pod and verify a session belongs to it.
fn workspace_container(name: &str) -> String {
    format!("devaipod-{name}-workspace")
}

/// Generate a short random session ID.
fn generate_session_id() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let suffix: u64 = rng.random::<u64>() & 0xFFFF_FFFF_FFFF;
    format!("pty_{suffix:x}")
}

/// Connect to the podman/docker socket via bollard.
fn connect_docker() -> Result<Docker, StatusCode> {
    let socket_path = get_container_socket().map_err(|e| {
        tracing::error!("No container socket: {}", e);
        StatusCode::SERVICE_UNAVAILABLE
    })?;
    Docker::connect_with_unix(
        &format!("unix://{}", socket_path.display()),
        120,
        bollard::API_DEFAULT_VERSION,
    )
    .map_err(|e| {
        tracing::error!("Failed to connect to container socket: {}", e);
        StatusCode::SERVICE_UNAVAILABLE
    })
}

/// Remove sessions that have been in "exited" state for longer than `EXITED_SESSION_TTL`.
/// Called opportunistically during session creation to bound growth without a background timer.
async fn cleanup_stale_sessions(sessions: &mut HashMap<String, PtySession>) {
    let now = std::time::Instant::now();
    let mut to_remove = Vec::new();
    for (id, session) in sessions.iter() {
        let output = session.output.lock().await;
        if let Some(exited_at) = output.exited_at {
            if now.duration_since(exited_at) > EXITED_SESSION_TTL {
                to_remove.push(id.clone());
            }
        }
    }
    for id in &to_remove {
        sessions.remove(id);
    }
    if !to_remove.is_empty() {
        tracing::info!("Cleaned up {} stale exited PTY sessions", to_remove.len());
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /pty` -- List sessions for this pod's workspace container.
async fn pty_list(
    Path(name): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<PtyInfo>>, StatusCode> {
    let container = workspace_container(&name);
    let sessions = state.pty_sessions.sessions.read().await;
    let mut infos = Vec::new();
    for s in sessions.values() {
        if s.container == container {
            infos.push(s.info().await);
        }
    }
    Ok(Json(infos))
}

/// `POST /pty` -- Create a new PTY session.
async fn pty_create(
    Path(name): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(input): Json<PtyCreateInput>,
) -> Result<(StatusCode, Json<PtyInfo>), StatusCode> {
    let container = workspace_container(&name);
    let docker = connect_docker()?;

    let command = input.command.unwrap_or_else(|| "/bin/bash".to_string());
    let args = input.args.unwrap_or_default();
    let cwd = input.cwd.unwrap_or_else(|| "/".to_string());
    let title = input
        .title
        .unwrap_or_else(|| format!("{} {}", command, args.join(" ")).trim().to_string());

    // Build the full command line for exec.
    let mut cmd_vec: Vec<String> = vec![command.clone()];
    cmd_vec.extend(args.clone());
    let cmd_refs: Vec<&str> = cmd_vec.iter().map(String::as_str).collect();

    // Build environment list with sensible defaults for terminal use.
    let mut env_vars = vec![
        "TERM=xterm-256color".to_string(),
        "COLORTERM=truecolor".to_string(),
    ];
    if let Some(extra) = input.env {
        // User-provided env vars override defaults.
        env_vars.extend(extra.into_iter().map(|(k, v)| format!("{k}={v}")));
    }
    let env_list = env_vars;

    let exec = docker
        .create_exec(
            &container,
            CreateExecOptions {
                cmd: Some(cmd_refs),
                attach_stdin: Some(true),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                tty: Some(true),
                working_dir: Some(cwd.as_str()),
                env: Some(env_list.iter().map(String::as_str).collect()),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| {
            tracing::error!("Failed to create exec in {}: {}", container, e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let exec_id = exec.id.clone();

    let start_result = docker
        .start_exec(
            &exec_id,
            Some(bollard::exec::StartExecOptions {
                detach: false,
                ..Default::default()
            }),
        )
        .await
        .map_err(|e| {
            tracing::error!("Failed to start exec {}: {}", exec_id, e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let session_id = generate_session_id();
    let (output_tx, _) = broadcast::channel::<(Vec<u8>, u64)>(BROADCAST_CAPACITY);
    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);

    let info = PtyInfo {
        id: session_id.clone(),
        title,
        command: command.clone(),
        args: args.clone(),
        cwd,
        status: "running".to_string(),
        pid: None,
    };

    let session_output = Arc::new(tokio::sync::Mutex::new(SessionOutput {
        ring_buffer: VecDeque::new(),
        cursor: 0,
        status: "running".to_string(),
        exited_at: None,
    }));

    let session = PtySession {
        info: info.clone(),
        exec_id: exec_id.clone(),
        container: container.clone(),
        output: session_output.clone(),
        output_tx: output_tx.clone(),
        stdin_tx: Some(stdin_tx),
    };

    {
        let mut sessions = state.pty_sessions.sessions.write().await;
        // Clean up stale exited sessions to prevent unbounded growth.
        cleanup_stale_sessions(&mut sessions).await;
        sessions.insert(session_id.clone(), session);
    }

    // Spawn background task to read exec output and bridge stdin.
    match start_result {
        StartExecResults::Attached { mut output, mut input } => {
            let tx = output_tx.clone();
            let so = session_output.clone();

            // Spawn stdin writer: forward bytes from the mpsc channel to exec stdin.
            tokio::spawn(async move {
                while let Some(data) = stdin_rx.recv().await {
                    if input.write_all(&data).await.is_err() {
                        break;
                    }
                }
            });

            // Spawn output reader. Only locks the per-session output, not the global map.
            let sid = session_id.clone();
            tokio::spawn(async move {
                while let Some(chunk) = output.next().await {
                    let bytes = match &chunk {
                        Ok(bollard::container::LogOutput::StdOut { message }) => message.to_vec(),
                        Ok(bollard::container::LogOutput::StdErr { message }) => message.to_vec(),
                        Ok(bollard::container::LogOutput::Console { message }) => message.to_vec(),
                        Ok(_) => continue,
                        Err(e) => {
                            tracing::debug!("Exec output stream ended: {}", e);
                            break;
                        }
                    };

                    if bytes.is_empty() {
                        continue;
                    }

                    {
                        let mut out = so.lock().await;
                        // Append to ring buffer.
                        out.ring_buffer.extend(bytes.iter());
                        out.cursor += bytes.len() as u64;

                        // Trim ring buffer if it exceeds the max size.
                        if out.ring_buffer.len() > MAX_RING_BUFFER_BYTES {
                            let overflow = out.ring_buffer.len() - MAX_RING_BUFFER_BYTES;
                            out.ring_buffer.drain(..overflow);
                        }

                        // Broadcast to WebSocket clients (ignore errors -- no receivers is fine).
                        let _ = tx.send((bytes, out.cursor));
                    }
                }

                // Mark session as exited (only locks per-session output, not global map).
                let mut out = so.lock().await;
                out.status = "exited".to_string();
                out.exited_at = Some(std::time::Instant::now());
                tracing::info!("PTY session {} exited", sid);
            });
        }
        StartExecResults::Detached => {
            state.pty_sessions.sessions.write().await.remove(&session_id);
            tracing::error!("Exec {} started in detached mode", exec_id);
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    Ok((StatusCode::CREATED, Json(info)))
}

/// `GET /pty/:id` -- Get session info.
async fn pty_get(
    Path((name, pty_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<PtyInfo>, StatusCode> {
    let sessions = state.pty_sessions.sessions.read().await;
    let session = sessions.get(&pty_id).ok_or(StatusCode::NOT_FOUND)?;
    if session.container != workspace_container(&name) {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(Json(session.info().await))
}

/// `PUT /pty/:id` -- Update session (resize, rename).
async fn pty_update(
    Path((name, pty_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    Json(input): Json<PtyUpdateInput>,
) -> Result<Json<PtyInfo>, StatusCode> {
    // Handle resize first (needs Docker client, done outside session lock).
    let exec_id = {
        let sessions = state.pty_sessions.sessions.read().await;
        let session = sessions.get(&pty_id).ok_or(StatusCode::NOT_FOUND)?;
        if session.container != workspace_container(&name) {
            return Err(StatusCode::NOT_FOUND);
        }
        session.exec_id.clone()
    };

    if let Some(size) = &input.size {
        let docker = connect_docker()?;
        docker
            .resize_exec(
                &exec_id,
                ResizeExecOptions {
                    height: size.rows,
                    width: size.cols,
                },
            )
            .await
            .map_err(|e| {
                tracing::error!("Failed to resize exec {}: {}", exec_id, e);
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
    }

    // Update title.
    let mut sessions = state.pty_sessions.sessions.write().await;
    let session = sessions.get_mut(&pty_id).ok_or(StatusCode::NOT_FOUND)?;
    if session.container != workspace_container(&name) {
        return Err(StatusCode::NOT_FOUND);
    }
    if let Some(title) = input.title {
        session.info.title = title;
    }

    Ok(Json(session.info().await))
}

/// `DELETE /pty/:id` -- Kill session and clean up.
async fn pty_delete(
    Path((name, pty_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
) -> Result<StatusCode, StatusCode> {
    let mut sessions = state.pty_sessions.sessions.write().await;
    let session = sessions.get(&pty_id).ok_or(StatusCode::NOT_FOUND)?;
    if session.container != workspace_container(&name) {
        return Err(StatusCode::NOT_FOUND);
    }
    sessions.remove(&pty_id);
    // Dropping the session closes the broadcast channel and stdin sender,
    // which will cause the background tasks to shut down.
    tracing::info!("Deleted PTY session {}", pty_id);
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /pty/:id/connect` -- WebSocket upgrade.
async fn pty_connect(
    Path((name, pty_id)): Path<(String, String)>,
    Query(query): Query<ConnectQuery>,
    State(state): State<Arc<AppState>>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, StatusCode> {
    // Verify session exists and belongs to this pod before upgrading.
    let sessions = state.pty_sessions.sessions.read().await;
    let session = sessions.get(&pty_id).ok_or(StatusCode::NOT_FOUND)?;
    if session.container != workspace_container(&name) {
        return Err(StatusCode::NOT_FOUND);
    }

    // Snapshot the data we need for the WebSocket handler.
    let replay_cursor = query.cursor.unwrap_or(0);
    let output_rx = session.output_tx.subscribe();
    let stdin_tx = session.stdin_tx.clone();

    // Compute the replay bytes under the per-session output lock.
    let out = session.output.lock().await;
    let current_cursor = out.cursor;
    let replay_bytes = if replay_cursor < current_cursor {
        let buffer_start_cursor = current_cursor - out.ring_buffer.len() as u64;
        let effective_start = replay_cursor.max(buffer_start_cursor);
        let offset = (effective_start - buffer_start_cursor) as usize;
        let (front, back) = out.ring_buffer.as_slices();
        let mut replay = Vec::with_capacity(out.ring_buffer.len() - offset);
        if offset < front.len() {
            replay.extend_from_slice(&front[offset..]);
            replay.extend_from_slice(back);
        } else {
            replay.extend_from_slice(&back[offset - front.len()..]);
        }
        Some(replay)
    } else {
        None
    };
    drop(out);
    drop(sessions);

    let pty_id_owned = pty_id.clone();
    Ok(ws.on_upgrade(move |socket| {
        handle_ws(
            socket,
            pty_id_owned,
            replay_bytes,
            current_cursor,
            output_rx,
            stdin_tx,
        )
    }))
}

/// Handle an upgraded WebSocket connection.
async fn handle_ws(
    socket: WebSocket,
    pty_id: String,
    replay_bytes: Option<Vec<u8>>,
    mut cursor: u64,
    mut output_rx: broadcast::Receiver<(Vec<u8>, u64)>,
    stdin_tx: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // Send meta frame with current cursor.
    let meta = serde_json::json!({"cursor": cursor});
    let mut meta_bytes = vec![0x00u8];
    meta_bytes.extend_from_slice(meta.to_string().as_bytes());
    if ws_tx.send(Message::Binary(meta_bytes.into())).await.is_err() {
        return;
    }

    // Replay buffered output.
    if let Some(replay) = replay_bytes {
        if !replay.is_empty()
            && ws_tx
                .send(Message::Text(
                    String::from_utf8_lossy(&replay).into_owned().into(),
                ))
                .await
                .is_err()
        {
            return;
        }
    }

    // Bridge: output_rx -> WebSocket, WebSocket -> stdin.
    loop {
        tokio::select! {
            // New output from exec.
            result = output_rx.recv() => {
                match result {
                    Ok((data, new_cursor)) => {
                        cursor = new_cursor;
                        // Send text data.
                        if ws_tx.send(Message::Text(
                            String::from_utf8_lossy(&data).into_owned().into()
                        )).await.is_err() {
                            break;
                        }
                        // Send meta frame with updated cursor.
                        let meta = serde_json::json!({"cursor": cursor});
                        let mut meta_bytes = vec![0x00u8];
                        meta_bytes.extend_from_slice(meta.to_string().as_bytes());
                        if ws_tx.send(Message::Binary(meta_bytes.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::debug!("WebSocket client for {} lagged {} messages", pty_id, n);
                        // Continue; the client will miss some output.
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // Session ended.
                        break;
                    }
                }
            }
            // Input from WebSocket client.
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Some(ref tx) = stdin_tx {
                            if tx.send(text.as_bytes().to_vec()).await.is_err() {
                                // stdin closed (exec exited).
                            }
                        }
                    }
                    Some(Ok(Message::Binary(data))) => {
                        if let Some(ref tx) = stdin_tx {
                            if tx.send(data.to_vec()).await.is_err() {
                                // stdin closed.
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        break;
                    }
                    Some(Err(e)) => {
                        tracing::debug!("WebSocket error for {}: {}", pty_id, e);
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    tracing::debug!("WebSocket client disconnected from {}", pty_id);
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Build the PTY sub-router.
///
/// The returned router expects to be nested under a path that provides the
/// `{name}` path parameter (the pod short name). Routes:
///
/// - `GET  /`              -- list sessions
/// - `POST /`              -- create session
/// - `GET  /:pty_id`       -- get session info
/// - `PUT  /:pty_id`       -- update session (resize / rename)
/// - `DELETE /:pty_id`     -- delete session
/// - `GET  /:pty_id/connect` -- WebSocket
pub fn pty_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(pty_list).post(pty_create))
        .route("/{pty_id}", get(pty_get).put(pty_update).delete(pty_delete))
        .route("/{pty_id}/connect", get(pty_connect))
}


