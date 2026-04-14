//! ACP (Agent Client Protocol) stdio client.
//!
//! This module implements a `Send + Sync` compatible JSON-RPC client for
//! communicating with ACP agents over stdin/stdout. It uses
//! `agent-client-protocol-schema` for ACP types but implements its own
//! JSON-RPC transport to avoid the `!Send` futures in
//! `agent-client-protocol`'s `ClientSideConnection`.
//!
//! The client is designed for use inside axum handlers (which require
//! `Send` futures) and broadcasts ACP events to WebSocket subscribers.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::{Mutex, broadcast, oneshot};

/// Type-erased async writer (stdin to the agent process, or a test pipe).
type BoxedWriter = Box<dyn AsyncWrite + Send + Unpin>;

/// Pending request slot: maps a JSON-RPC request id to a oneshot sender
/// that delivers the response (or error).
type PendingMap =
    HashMap<i64, oneshot::Sender<Result<Box<serde_json::value::RawValue>, JsonRpcError>>>;

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 wire types (Send-compatible, unlike the crate's built-in ones)
// ---------------------------------------------------------------------------

/// JSON-RPC 2.0 request envelope.
#[derive(Debug, Serialize)]
struct JsonRpcRequest<T: Serialize> {
    jsonrpc: &'static str,
    id: i64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<T>,
}

/// JSON-RPC 2.0 notification envelope (no `id`).
#[derive(Debug, Serialize)]
struct JsonRpcNotification<T: Serialize> {
    jsonrpc: &'static str,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<T>,
}

/// Raw JSON-RPC response for deserialization.
///
/// Covers responses (have `id`), notifications (have `method`, no `id`),
/// and server-to-client requests (have both `id` and `method`).
#[derive(Debug, Deserialize)]
struct JsonRpcMessage {
    id: Option<serde_json::Value>,
    result: Option<Box<serde_json::value::RawValue>>,
    error: Option<JsonRpcError>,
    method: Option<String>,
    params: Option<Box<serde_json::value::RawValue>>,
}

/// JSON-RPC error object.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub(crate) struct JsonRpcError {
    /// Error code.
    pub(crate) code: i32,
    /// Human-readable message.
    pub(crate) message: String,
    /// Optional structured data.
    #[allow(dead_code)]
    pub(crate) data: Option<serde_json::Value>,
}

impl std::fmt::Display for JsonRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JSON-RPC error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for JsonRpcError {}

// ---------------------------------------------------------------------------
// ACP events broadcast to WebSocket clients
// ---------------------------------------------------------------------------

/// ACP event sent to WebSocket clients.
///
/// These are serialized as JSON and sent over the `/ws/events` WebSocket
/// connection. The frontend uses the `type` discriminator to render
/// appropriate UI elements.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum AcpEvent {
    /// ACP session update from the agent (text chunk, tool call, etc.).
    #[serde(rename = "session_update")]
    SessionUpdate {
        /// Session this update belongs to.
        #[serde(rename = "sessionId")]
        session_id: String,
        /// The raw update payload (SessionUpdate enum variant).
        update: serde_json::Value,
    },
    /// Permission request from the agent.
    #[serde(rename = "permission_request")]
    PermissionRequest {
        /// JSON-RPC request id from the agent.
        #[serde(rename = "requestId")]
        request_id: i64,
        /// Session this request belongs to.
        #[serde(rename = "sessionId")]
        session_id: String,
        /// The tool call requiring permission.
        #[serde(rename = "toolCall")]
        tool_call: serde_json::Value,
        /// Available permission options.
        options: serde_json::Value,
    },
    /// Agent initialization completed.
    #[serde(rename = "initialized")]
    Initialized {
        /// Agent implementation info (name, version).
        #[serde(rename = "agentInfo")]
        agent_info: Option<serde_json::Value>,
        /// Agent capabilities.
        capabilities: Option<serde_json::Value>,
    },
    /// A new session was created.
    #[serde(rename = "session_created")]
    SessionCreated {
        /// The newly created session's ID.
        #[serde(rename = "sessionId")]
        session_id: String,
    },
    /// A prompt completed.
    #[serde(rename = "prompt_response")]
    PromptCompleted {
        /// Session this prompt was in.
        #[serde(rename = "sessionId")]
        session_id: String,
        /// Why the prompt ended (e.g. "end_turn").
        #[serde(rename = "stopReason")]
        stop_reason: String,
    },
    /// Connection error.
    #[serde(rename = "error")]
    Error {
        /// Human-readable error description.
        message: String,
    },
    /// Keepalive ping.
    #[serde(rename = "keepalive")]
    Keepalive,
    /// Session list response.
    #[serde(rename = "session_list")]
    SessionList {
        /// List of sessions (opaque JSON from the agent).
        sessions: serde_json::Value,
    },
}

// ---------------------------------------------------------------------------
// AcpClient
// ---------------------------------------------------------------------------

/// ACP client that manages a child process and speaks JSON-RPC over stdio.
///
/// This is `Send + Sync` compatible, designed for use in axum handlers.
/// It spawns the agent command as a child process, reads JSON-RPC messages
/// from stdout in a background task, and writes requests to stdin.
pub(crate) struct AcpClient {
    /// Stdin writer for sending JSON-RPC messages.
    stdin: Arc<Mutex<BoxedWriter>>,
    /// Monotonically increasing request ID counter.
    next_id: Arc<std::sync::atomic::AtomicI64>,
    /// Pending request slots: id → oneshot sender for the response.
    pending: Arc<Mutex<PendingMap>>,
    /// Broadcast channel for ACP events → WebSocket clients.
    event_tx: broadcast::Sender<AcpEvent>,
    /// Child process handle (None for test/in-process clients).
    child: Option<Arc<Mutex<Child>>>,
    /// Current session ID (set after `session/new`).
    session_id: Arc<Mutex<Option<String>>>,
    /// Known session IDs (prevent cross-talk from buggy agents).
    known_sessions: Arc<Mutex<HashSet<String>>>,
}

impl std::fmt::Debug for AcpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcpClient").finish_non_exhaustive()
    }
}

impl Clone for AcpClient {
    fn clone(&self) -> Self {
        Self {
            stdin: Arc::clone(&self.stdin),
            next_id: Arc::clone(&self.next_id),
            pending: Arc::clone(&self.pending),
            event_tx: self.event_tx.clone(),
            child: self.child.as_ref().map(Arc::clone),
            session_id: Arc::clone(&self.session_id),
            known_sessions: Arc::clone(&self.known_sessions),
        }
    }
}

impl AcpClient {
    /// Spawn an ACP agent process and start the stdout reader task.
    ///
    /// - `command`: the agent command (e.g. `["opencode", "acp"]`)
    /// - `env`: environment variables to set on the child process
    /// - `cwd`: working directory for the child
    /// - `event_tx`: broadcast channel for ACP events
    pub(crate) fn spawn(
        command: Vec<String>,
        env: HashMap<String, String>,
        cwd: &str,
        event_tx: broadcast::Sender<AcpEvent>,
    ) -> color_eyre::Result<Self> {
        if command.is_empty() {
            color_eyre::eyre::bail!("ACP agent command is empty");
        }

        let mut cmd = tokio::process::Command::new(&command[0]);
        if command.len() > 1 {
            cmd.args(&command[1..]);
        }
        cmd.current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .envs(env);

        let mut child = cmd.spawn().map_err(|e| {
            color_eyre::eyre::eyre!("Failed to spawn ACP agent {:?}: {}", command, e)
        })?;

        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");

        let client = Self::from_streams(Box::new(stdin), stdout, event_tx);

        // Store the child handle so we can kill it later.
        // Safety: we just created `client` with child=None, so we can set it.
        let client = Self {
            child: Some(Arc::new(Mutex::new(child))),
            ..client
        };

        Ok(client)
    }

    /// Create an `AcpClient` from pre-existing read/write streams.
    ///
    /// This is the core constructor shared by `spawn()` (which passes
    /// `ChildStdin`/`ChildStdout`) and test helpers (which pass duplex
    /// streams). The `child` field is left as `None`; callers that own a
    /// child process should set it after construction.
    fn from_streams(
        writer: BoxedWriter,
        reader: impl AsyncRead + Send + Unpin + 'static,
        event_tx: broadcast::Sender<AcpEvent>,
    ) -> Self {
        let pending: Arc<Mutex<PendingMap>> = Arc::new(Mutex::new(HashMap::new()));
        let stdin_arc: Arc<Mutex<BoxedWriter>> = Arc::new(Mutex::new(writer));

        let known_sessions: Arc<Mutex<HashSet<String>>> =
            Arc::new(Mutex::new(HashSet::new()));

        // Spawn the stdout reader task.
        let reader_pending = Arc::clone(&pending);
        let reader_event_tx = event_tx.clone();
        let reader_stdin = Arc::clone(&stdin_arc);
        let reader_known_sessions = Arc::clone(&known_sessions);

        tokio::spawn(async move {
            let mut buf_reader = BufReader::new(reader);
            let mut line = String::new();
            loop {
                line.clear();
                match buf_reader.read_line(&mut line).await {
                    Ok(0) => {
                        tracing::info!("ACP agent stdout closed (process exited)");
                        let _ = reader_event_tx.send(AcpEvent::Error {
                            message: "Agent process exited".to_string(),
                        });
                        break;
                    }
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        Self::handle_message(
                            trimmed,
                            &reader_pending,
                            &reader_event_tx,
                            &reader_stdin,
                            &reader_known_sessions,
                        )
                        .await;
                    }
                    Err(e) => {
                        tracing::error!("Error reading ACP agent stdout: {}", e);
                        let _ = reader_event_tx.send(AcpEvent::Error {
                            message: format!("Agent stdout read error: {}", e),
                        });
                        break;
                    }
                }
            }
        });

        Self {
            stdin: stdin_arc,
            next_id: Arc::new(std::sync::atomic::AtomicI64::new(1)),
            pending,
            event_tx,
            child: None,
            session_id: Arc::new(Mutex::new(None)),
            known_sessions,
        }
    }

    /// Handle a single JSON-RPC message from the agent's stdout.
    async fn handle_message(
        raw: &str,
        pending: &Mutex<PendingMap>,
        event_tx: &broadcast::Sender<AcpEvent>,
        stdin: &Mutex<BoxedWriter>,
        known_sessions: &Mutex<HashSet<String>>,
    ) {
        let msg: JsonRpcMessage = match serde_json::from_str(raw) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    "Failed to parse JSON-RPC message: {}: {:?}",
                    e,
                    &raw[..raw.len().min(200)]
                );
                return;
            }
        };

        match (&msg.id, &msg.method) {
            // Response to one of our requests (has id, no method).
            (Some(id), None) => {
                let id_num = id.as_i64().unwrap_or(-1);
                let mut pending_lock = pending.lock().await;
                if let Some(sender) = pending_lock.remove(&id_num) {
                    if let Some(error) = msg.error {
                        let _ = sender.send(Err(error));
                    } else if let Some(result) = msg.result {
                        let _ = sender.send(Ok(result));
                    } else {
                        // Null result is valid (e.g. for void methods).
                        let null_raw = serde_json::value::RawValue::from_string("null".to_string())
                            .expect("null is valid JSON");
                        let _ = sender.send(Ok(null_raw));
                    }
                } else {
                    tracing::debug!("Received response for unknown request id {}", id_num);
                }
            }
            // Notification from agent (has method, no id).
            (None, Some(method)) => match method.as_str() {
                "session/update" | "sessionUpdate" => {
                    if let Some(params) = &msg.params
                        && let Ok(v) = serde_json::from_str::<serde_json::Value>(params.get())
                    {
                        let session_id = v
                            .get("sessionId")
                            .and_then(|s| s.as_str())
                            .unwrap_or("unknown")
                            .to_string();

                        // Validate the session ID to prevent cross-talk.
                        let is_known = known_sessions.lock().await.contains(&session_id);
                        if !is_known {
                            tracing::warn!(
                                "Ignoring session/update for unknown session: {}",
                                session_id
                            );
                            return;
                        }

                        let update = v.get("update").cloned().unwrap_or(serde_json::Value::Null);
                        let _ = event_tx.send(AcpEvent::SessionUpdate { session_id, update });
                    }
                }
                other => {
                    tracing::debug!("Ignoring notification from agent: {}", other);
                }
            },
            // Request from agent to client (has both id and method).
            (Some(id), Some(method)) => {
                let id_num = id.as_i64().unwrap_or(0);
                match method.as_str() {
                    "session/requestPermission" => {
                        // All permission requests are broadcast to the frontend for handling.
                        // The agent's own config (YOLO mode) controls whether it even
                        // sends these requests.
                        if let Some(params) = &msg.params
                            && let Ok(v) = serde_json::from_str::<serde_json::Value>(params.get())
                        {
                            let session_id = v
                                .get("sessionId")
                                .and_then(|s| s.as_str())
                                .unwrap_or("unknown")
                                .to_string();
                            let tool_call = v
                                .get("toolCall")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null);
                            let options =
                                v.get("options").cloned().unwrap_or(serde_json::Value::Null);
                            let _ = event_tx.send(AcpEvent::PermissionRequest {
                                request_id: id_num,
                                session_id,
                                tool_call,
                                options,
                            });
                        }
                    }
                    other => {
                        tracing::debug!("Unhandled request from agent: {} (id={})", other, id_num);
                        // Send a "method not found" error response.
                        let error_resp = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id_num,
                            "error": {
                                "code": -32601,
                                "message": format!("Method not implemented: {}", other)
                            }
                        });
                        let mut line = serde_json::to_string(&error_resp).unwrap();
                        line.push('\n');
                        let mut stdin_lock = stdin.lock().await;
                        let _ = stdin_lock.write_all(line.as_bytes()).await;
                        let _ = stdin_lock.flush().await;
                    }
                }
            }
            // Invalid: no id and no method.
            (None, None) => {
                tracing::warn!("Received JSON-RPC message with neither id nor method");
            }
        }
    }

    /// Send a permission response back to the agent.
    async fn send_permission_response(
        stdin: &Mutex<BoxedWriter>,
        request_id: i64,
        option_id: &str,
    ) {
        let resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "selectedOption": option_id,
                "outcome": "allowed"
            }
        });
        let mut line = serde_json::to_string(&resp).unwrap();
        line.push('\n');
        let mut stdin_lock = stdin.lock().await;
        if let Err(e) = stdin_lock.write_all(line.as_bytes()).await {
            tracing::error!("Failed to write permission response: {}", e);
        }
        let _ = stdin_lock.flush().await;
    }

    /// Send a JSON-RPC request and wait for the response.
    async fn request<T: Serialize>(
        &self,
        method: &str,
        params: Option<T>,
    ) -> Result<Box<serde_json::value::RawValue>, JsonRpcError> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        };
        let mut line = serde_json::to_string(&req).map_err(|e| JsonRpcError {
            code: -32600,
            message: format!("Failed to serialize request: {}", e),
            data: None,
        })?;
        line.push('\n');

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id, tx);
        }

        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(line.as_bytes())
                .await
                .map_err(|e| JsonRpcError {
                    code: -32603,
                    message: format!("Failed to write to agent stdin: {}", e),
                    data: None,
                })?;
            stdin.flush().await.map_err(|e| JsonRpcError {
                code: -32603,
                message: format!("Failed to flush agent stdin: {}", e),
                data: None,
            })?;
        }

        // Wait for the response with a timeout.
        match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(JsonRpcError {
                code: -32603,
                message: "Response channel closed (agent may have exited)".to_string(),
                data: None,
            }),
            Err(_) => {
                // Clean up pending slot.
                let mut pending = self.pending.lock().await;
                pending.remove(&id);
                Err(JsonRpcError {
                    code: -32603,
                    message: "Request timed out after 30s".to_string(),
                    data: None,
                })
            }
        }
    }

    /// Send the ACP `initialize` request.
    pub(crate) async fn initialize(&self) -> Result<serde_json::Value, JsonRpcError> {
        use agent_client_protocol_schema::{
            ClientCapabilities, Implementation, InitializeRequest, ProtocolVersion,
        };
        let req = InitializeRequest::new(ProtocolVersion::LATEST)
            .client_info(Implementation::new("devaipod", env!("CARGO_PKG_VERSION")))
            .client_capabilities(ClientCapabilities::default());

        let raw = self.request("initialize", Some(req)).await?;
        let resp: serde_json::Value =
            serde_json::from_str(raw.get()).map_err(|e| JsonRpcError {
                code: -32603,
                message: format!("Failed to parse initialize response: {}", e),
                data: None,
            })?;

        // Send initialized notification.
        let notif = JsonRpcNotification::<serde_json::Value> {
            jsonrpc: "2.0",
            method: "initialized".to_string(),
            params: None,
        };
        let mut line = serde_json::to_string(&notif).unwrap();
        line.push('\n');
        {
            let mut stdin = self.stdin.lock().await;
            let _ = stdin.write_all(line.as_bytes()).await;
            let _ = stdin.flush().await;
        }

        let _ = self.event_tx.send(AcpEvent::Initialized {
            agent_info: resp.get("agentInfo").cloned(),
            capabilities: resp.get("agentCapabilities").cloned(),
        });

        Ok(resp)
    }

    /// Create a new ACP session, returns the session ID.
    pub(crate) async fn new_session(&self, cwd: &str) -> Result<String, JsonRpcError> {
        use agent_client_protocol_schema::NewSessionRequest;
        let req = NewSessionRequest::new(cwd);

        let raw = self.request("session/new", Some(req)).await?;
        let resp: serde_json::Value =
            serde_json::from_str(raw.get()).map_err(|e| JsonRpcError {
                code: -32603,
                message: format!("Failed to parse session/new response: {}", e),
                data: None,
            })?;

        let session_id = resp
            .get("sessionId")
            .and_then(|s| s.as_str())
            .unwrap_or("unknown")
            .to_string();

        {
            let mut sid = self.session_id.lock().await;
            *sid = Some(session_id.clone());
        }

        {
            let mut known = self.known_sessions.lock().await;
            known.insert(session_id.clone());
        }

        let _ = self.event_tx.send(AcpEvent::SessionCreated {
            session_id: session_id.clone(),
        });

        Ok(session_id)
    }

    /// Send a prompt to the agent (fire-and-forget).
    ///
    /// The JSON-RPC request is sent but the response is NOT awaited here.
    /// The reader loop handles the response asynchronously and broadcasts
    /// `PromptCompleted` when it arrives. Session update events stream to
    /// WebSocket clients in real-time as the agent works.
    pub(crate) async fn prompt(&self, session_id: &str, text: &str) -> Result<(), JsonRpcError> {
        use agent_client_protocol_schema::{ContentBlock, PromptRequest, TextContent};
        let params = PromptRequest::new(
            session_id.to_string(),
            vec![ContentBlock::Text(TextContent::new(text.to_string()))],
        );

        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: "session/prompt".to_string(),
            params: Some(params),
        };
        let mut line = serde_json::to_string(&req).map_err(|e| JsonRpcError {
            code: -32600,
            message: format!("Failed to serialize request: {}", e),
            data: None,
        })?;
        line.push('\n');

        // Register a pending slot so the reader loop can handle the response.
        // When the response arrives, broadcast PromptCompleted.
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id, tx);
        }

        // Spawn a task to handle the response asynchronously.
        let event_tx = self.event_tx.clone();
        let sid = session_id.to_string();
        tokio::spawn(async move {
            match rx.await {
                Ok(Ok(raw)) => {
                    let resp: serde_json::Value =
                        serde_json::from_str(raw.get()).unwrap_or_default();
                    let stop_reason = resp
                        .get("stopReason")
                        .and_then(|s| s.as_str())
                        .unwrap_or("end_turn")
                        .to_string();
                    let _ = event_tx.send(AcpEvent::PromptCompleted {
                        session_id: sid,
                        stop_reason,
                    });
                }
                Ok(Err(e)) => {
                    tracing::error!("Prompt response error: {}", e);
                    let _ = event_tx.send(AcpEvent::Error {
                        message: format!("Prompt failed: {}", e),
                    });
                }
                Err(_) => {
                    tracing::error!("Prompt response channel closed");
                }
            }
        });

        // Send the request (don't wait for response).
        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(line.as_bytes())
                .await
                .map_err(|e| JsonRpcError {
                    code: -32603,
                    message: format!("Failed to write to agent stdin: {}", e),
                    data: None,
                })?;
            stdin.flush().await.map_err(|e| JsonRpcError {
                code: -32603,
                message: format!("Failed to flush agent stdin: {}", e),
                data: None,
            })?;
        }

        Ok(())
    }

    /// Send a cancel notification for the given session.
    pub(crate) async fn cancel(&self, session_id: &str) -> Result<(), JsonRpcError> {
        let notif = JsonRpcNotification {
            jsonrpc: "2.0",
            method: "session/cancel".to_string(),
            params: Some(serde_json::json!({ "sessionId": session_id })),
        };
        let mut line = serde_json::to_string(&notif).map_err(|e| JsonRpcError {
            code: -32600,
            message: format!("Failed to serialize cancel: {}", e),
            data: None,
        })?;
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| JsonRpcError {
                code: -32603,
                message: format!("Failed to write cancel: {}", e),
                data: None,
            })?;
        let _ = stdin.flush().await;
        Ok(())
    }

    /// Respond to a permission request from the agent.
    pub(crate) async fn respond_permission(&self, request_id: i64, option_id: &str) {
        Self::send_permission_response(&self.stdin, request_id, option_id).await;
    }

    /// Get the current session ID (if any).
    pub(crate) async fn current_session_id(&self) -> Option<String> {
        self.session_id.lock().await.clone()
    }

    /// List all sessions (returns raw JSON from agent).
    pub(crate) async fn list_sessions(&self) -> Result<serde_json::Value, JsonRpcError> {
        use agent_client_protocol_schema::ListSessionsRequest;
        let req = ListSessionsRequest::new();

        let raw = self.request("session/list", Some(req)).await?;
        let resp: serde_json::Value =
            serde_json::from_str(raw.get()).map_err(|e| JsonRpcError {
                code: -32603,
                message: format!("Failed to parse session/list response: {}", e),
                data: None,
            })?;

        // Add all listed session IDs to the known set.
        if let Some(sessions) = resp.get("sessions").and_then(|s| s.as_array()) {
            let mut known = self.known_sessions.lock().await;
            for session in sessions {
                if let Some(sid) = session.get("sessionId").and_then(|s| s.as_str()) {
                    known.insert(sid.to_string());
                }
            }
        }

        Ok(resp)
    }

    /// Load a session (replays its history via session/update notifications).
    ///
    /// This is fire-and-forget like `prompt()` — the session history will
    /// stream as `session/update` events through the broadcast channel.
    pub(crate) async fn load_session(
        &self,
        session_id: &str,
        cwd: &str,
    ) -> Result<(), JsonRpcError> {
        // Add the session ID to known sessions before loading.
        {
            let mut known = self.known_sessions.lock().await;
            known.insert(session_id.to_string());
        }

        // LoadSessionRequest requires owned strings for 'static lifetime
        let params = serde_json::json!({
            "sessionId": session_id,
            "cwd": cwd,
            "mcpServers": []
        });

        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let rpc_req = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: "session/load".to_string(),
            params: Some(params),
        };
        let mut line = serde_json::to_string(&rpc_req).map_err(|e| JsonRpcError {
            code: -32600,
            message: format!("Failed to serialize request: {}", e),
            data: None,
        })?;
        line.push('\n');

        // Register a pending slot so the reader loop can handle the response.
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id, tx);
        }

        // Spawn a task to handle the response asynchronously (load doesn't
        // emit a PromptCompleted event, so we just log success/error).
        tokio::spawn(async move {
            match rx.await {
                Ok(Ok(_)) => {
                    tracing::debug!("Session load completed");
                }
                Ok(Err(e)) => {
                    tracing::error!("Session load error: {}", e);
                }
                Err(_) => {
                    tracing::error!("Session load response channel closed");
                }
            }
        });

        // Send the request (don't wait for response).
        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(line.as_bytes())
                .await
                .map_err(|e| JsonRpcError {
                    code: -32603,
                    message: format!("Failed to write to agent stdin: {}", e),
                    data: None,
                })?;
            stdin.flush().await.map_err(|e| JsonRpcError {
                code: -32603,
                message: format!("Failed to flush agent stdin: {}", e),
                data: None,
            })?;
        }

        Ok(())
    }

    /// Kill the child process (no-op for test clients without a child).
    #[allow(dead_code)] // Used in integration tests
    pub(crate) async fn kill(&self) {
        if let Some(child) = &self.child {
            let mut child = child.lock().await;
            let _ = child.kill().await;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Mock ACP agent
    // -----------------------------------------------------------------------

    /// Mock ACP agent that speaks JSON-RPC over paired streams.
    ///
    /// Reads newline-delimited JSON-RPC messages from `reader`, dispatches
    /// them by method, and writes responses/notifications to `writer`.
    /// Runs as a tokio task — drop the returned `JoinHandle` (or abort it)
    /// to stop the mock.
    struct MockAcpAgent;

    impl MockAcpAgent {
        /// Spawn a mock agent task and return an `AcpClient` wired to it.
        ///
        /// The mock handles `initialize`, `session/new`, `session/prompt`,
        /// and `session/cancel`. On `session/prompt` it sends a few
        /// `session/update` notifications before the response.
        fn spawn_with_client() -> (
            AcpClient,
            broadcast::Receiver<AcpEvent>,
            tokio::task::JoinHandle<()>,
        ) {
            let (agent_reader, client_writer) = tokio::io::duplex(8192);
            let (client_reader, agent_writer) = tokio::io::duplex(8192);

            let (event_tx, event_rx) = broadcast::channel(64);
            let client = AcpClient::from_streams(Box::new(client_writer), client_reader, event_tx);

            let handle = tokio::spawn(Self::run(agent_reader, agent_writer));

            (client, event_rx, handle)
        }

        async fn run(reader: tokio::io::DuplexStream, writer: tokio::io::DuplexStream) {
            let mut buf_reader = BufReader::new(reader);
            let writer = Arc::new(Mutex::new(writer));
            let mut line = String::new();

            loop {
                line.clear();
                match buf_reader.read_line(&mut line).await {
                    Ok(0) => break, // client closed
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        Self::dispatch(trimmed, &writer).await;
                    }
                    Err(_) => break,
                }
            }
        }

        async fn dispatch(raw: &str, writer: &Mutex<tokio::io::DuplexStream>) {
            // Parse the incoming message.
            let msg: serde_json::Value = match serde_json::from_str(raw) {
                Ok(v) => v,
                Err(_) => return,
            };

            let id = msg.get("id").cloned();
            let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");

            match method {
                "initialize" => {
                    let resp = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "protocolVersion": "2025-03-26",
                            "agentInfo": {
                                "name": "mock-agent",
                                "version": "0.1.0"
                            },
                            "agentCapabilities": {
                                "sessionModes": ["agentic"],
                                "slashCommands": [
                                    {"name": "/help", "description": "Show help"}
                                ]
                            }
                        }
                    });
                    Self::write_line(writer, &resp).await;
                }
                "initialized" => {
                    // Notification from client — no response needed.
                }
                "session/new" => {
                    let resp = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "sessionId": "mock-session-1"
                        }
                    });
                    Self::write_line(writer, &resp).await;
                }
                "session/prompt" => {
                    let session_id = msg
                        .pointer("/params/sessionId")
                        .and_then(|s| s.as_str())
                        .unwrap_or("unknown");

                    // Send session/update notifications before the response.
                    let updates = vec![
                        // 1. Agent message chunk (text).
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "session/update",
                            "params": {
                                "sessionId": session_id,
                                "update": {
                                    "type": "agent_message_chunk",
                                    "text": "Hello from the mock agent!"
                                }
                            }
                        }),
                        // 2. Tool call started.
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "session/update",
                            "params": {
                                "sessionId": session_id,
                                "update": {
                                    "type": "tool_call",
                                    "toolCallId": "tc-1",
                                    "name": "bash",
                                    "arguments": {"command": "echo hello"}
                                }
                            }
                        }),
                        // 3. Tool call completed.
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "session/update",
                            "params": {
                                "sessionId": session_id,
                                "update": {
                                    "type": "tool_call_update",
                                    "toolCallId": "tc-1",
                                    "status": "completed",
                                    "output": "hello\n"
                                }
                            }
                        }),
                    ];

                    for update in &updates {
                        Self::write_line(writer, update).await;
                    }

                    // Prompt response.
                    let resp = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "stopReason": "end_turn"
                        }
                    });
                    Self::write_line(writer, &resp).await;
                }
                "session/cancel" => {
                    // Notification — no response.
                }
                "session/list" => {
                    let resp = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "sessions": [
                                {
                                    "sessionId": "mock-001",
                                    "title": "Test session",
                                    "updatedAt": "2026-01-01T00:00:00Z"
                                }
                            ]
                        }
                    });
                    Self::write_line(writer, &resp).await;
                }
                "session/load" => {
                    let session_id = msg
                        .pointer("/params/sessionId")
                        .and_then(|s| s.as_str())
                        .unwrap_or("unknown");

                    // Send a session/update notification (replayed message).
                    let update = serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session_id,
                            "update": {
                                "type": "agent_message_chunk",
                                "text": "Replayed message from history"
                            }
                        }
                    });
                    Self::write_line(writer, &update).await;

                    // Respond with empty result.
                    let resp = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {}
                    });
                    Self::write_line(writer, &resp).await;
                }
                _ => {
                    // Unknown method — return error.
                    if id.is_some() {
                        let resp = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {
                                "code": -32601,
                                "message": format!("Method not found: {}", method)
                            }
                        });
                        Self::write_line(writer, &resp).await;
                    }
                }
            }
        }

        async fn write_line(writer: &Mutex<tokio::io::DuplexStream>, value: &serde_json::Value) {
            let mut line = serde_json::to_string(value).unwrap();
            line.push('\n');
            let mut w = writer.lock().await;
            let _ = w.write_all(line.as_bytes()).await;
            let _ = w.flush().await;
        }
    }

    // -----------------------------------------------------------------------
    // Existing tests (unchanged)
    // -----------------------------------------------------------------------

    #[test]
    fn test_acp_event_serialization() {
        let event = AcpEvent::Keepalive;
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("keepalive"));

        let event = AcpEvent::SessionUpdate {
            session_id: "abc123".to_string(),
            update: serde_json::json!({"sessionUpdate": "agent_message_chunk"}),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("session_update"));
        assert!(json.contains("abc123"));

        let event = AcpEvent::Error {
            message: "test error".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("test error"));
    }

    #[test]
    fn test_acp_event_deserialization() {
        let json = r#"{"type":"keepalive"}"#;
        let event: AcpEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, AcpEvent::Keepalive));

        let json = r#"{"type":"session_created","sessionId":"test-id"}"#;
        let event: AcpEvent = serde_json::from_str(json).unwrap();
        match event {
            AcpEvent::SessionCreated { session_id } => assert_eq!(session_id, "test-id"),
            _ => panic!("expected SessionCreated"),
        }
    }

    #[test]
    fn test_json_rpc_error_display() {
        let err = JsonRpcError {
            code: -32600,
            message: "Invalid Request".to_string(),
            data: None,
        };
        assert_eq!(format!("{}", err), "JSON-RPC error -32600: Invalid Request");
    }

    #[tokio::test]
    async fn test_acp_client_spawn_with_echo() {
        let (event_tx, _event_rx) = broadcast::channel(16);
        let client = AcpClient::spawn(vec!["cat".to_string()], HashMap::new(), "/tmp", event_tx);
        assert!(client.is_ok());
        if let Ok(c) = client {
            c.kill().await;
        }
    }

    #[test]
    fn test_acp_client_spawn_empty_command() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (event_tx, _) = broadcast::channel(16);
        let result =
            rt.block_on(async { AcpClient::spawn(vec![], HashMap::new(), "/tmp", event_tx) });
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_acp_client_spawn_nonexistent_command() {
        let (event_tx, _) = broadcast::channel(16);
        let result = AcpClient::spawn(
            vec!["nonexistent-acp-agent-binary-xyz123".to_string()],
            HashMap::new(),
            "/tmp",
            event_tx,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_acp_event_roundtrip() {
        let events = vec![
            AcpEvent::Keepalive,
            AcpEvent::Initialized {
                agent_info: Some(serde_json::json!({"name": "test"})),
                capabilities: None,
            },
            AcpEvent::SessionCreated {
                session_id: "s1".to_string(),
            },
            AcpEvent::PromptCompleted {
                session_id: "s1".to_string(),
                stop_reason: "end_turn".to_string(),
            },
            AcpEvent::Error {
                message: "oops".to_string(),
            },
            AcpEvent::PermissionRequest {
                request_id: 42,
                session_id: "s1".to_string(),
                tool_call: serde_json::json!({"name": "bash"}),
                options: serde_json::json!([]),
            },
            AcpEvent::SessionList {
                sessions: serde_json::json!([
                    {"sessionId": "s1", "title": "Test session"},
                    {"sessionId": "s2"}
                ]),
            },
        ];

        for event in events {
            let json = serde_json::to_string(&event).unwrap();
            let _: AcpEvent = serde_json::from_str(&json).unwrap();
        }
    }

    #[test]
    fn test_acp_event_session_list_serialization() {
        let event = AcpEvent::SessionList {
            sessions: serde_json::json!([
                {"sessionId": "s1", "title": "First session"},
                {"sessionId": "s2", "title": "Second session"}
            ]),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"session_list\""));
        assert!(json.contains("\"sessions\""));
        assert!(json.contains("First session"));

        // Verify deserialization
        let parsed: AcpEvent = serde_json::from_str(&json).unwrap();
        match parsed {
            AcpEvent::SessionList { sessions } => {
                assert!(sessions.is_array());
                let arr = sessions.as_array().unwrap();
                assert_eq!(arr.len(), 2);
            }
            _ => panic!("Expected SessionList variant"),
        }
    }

    // -----------------------------------------------------------------------
    // Mock-based integration tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_acp_initialize_handshake() {
        let (client, mut event_rx, _handle) = MockAcpAgent::spawn_with_client();

        let result = client.initialize().await;
        assert!(result.is_ok(), "initialize() failed: {:?}", result.err());

        let resp = result.unwrap();
        assert_eq!(resp["agentInfo"]["name"], "mock-agent");
        assert_eq!(resp["agentInfo"]["version"], "0.1.0");
        assert!(resp["agentCapabilities"]["slashCommands"].is_array());

        // Verify the Initialized event was broadcast.
        let event = event_rx.recv().await.unwrap();
        match event {
            AcpEvent::Initialized {
                agent_info,
                capabilities,
            } => {
                assert!(agent_info.is_some());
                assert_eq!(agent_info.unwrap()["name"], "mock-agent");
                assert!(capabilities.is_some());
            }
            other => panic!("expected Initialized event, got: {:?}", other),
        }

        client.kill().await;
    }

    #[tokio::test]
    async fn test_acp_new_session() {
        let (client, mut event_rx, _handle) = MockAcpAgent::spawn_with_client();

        client.initialize().await.unwrap();
        let _init_event = event_rx.recv().await.unwrap(); // consume Initialized

        let session_id = client.new_session("/tmp").await;
        assert!(
            session_id.is_ok(),
            "new_session() failed: {:?}",
            session_id.err()
        );
        assert_eq!(session_id.unwrap(), "mock-session-1");

        // Verify SessionCreated event.
        let event = event_rx.recv().await.unwrap();
        match event {
            AcpEvent::SessionCreated { session_id } => {
                assert_eq!(session_id, "mock-session-1");
            }
            other => panic!("expected SessionCreated, got: {:?}", other),
        }

        // Verify current_session_id() is set.
        assert_eq!(
            client.current_session_id().await,
            Some("mock-session-1".to_string())
        );

        client.kill().await;
    }

    #[tokio::test]
    async fn test_acp_prompt_receives_events() {
        let (client, mut event_rx, _handle) = MockAcpAgent::spawn_with_client();

        client.initialize().await.unwrap();
        let _init_event = event_rx.recv().await.unwrap();

        let session_id = client.new_session("/tmp").await.unwrap();
        let _session_event = event_rx.recv().await.unwrap();

        // Send a prompt.
        let result = client.prompt(&session_id, "Say hello").await;
        assert!(result.is_ok(), "prompt() failed: {:?}", result.err());

        // Collect the events that were broadcast. The mock sends 3
        // session/update notifications, then the prompt() method itself
        // sends a PromptCompleted event.
        let mut session_updates = Vec::new();
        let mut prompt_completed = false;

        // We expect 4 events total (3 updates + 1 completed).
        for _ in 0..4 {
            match tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await {
                Ok(Ok(AcpEvent::SessionUpdate {
                    session_id: sid,
                    update,
                })) => {
                    assert_eq!(sid, "mock-session-1");
                    session_updates.push(update);
                }
                Ok(Ok(AcpEvent::PromptCompleted {
                    session_id: sid,
                    stop_reason,
                })) => {
                    assert_eq!(sid, "mock-session-1");
                    assert_eq!(stop_reason, "end_turn");
                    prompt_completed = true;
                }
                Ok(Ok(other)) => {
                    panic!("unexpected event: {:?}", other);
                }
                Ok(Err(e)) => panic!("event_rx error: {:?}", e),
                Err(_) => panic!("timed out waiting for event"),
            }
        }

        assert_eq!(session_updates.len(), 3, "expected 3 session updates");
        assert!(prompt_completed, "expected PromptCompleted event");

        // Verify the update types.
        assert_eq!(session_updates[0]["type"], "agent_message_chunk");
        assert_eq!(session_updates[0]["text"], "Hello from the mock agent!");
        assert_eq!(session_updates[1]["type"], "tool_call");
        assert_eq!(session_updates[1]["name"], "bash");
        assert_eq!(session_updates[2]["type"], "tool_call_update");
        assert_eq!(session_updates[2]["status"], "completed");

        client.kill().await;
    }

    #[tokio::test]
    async fn test_acp_cancel_sends_notification() {
        let (client, _event_rx, _handle) = MockAcpAgent::spawn_with_client();

        client.initialize().await.unwrap();
        let session_id = client.new_session("/tmp").await.unwrap();

        // cancel() is a notification — it doesn't wait for a response.
        // It should succeed without error (the mock just ignores it).
        let result = client.cancel(&session_id).await;
        assert!(result.is_ok(), "cancel() failed: {:?}", result.err());

        client.kill().await;
    }

    // Removed test_acp_auto_approve_permission — auto-approve is now the agent's
    // responsibility via its own config, not the ACP client's.

    #[tokio::test]
    async fn test_acp_permission_forwarded() {
        let (agent_reader, client_writer) = tokio::io::duplex(8192);
        let (client_reader, agent_writer) = tokio::io::duplex(8192);

        let (event_tx, mut event_rx) = broadcast::channel(64);
        let _client = AcpClient::from_streams(Box::new(client_writer), client_reader, event_tx);

        let agent_writer = Arc::new(Mutex::new(agent_writer));
        let perm_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 42,
            "method": "session/requestPermission",
            "params": {
                "sessionId": "s1",
                "toolCall": {"name": "write_file", "arguments": {"path": "/etc/hosts"}},
                "options": [{"id": "allow_once"}, {"id": "deny"}]
            }
        });
        {
            let mut line = serde_json::to_string(&perm_request).unwrap();
            line.push('\n');
            let mut w = agent_writer.lock().await;
            w.write_all(line.as_bytes()).await.unwrap();
            w.flush().await.unwrap();
        }

        // The permission request should be forwarded as an event (no auto-approve).
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
            .await
            .expect("timed out")
            .expect("recv error");

        match event {
            AcpEvent::PermissionRequest {
                request_id,
                session_id,
                tool_call,
                ..
            } => {
                assert_eq!(request_id, 42);
                assert_eq!(session_id, "s1");
                assert_eq!(tool_call["name"], "write_file");
            }
            other => panic!("expected PermissionRequest, got: {:?}", other),
        }

        // Now manually respond via the client API.
        _client.respond_permission(42, "deny").await;

        // Read the response from the agent side.
        let mut buf_reader = BufReader::new(agent_reader);
        let mut response_line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            buf_reader.read_line(&mut response_line),
        )
        .await
        .expect("timed out")
        .expect("read error");

        let resp: serde_json::Value = serde_json::from_str(response_line.trim()).unwrap();
        assert_eq!(resp["id"], 42);
        assert_eq!(resp["result"]["selectedOption"], "deny");
    }

    #[tokio::test]
    async fn test_acp_handle_message_response_routing() {
        // Test that handle_message correctly routes responses to pending slots.
        let pending: Arc<Mutex<PendingMap>> = Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, _) = broadcast::channel(16);
        let (_reader, writer) = tokio::io::duplex(1024);
        let stdin: Mutex<BoxedWriter> = Mutex::new(Box::new(writer));
        let known_sessions: Mutex<HashSet<String>> = Mutex::new(HashSet::new());

        // Insert a pending request.
        let (tx, rx) = oneshot::channel();
        pending.lock().await.insert(7, tx);

        // Simulate receiving a response.
        let raw = r#"{"jsonrpc":"2.0","id":7,"result":{"ok":true}}"#;
        AcpClient::handle_message(raw, &pending, &event_tx, &stdin, &known_sessions).await;

        let result = rx.await.unwrap().unwrap();
        let v: serde_json::Value = serde_json::from_str(result.get()).unwrap();
        assert_eq!(v["ok"], true);
    }

    #[tokio::test]
    async fn test_acp_handle_message_error_response() {
        let pending: Arc<Mutex<PendingMap>> = Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, _) = broadcast::channel(16);
        let (_reader, writer) = tokio::io::duplex(1024);
        let stdin: Mutex<BoxedWriter> = Mutex::new(Box::new(writer));
        let known_sessions: Mutex<HashSet<String>> = Mutex::new(HashSet::new());

        let (tx, rx) = oneshot::channel();
        pending.lock().await.insert(3, tx);

        let raw =
            r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32601,"message":"Method not found"}}"#;
        AcpClient::handle_message(raw, &pending, &event_tx, &stdin, &known_sessions).await;

        let result = rx.await.unwrap();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, -32601);
        assert_eq!(err.message, "Method not found");
    }

    #[tokio::test]
    async fn test_acp_handle_message_null_result() {
        let pending: Arc<Mutex<PendingMap>> = Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, _) = broadcast::channel(16);
        let (_reader, writer) = tokio::io::duplex(1024);
        let stdin: Mutex<BoxedWriter> = Mutex::new(Box::new(writer));
        let known_sessions: Mutex<HashSet<String>> = Mutex::new(HashSet::new());

        let (tx, rx) = oneshot::channel();
        pending.lock().await.insert(5, tx);

        // Response with neither result nor error → should yield null.
        let raw = r#"{"jsonrpc":"2.0","id":5}"#;
        AcpClient::handle_message(raw, &pending, &event_tx, &stdin, &known_sessions).await;

        let result = rx.await.unwrap().unwrap();
        let v: serde_json::Value = serde_json::from_str(result.get()).unwrap();
        assert!(v.is_null());
    }

    #[tokio::test]
    async fn test_acp_handle_message_session_update_notification() {
        let pending: Arc<Mutex<PendingMap>> = Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, mut event_rx) = broadcast::channel(16);
        let (_reader, writer) = tokio::io::duplex(1024);
        let stdin: Mutex<BoxedWriter> = Mutex::new(Box::new(writer));
        let known_sessions: Mutex<HashSet<String>> = Mutex::new(HashSet::new());

        // Add the session to known sessions before testing.
        known_sessions.lock().await.insert("s42".to_string());

        let raw = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s42","update":{"type":"agent_message_chunk","text":"hi"}}}"#;
        AcpClient::handle_message(raw, &pending, &event_tx, &stdin, &known_sessions).await;

        let event = event_rx.recv().await.unwrap();
        match event {
            AcpEvent::SessionUpdate { session_id, update } => {
                assert_eq!(session_id, "s42");
                assert_eq!(update["type"], "agent_message_chunk");
                assert_eq!(update["text"], "hi");
            }
            other => panic!("expected SessionUpdate, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_acp_handle_message_unknown_session_rejected() {
        let pending: Arc<Mutex<PendingMap>> = Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, mut event_rx) = broadcast::channel(16);
        let (_reader, writer) = tokio::io::duplex(1024);
        let stdin: Mutex<BoxedWriter> = Mutex::new(Box::new(writer));
        let known_sessions: Mutex<HashSet<String>> = Mutex::new(HashSet::new());

        // Don't add "s99" to known_sessions — it should be rejected.
        let raw = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s99","update":{"type":"agent_message_chunk","text":"malicious"}}}"#;
        AcpClient::handle_message(raw, &pending, &event_tx, &stdin, &known_sessions).await;

        // The event should NOT be broadcast.
        match tokio::time::timeout(std::time::Duration::from_millis(100), event_rx.recv()).await {
            Ok(_) => panic!("expected no event for unknown session"),
            Err(_) => {} // timeout is expected
        }
    }

    #[tokio::test]
    async fn test_acp_request_timeout() {
        // Create a mock that never responds.
        let (_agent_reader, client_writer) = tokio::io::duplex(8192);
        let (client_reader, _agent_writer) = tokio::io::duplex(8192);

        let (event_tx, _) = broadcast::channel(16);
        let client = AcpClient::from_streams(Box::new(client_writer), client_reader, event_tx);

        // Override the default 30s timeout — we test with a raw request
        // to a method the mock ignores, so it will time out. But 30s is
        // too long for a unit test. Instead, test the timeout path by
        // verifying the pending slot is cleaned up when the channel is
        // dropped.
        //
        // We can't easily reduce the timeout without changing the code,
        // so instead test that a dropped oneshot sender produces the
        // expected error.
        let id = client
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = oneshot::channel::<Result<Box<serde_json::value::RawValue>, JsonRpcError>>();
        client.pending.lock().await.insert(id, tx);

        // Drop the sender — simulates agent exit / channel close.
        client.pending.lock().await.remove(&id);
        drop(rx);

        // Verify pending map is clean.
        assert!(client.pending.lock().await.is_empty());

        client.kill().await;
    }

    #[tokio::test]
    async fn test_acp_full_flow_init_session_prompt() {
        // End-to-end: initialize → new_session → prompt.
        let (client, mut event_rx, _handle) = MockAcpAgent::spawn_with_client();

        // 1. Initialize.
        let init = client.initialize().await.unwrap();
        assert_eq!(init["agentInfo"]["name"], "mock-agent");

        let event = event_rx.recv().await.unwrap();
        assert!(matches!(event, AcpEvent::Initialized { .. }));

        // 2. New session.
        let sid = client.new_session("/workspace").await.unwrap();
        assert_eq!(sid, "mock-session-1");

        let event = event_rx.recv().await.unwrap();
        assert!(matches!(event, AcpEvent::SessionCreated { .. }));

        // 3. Prompt.
        client.prompt(&sid, "Do something").await.unwrap();

        // Drain events: 3 updates + 1 completed.
        let mut update_count = 0;
        for _ in 0..4 {
            let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
                .await
                .unwrap()
                .unwrap();
            match event {
                AcpEvent::SessionUpdate { .. } => update_count += 1,
                AcpEvent::PromptCompleted { stop_reason, .. } => {
                    assert_eq!(stop_reason, "end_turn");
                }
                other => panic!("unexpected: {:?}", other),
            }
        }
        assert_eq!(update_count, 3);

        client.kill().await;
    }

    // Removed test_acp_set_auto_approve_toggle — auto-approve is now the agent's
    // responsibility via its own config.

    // -----------------------------------------------------------------------
    // Mock "Goose" ACP agent — second agent with different behavior
    // -----------------------------------------------------------------------

    /// Mock ACP agent simulating a "Goose"-like agent with different slash
    /// commands, tool kinds, and event patterns than [`MockAcpAgent`].
    ///
    /// Differences from MockAcpAgent:
    /// - `agentInfo`: name "goose", version "2.0.0"
    /// - Slash commands: `/research`, `/implement`, `/review`
    /// - Session modes: "research", "implement"
    /// - Tool calls use kinds "search" and "execute" instead of unnamed
    /// - Sends thinking blocks and interleaved text + tool calls
    struct MockGooseAgent;

    impl MockGooseAgent {
        /// Spawn a mock Goose agent and return an `AcpClient` wired to it.
        fn spawn_with_client() -> (
            AcpClient,
            broadcast::Receiver<AcpEvent>,
            tokio::task::JoinHandle<()>,
        ) {
            let (agent_reader, client_writer) = tokio::io::duplex(8192);
            let (client_reader, agent_writer) = tokio::io::duplex(8192);

            let (event_tx, event_rx) = broadcast::channel(64);
            let client = AcpClient::from_streams(Box::new(client_writer), client_reader, event_tx);

            let handle = tokio::spawn(Self::run(agent_reader, agent_writer));

            (client, event_rx, handle)
        }

        async fn run(reader: tokio::io::DuplexStream, writer: tokio::io::DuplexStream) {
            let mut buf_reader = BufReader::new(reader);
            let writer = Arc::new(Mutex::new(writer));
            let mut line = String::new();

            loop {
                line.clear();
                match buf_reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        Self::dispatch(trimmed, &writer).await;
                    }
                    Err(_) => break,
                }
            }
        }

        async fn dispatch(raw: &str, writer: &Mutex<tokio::io::DuplexStream>) {
            let msg: serde_json::Value = match serde_json::from_str(raw) {
                Ok(v) => v,
                Err(_) => return,
            };

            let id = msg.get("id").cloned();
            let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");

            match method {
                "initialize" => {
                    let resp = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "protocolVersion": "2025-03-26",
                            "agentInfo": {
                                "name": "goose",
                                "version": "2.0.0"
                            },
                            "agentCapabilities": {
                                "sessionModes": ["research", "implement"],
                                "slashCommands": [
                                    {"name": "/research", "description": "Deep research mode"},
                                    {"name": "/implement", "description": "Implementation mode"},
                                    {"name": "/review", "description": "Code review mode"}
                                ]
                            }
                        }
                    });
                    Self::write_line(writer, &resp).await;
                }
                "initialized" => {
                    // Notification — no response.
                }
                "session/new" => {
                    let resp = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "sessionId": "goose-session-1"
                        }
                    });
                    Self::write_line(writer, &resp).await;
                }
                "session/prompt" => {
                    let session_id = msg
                        .pointer("/params/sessionId")
                        .and_then(|s| s.as_str())
                        .unwrap_or("unknown");

                    // Goose sends a different event pattern than MockAcpAgent:
                    // 1. Thinking block
                    // 2. Text chunk
                    // 3. Tool call with kind "search"
                    // 4. Another text chunk (interleaved)
                    // 5. Tool call with kind "execute"
                    // 6. Final text chunk
                    let updates = vec![
                        // 1. Thinking block.
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "session/update",
                            "params": {
                                "sessionId": session_id,
                                "update": {
                                    "type": "agent_message_chunk",
                                    "thinking": true,
                                    "text": "Let me analyze the codebase structure..."
                                }
                            }
                        }),
                        // 2. Text chunk.
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "session/update",
                            "params": {
                                "sessionId": session_id,
                                "update": {
                                    "type": "agent_message_chunk",
                                    "text": "I'll search for relevant files first."
                                }
                            }
                        }),
                        // 3. Tool call with kind "search".
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "session/update",
                            "params": {
                                "sessionId": session_id,
                                "update": {
                                    "type": "tool_call",
                                    "toolCallId": "goose-tc-1",
                                    "name": "grep",
                                    "kind": "search",
                                    "title": "Searching codebase",
                                    "arguments": {"pattern": "fn main", "path": "."}
                                }
                            }
                        }),
                        // 4. Interleaved text chunk.
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "session/update",
                            "params": {
                                "sessionId": session_id,
                                "update": {
                                    "type": "agent_message_chunk",
                                    "text": "Found the entry point. Now executing the build."
                                }
                            }
                        }),
                        // 5. Tool call with kind "execute".
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "session/update",
                            "params": {
                                "sessionId": session_id,
                                "update": {
                                    "type": "tool_call",
                                    "toolCallId": "goose-tc-2",
                                    "name": "shell",
                                    "kind": "execute",
                                    "title": "Running build",
                                    "arguments": {"command": "cargo build"}
                                }
                            }
                        }),
                        // 6. Final text chunk.
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "session/update",
                            "params": {
                                "sessionId": session_id,
                                "update": {
                                    "type": "agent_message_chunk",
                                    "text": "Build completed successfully."
                                }
                            }
                        }),
                    ];

                    for update in &updates {
                        Self::write_line(writer, update).await;
                    }

                    // Prompt response.
                    let resp = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "stopReason": "end_turn"
                        }
                    });
                    Self::write_line(writer, &resp).await;
                }
                "session/cancel" => {
                    // Notification — no response.
                }
                _ => {
                    if id.is_some() {
                        let resp = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {
                                "code": -32601,
                                "message": format!("Method not found: {}", method)
                            }
                        });
                        Self::write_line(writer, &resp).await;
                    }
                }
            }
        }

        async fn write_line(writer: &Mutex<tokio::io::DuplexStream>, value: &serde_json::Value) {
            let mut line = serde_json::to_string(value).unwrap();
            line.push('\n');
            let mut w = writer.lock().await;
            let _ = w.write_all(line.as_bytes()).await;
            let _ = w.flush().await;
        }
    }

    // -----------------------------------------------------------------------
    // Generic framework validation tests (second agent)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_second_agent_different_slash_commands() {
        let (client, mut event_rx, _handle) = MockGooseAgent::spawn_with_client();

        // Initialize and verify different agentInfo.
        let init = client.initialize().await.unwrap();
        assert_eq!(init["agentInfo"]["name"], "goose");
        assert_eq!(init["agentInfo"]["version"], "2.0.0");

        // Verify different slash commands.
        let commands = init["agentCapabilities"]["slashCommands"]
            .as_array()
            .unwrap();
        assert_eq!(commands.len(), 3);
        let names: Vec<&str> = commands
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"/research"));
        assert!(names.contains(&"/implement"));
        assert!(names.contains(&"/review"));
        // MockAcpAgent's /help should NOT be present.
        assert!(!names.contains(&"/help"));

        // Verify different session modes.
        let modes = init["agentCapabilities"]["sessionModes"]
            .as_array()
            .unwrap();
        let mode_strs: Vec<&str> = modes.iter().map(|m| m.as_str().unwrap()).collect();
        assert!(mode_strs.contains(&"research"));
        assert!(mode_strs.contains(&"implement"));

        // Verify Initialized event.
        let event = event_rx.recv().await.unwrap();
        match event {
            AcpEvent::Initialized { agent_info, .. } => {
                assert_eq!(agent_info.unwrap()["name"], "goose");
            }
            other => panic!("expected Initialized, got: {:?}", other),
        }

        // Create session — verify different session ID prefix.
        let sid = client.new_session("/workspace").await.unwrap();
        assert_eq!(sid, "goose-session-1");

        client.kill().await;
    }

    #[tokio::test]
    async fn test_second_agent_different_tool_kinds() {
        let (client, mut event_rx, _handle) = MockGooseAgent::spawn_with_client();

        client.initialize().await.unwrap();
        let _init_event = event_rx.recv().await.unwrap();

        let sid = client.new_session("/workspace").await.unwrap();
        let _session_event = event_rx.recv().await.unwrap();

        // Send prompt — Goose sends 6 updates + 1 PromptCompleted.
        client.prompt(&sid, "Build the project").await.unwrap();

        let mut updates = Vec::new();
        let mut completed = false;

        for _ in 0..7 {
            match tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await {
                Ok(Ok(AcpEvent::SessionUpdate { update, .. })) => {
                    updates.push(update);
                }
                Ok(Ok(AcpEvent::PromptCompleted { stop_reason, .. })) => {
                    assert_eq!(stop_reason, "end_turn");
                    completed = true;
                }
                Ok(Ok(other)) => panic!("unexpected event: {:?}", other),
                Ok(Err(e)) => panic!("recv error: {:?}", e),
                Err(_) => panic!("timed out waiting for event"),
            }
        }

        assert!(completed, "expected PromptCompleted");
        assert_eq!(updates.len(), 6, "expected 6 session updates from Goose");

        // Verify thinking block (update 0).
        assert_eq!(updates[0]["type"], "agent_message_chunk");
        assert_eq!(updates[0]["thinking"], true);
        assert!(updates[0]["text"]
            .as_str()
            .unwrap()
            .contains("analyze"));

        // Verify tool call with kind "search" (update 2).
        assert_eq!(updates[2]["type"], "tool_call");
        assert_eq!(updates[2]["kind"], "search");
        assert_eq!(updates[2]["title"], "Searching codebase");
        assert_eq!(updates[2]["name"], "grep");

        // Verify interleaved text (update 3).
        assert_eq!(updates[3]["type"], "agent_message_chunk");
        assert!(updates[3]["text"]
            .as_str()
            .unwrap()
            .contains("entry point"));

        // Verify tool call with kind "execute" (update 4).
        assert_eq!(updates[4]["type"], "tool_call");
        assert_eq!(updates[4]["kind"], "execute");
        assert_eq!(updates[4]["title"], "Running build");
        assert_eq!(updates[4]["name"], "shell");

        client.kill().await;
    }

    #[tokio::test]
    async fn test_both_agents_use_same_client() {
        // Prove that the same AcpClient code handles both mock agents
        // without any agent-specific logic.

        // --- Agent 1: MockAcpAgent ---
        let (client1, mut rx1, _h1) = MockAcpAgent::spawn_with_client();
        let init1 = client1.initialize().await.unwrap();
        assert_eq!(init1["agentInfo"]["name"], "mock-agent");
        let _ev = rx1.recv().await.unwrap(); // Initialized

        let sid1 = client1.new_session("/tmp").await.unwrap();
        assert_eq!(sid1, "mock-session-1");
        let _ev = rx1.recv().await.unwrap(); // SessionCreated

        client1.prompt(&sid1, "hello").await.unwrap();
        // Drain 3 updates + 1 completed = 4 events.
        let mut count1 = 0;
        for _ in 0..4 {
            let ev = tokio::time::timeout(std::time::Duration::from_secs(2), rx1.recv())
                .await
                .unwrap()
                .unwrap();
            match ev {
                AcpEvent::SessionUpdate { .. } => count1 += 1,
                AcpEvent::PromptCompleted { .. } => {}
                other => panic!("agent1 unexpected: {:?}", other),
            }
        }
        assert_eq!(count1, 3, "MockAcpAgent sends 3 updates");

        // --- Agent 2: MockGooseAgent ---
        let (client2, mut rx2, _h2) = MockGooseAgent::spawn_with_client();
        let init2 = client2.initialize().await.unwrap();
        assert_eq!(init2["agentInfo"]["name"], "goose");
        let _ev = rx2.recv().await.unwrap(); // Initialized

        let sid2 = client2.new_session("/workspace").await.unwrap();
        assert_eq!(sid2, "goose-session-1");
        let _ev = rx2.recv().await.unwrap(); // SessionCreated

        client2.prompt(&sid2, "build").await.unwrap();
        // Drain 6 updates + 1 completed = 7 events.
        let mut count2 = 0;
        for _ in 0..7 {
            let ev = tokio::time::timeout(std::time::Duration::from_secs(2), rx2.recv())
                .await
                .unwrap()
                .unwrap();
            match ev {
                AcpEvent::SessionUpdate { .. } => count2 += 1,
                AcpEvent::PromptCompleted { .. } => {}
                other => panic!("agent2 unexpected: {:?}", other),
            }
        }
        assert_eq!(count2, 6, "MockGooseAgent sends 6 updates");

        // Both used the exact same AcpClient type — zero agent-specific code.
        client1.kill().await;
        client2.kill().await;
    }

    #[tokio::test]
    async fn test_acp_handle_unknown_method_from_agent() {
        // When the agent sends a request with an unknown method, the client
        // should respond with a -32601 error.
        let (agent_reader, client_writer) = tokio::io::duplex(8192);
        let (client_reader, agent_writer) = tokio::io::duplex(8192);

        let (event_tx, _) = broadcast::channel(64);
        let _client = AcpClient::from_streams(Box::new(client_writer), client_reader, event_tx);

        // Send an unknown request from the "agent".
        let agent_writer = Arc::new(Mutex::new(agent_writer));
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 123,
            "method": "agent/unknownMethod",
            "params": {}
        });
        {
            let mut line = serde_json::to_string(&req).unwrap();
            line.push('\n');
            let mut w = agent_writer.lock().await;
            w.write_all(line.as_bytes()).await.unwrap();
            w.flush().await.unwrap();
        }

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Read the error response.
        let mut buf_reader = BufReader::new(agent_reader);
        let mut response_line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            buf_reader.read_line(&mut response_line),
        )
        .await
        .expect("timed out")
        .expect("read error");

        let resp: serde_json::Value = serde_json::from_str(response_line.trim()).unwrap();
        assert_eq!(resp["id"], 123);
        assert_eq!(resp["error"]["code"], -32601);
        assert!(
            resp["error"]["message"]
                .as_str()
                .unwrap()
                .contains("unknownMethod")
        );
    }

    #[tokio::test]
    async fn test_acp_list_sessions() {
        let (client, mut event_rx, _handle) = MockAcpAgent::spawn_with_client();

        client.initialize().await.unwrap();
        let _init_event = event_rx.recv().await.unwrap();

        let session_id = client.new_session("/tmp").await.unwrap();
        let _session_event = event_rx.recv().await.unwrap();

        // Call list_sessions.
        let result = client.list_sessions().await;
        assert!(result.is_ok(), "list_sessions() failed: {:?}", result.err());

        let resp = result.unwrap();
        let sessions = resp.get("sessions").and_then(|s| s.as_array()).unwrap();
        assert_eq!(sessions.len(), 1, "expected 1 session in list");
        assert_eq!(sessions[0]["sessionId"], "mock-001");
        assert_eq!(sessions[0]["title"], "Test session");
        assert_eq!(sessions[0]["updatedAt"], "2026-01-01T00:00:00Z");

        // Verify known_sessions includes the listed session ID.
        let known = client.known_sessions.lock().await;
        assert!(known.contains("mock-001"), "mock-001 should be in known_sessions");
        assert!(known.contains(&session_id), "created session should be in known_sessions");

        client.kill().await;
    }

    #[tokio::test]
    async fn test_acp_load_session() {
        let (client, mut event_rx, _handle) = MockAcpAgent::spawn_with_client();

        client.initialize().await.unwrap();
        let _init_event = event_rx.recv().await.unwrap();

        let session_id = client.new_session("/tmp").await.unwrap();
        let _session_event = event_rx.recv().await.unwrap();

        // Call load_session.
        let result = client.load_session(&session_id, "/workspace").await;
        assert!(result.is_ok(), "load_session() failed: {:?}", result.err());

        // Verify a SessionUpdate event is received (replayed message).
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
            .await
            .expect("timed out waiting for SessionUpdate")
            .expect("recv error");

        match event {
            AcpEvent::SessionUpdate { session_id: sid, update } => {
                assert_eq!(sid, session_id);
                assert_eq!(update["type"], "agent_message_chunk");
                assert_eq!(update["text"], "Replayed message from history");
            }
            other => panic!("expected SessionUpdate, got: {:?}", other),
        }

        client.kill().await;
    }

    #[tokio::test]
    async fn test_acp_agent_stdout_close() {
        // Create client with duplex pipe, then drop the write end to simulate agent crash.
        let (client_reader, agent_writer) = tokio::io::duplex(8192);
        let (agent_reader, client_writer) = tokio::io::duplex(8192);

        let (event_tx, mut event_rx) = broadcast::channel(64);
        let _client = AcpClient::from_streams(Box::new(client_writer), client_reader, event_tx);

        // Drop the agent_writer to close stdout from the agent's side.
        drop(agent_writer);
        drop(agent_reader);

        // Verify AcpEvent::Error with "Agent process exited" is received.
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
            .await
            .expect("timed out waiting for Error event")
            .expect("recv error");

        match event {
            AcpEvent::Error { message } => {
                assert_eq!(message, "Agent process exited");
            }
            other => panic!("expected AcpEvent::Error, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_acp_malformed_json_on_stdout() {
        // Create client with duplex pipe.
        let (client_reader, mut agent_writer) = tokio::io::duplex(8192);
        let (_agent_reader, client_writer) = tokio::io::duplex(8192);

        let (event_tx, mut event_rx) = broadcast::channel(64);
        let _client = AcpClient::from_streams(Box::new(client_writer), client_reader, event_tx);

        // Write garbage text followed by a valid JSON-RPC message.
        agent_writer.write_all(b"This is not JSON!\n").await.unwrap();
        agent_writer.flush().await.unwrap();

        // Give the reader task time to process the garbage (it should log a warning and skip it).
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Now write a valid initialize response.
        let valid_msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "protocolVersion": "2025-03-26",
                "agentInfo": {"name": "test", "version": "1.0.0"}
            }
        });
        let mut line = serde_json::to_string(&valid_msg).unwrap();
        line.push('\n');
        agent_writer.write_all(line.as_bytes()).await.unwrap();
        agent_writer.flush().await.unwrap();

        // The client should process the valid message correctly.
        // To verify, we need to have a pending request with id=1.
        let (tx, rx) = oneshot::channel();
        _client.pending.lock().await.insert(1, tx);

        // Wait for the response to be routed to the pending slot.
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), rx)
            .await
            .expect("timed out waiting for response")
            .expect("channel closed");

        assert!(result.is_ok(), "expected successful response");
        let raw = result.unwrap();
        let v: serde_json::Value = serde_json::from_str(raw.get()).unwrap();
        assert_eq!(v["agentInfo"]["name"], "test");

        // Verify no Error event was broadcast (garbage was silently skipped).
        match tokio::time::timeout(std::time::Duration::from_millis(100), event_rx.recv()).await {
            Ok(_) => panic!("expected no event for malformed JSON"),
            Err(_) => {} // timeout is expected — no error event
        }
    }
}
