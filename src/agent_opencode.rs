//! OpenCode HTTP API backend for the agent trait.
//!
//! This module implements [`AgentBackend`] for the OpenCode HTTP server,
//! preserving the existing behavior where pod-api queries the OpenCode
//! REST API at `http://127.0.0.1:{port}/session` and proxies requests
//! from the frontend.
//!
//! This is the "legacy" backend that will be used when `--agent opencode-legacy`
//! is specified (or as the default until ACP is fully wired up).

use base64::prelude::*;
use color_eyre::eyre::{Context, Result};

use crate::agent::{
    AgentActivity, AgentBackend, AgentEnvConfig, AgentStartupConfig, AgentStatusSummary,
    MockServerConfig,
};

/// Default port for the OpenCode HTTP server inside the pod.
pub(crate) const DEFAULT_PORT: u16 = 4096;

/// Port for the worker's OpenCode server (internal, no auth).
pub(crate) const WORKER_PORT: u16 = 4098;

/// OpenCode HTTP API backend.
///
/// Queries the OpenCode server via its REST API (`/session`, `/session/{id}/message`)
/// and generates the startup command `opencode serve --port <port>`.
#[derive(Debug, Clone)]
pub(crate) struct OpenCodeBackend;

impl OpenCodeBackend {
    /// Create a new OpenCode backend.
    pub(crate) fn new() -> Self {
        Self
    }
}

/// Maximum number of output lines to return in the summary.
#[allow(dead_code)] // Used by derive_status_from_messages
const SUMMARY_MAX_LINES: usize = 3;

/// Derive agent status fields from OpenCode session messages.
///
/// This is the canonical implementation, extracted from pod_api.rs.
#[allow(dead_code)] // Used by OpenCode backend for legacy HTTP-based agents
pub(crate) fn derive_status_from_messages(
    messages: &[serde_json::Value],
) -> (
    AgentActivity,
    Option<String>, // status_line
    Option<String>, // current_tool
    Vec<String>,    // recent_output
    Option<i64>,    // last_message_ts
) {
    if messages.is_empty() {
        return (AgentActivity::Unknown, None, None, vec![], None);
    }

    // Find the last assistant message.
    let last_assistant = messages.iter().rev().find(|msg| {
        msg.get("info")
            .and_then(|i| i.get("role"))
            .and_then(|r| r.as_str())
            == Some("assistant")
    });

    let Some(last_assistant) = last_assistant else {
        return (AgentActivity::Unknown, None, None, vec![], None);
    };

    let info = match last_assistant.get("info") {
        Some(i) => i,
        None => return (AgentActivity::Unknown, None, None, vec![], None),
    };

    let parts = last_assistant
        .get("parts")
        .and_then(|p| p.as_array())
        .map(|arr| arr.as_slice())
        .unwrap_or(&[]);

    // Extract recent output from parts.
    let recent_output = extract_recent_output(parts);

    // Extract current tool (first incomplete tool).
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

    // Build status line from first text part.
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

    // Determine activity.
    let activity = determine_activity(info, parts);

    // Extract the most recent message timestamp.
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
        activity,
        status_line,
        current_tool,
        recent_output,
        last_message_ts,
    )
}

/// Extract recent output lines from message parts.
#[allow(dead_code)] // Used by derive_status_from_messages
fn extract_recent_output(parts: &[serde_json::Value]) -> Vec<String> {
    let mut lines = Vec::new();
    for part in parts {
        let part_type = part.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match part_type {
            "text" => {
                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                    for line in text.lines().rev().take(SUMMARY_MAX_LINES) {
                        let truncated = if line.chars().count() > 80 {
                            let s: String = line.chars().take(77).collect();
                            format!("{s}...")
                        } else {
                            line.to_string()
                        };
                        if !truncated.trim().is_empty() {
                            lines.push(truncated);
                        }
                        if lines.len() >= SUMMARY_MAX_LINES {
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
        if lines.len() >= SUMMARY_MAX_LINES {
            break;
        }
    }
    lines.reverse();
    lines
}

/// Determine agent activity from the message info and parts.
#[allow(dead_code)] // Used by derive_status_from_messages
fn determine_activity(info: &serde_json::Value, parts: &[serde_json::Value]) -> AgentActivity {
    if info.get("time").and_then(|t| t.get("completed")).is_none() {
        return AgentActivity::Working;
    }

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
        return AgentActivity::Working;
    }

    let finish = info.get("finish").and_then(|f| f.as_str()).unwrap_or("");
    if finish == "tool-calls" {
        AgentActivity::Working
    } else {
        AgentActivity::Idle
    }
}

#[async_trait::async_trait]
impl AgentBackend for OpenCodeBackend {
    async fn query_status(&self, password: &str, port: u16) -> AgentStatusSummary {
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(_) => return AgentStatusSummary::default(),
        };

        let credentials = BASE64_STANDARD.encode(format!("opencode:{}", password));
        let auth_value = format!("Basic {}", credentials);

        // Fetch sessions from the local OpenCode server.
        let sessions_resp = match client
            .get(format!("http://127.0.0.1:{}/session", port))
            .header(reqwest::header::AUTHORIZATION, &auth_value)
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r,
            _ => return AgentStatusSummary::default(),
        };

        let sessions: Vec<serde_json::Value> = match sessions_resp.json().await {
            Ok(s) => s,
            Err(_) => return AgentStatusSummary::default(),
        };

        let session_count = sessions.len();

        if sessions.is_empty() {
            return AgentStatusSummary {
                activity: AgentActivity::Idle,
                status_line: Some("Waiting for input...".to_string()),
                session_count: 0,
                ..Default::default()
            };
        }

        // Find the root session (no parentID or null parentID).
        let root_session = sessions.iter().find(|s| crate::session_is_root(s));

        let session_id = match root_session
            .and_then(|s| s.get("id"))
            .and_then(|id| id.as_str())
        {
            Some(id) => id.to_string(),
            None => return AgentStatusSummary::default(),
        };

        // Fetch recent messages for the root session.
        let messages_resp = match client
            .get(format!(
                "http://127.0.0.1:{}/session/{}/message?limit=5",
                port, session_id
            ))
            .header(reqwest::header::AUTHORIZATION, &auth_value)
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r,
            _ => return AgentStatusSummary::default(),
        };

        let messages: Vec<serde_json::Value> = match messages_resp.json().await {
            Ok(m) => m,
            Err(_) => return AgentStatusSummary::default(),
        };

        let (activity, status_line, current_tool, recent_output, last_message_ts) =
            derive_status_from_messages(&messages);

        AgentStatusSummary {
            activity,
            status_line,
            current_tool,
            recent_output,
            last_message_ts,
            session_count,
        }
    }

    fn startup_command(&self, agent_home: &str, state_path: &str) -> AgentStartupConfig {
        let port = DEFAULT_PORT;

        let startup_script = format!(
            r#"mkdir -p {home}/.config/opencode {home}/.local/share {home}/.local/bin {home}/.cache

# Mock mode: run inline mock server instead of the real opencode server.
# Used by integration tests to avoid needing a real AI provider.
# Uses Python3 (available in all devcontainer images) so no extra binary
# is required in the agent container.
if [ -n "${{DEVAIPOD_MOCK_AGENT}}" ]; then
    # Wait for devaipod to finish setup before starting mock server.
    while [ ! -f {state} ]; do sleep 0.1; done
    exec python3 -u -c "
import json, http.server, socketserver

SESSION = json.dumps([dict(id='mock-001',slug='mock',projectID='p',directory='/workspaces/test',title='Mock',version='1.0.0',time=dict(created=1700000000000,updated=1700000100000))])
MESSAGE = json.dumps([dict(info=dict(role='assistant',time=dict(created=1700000001000,completed=1700000002000),finish='stop'),parts=[dict(type='text',text='Ready.')])])

class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path.startswith('/session') and '/message' in self.path:
            body = MESSAGE
        elif self.path.startswith('/session'):
            body = SESSION
        else:
            self.send_error(404); return
        self.send_response(200)
        self.send_header('Content-Type','application/json')
        self.end_headers()
        self.wfile.write(body.encode())
    def log_message(self, *a): pass

print('Mock opencode on port {port}', flush=True)
socketserver.TCPServer(('0.0.0.0',{port}),H).serve_forever()
"
fi

# Pre-flight: verify the agent binary is available in the container image.
# Check before waiting for the state file so the pod enters Degraded state
# immediately rather than blocking on setup that cannot succeed.
if ! command -v opencode >/dev/null 2>&1; then
    echo "devaipod-error: agent-binary-not-found: opencode" >&2
    exit 42
fi

# Wait for devaipod to finish setup (dotfiles, task config) before starting
# opencode.  The state file lives on the container overlay so it persists
# across stop/start but is absent after a container rebuild.
while [ ! -f {state} ]; do
    sleep 0.1
done

# Run opencode serve, bound to 0.0.0.0 so it's accessible from the published port
exec opencode serve --port {port} --hostname 0.0.0.0"#,
            home = agent_home,
            state = state_path,
            port = port
        );

        AgentStartupConfig {
            startup_script,
            listen_port: Some(port),
        }
    }

    fn container_env(&self, config: &AgentEnvConfig) -> Vec<(String, String)> {
        let mut env = Vec::new();

        // Auto-approve all tool permissions for YOLO mode
        if config.auto_approve {
            env.push((
                "OPENCODE_PERMISSION".to_string(),
                r#"{"*":"allow"}"#.to_string(),
            ));
        }

        // Build MCP config combining service-gator and any additional MCP servers
        let mut mcp_servers = serde_json::Map::new();

        if config.enable_gator {
            mcp_servers.insert(
                "service-gator".to_string(),
                serde_json::json!({
                    "type": "remote",
                    "url": format!("http://localhost:{}/mcp", config.gator_port),
                    "enabled": true
                }),
            );
        }

        // Add any additional MCP servers from config
        for (name, value) in &config.mcp_servers {
            mcp_servers.insert(name.clone(), value.clone());
        }

        if !mcp_servers.is_empty() {
            let mcp_config = serde_json::json!({
                "mcp": mcp_servers
            });
            env.push((
                "OPENCODE_CONFIG_CONTENT".to_string(),
                mcp_config.to_string(),
            ));
        }

        // When orchestration is enabled, set OPENCODE_WORKER_URL
        if config.enable_orchestration {
            env.push((
                "OPENCODE_WORKER_URL".to_string(),
                format!("http://localhost:{}", config.worker_port),
            ));
        }

        env
    }

    fn mock_config(&self, port: u16) -> MockServerConfig {
        MockServerConfig {
            port,
            is_http: true,
        }
    }

    async fn run_mock_server(&self, port: u16) -> Result<()> {
        use axum::Router;
        use axum::extract::Path;
        use axum::routing::get;

        /// `GET /session` — return a canned session list (one root session).
        async fn mock_sessions() -> axum::Json<serde_json::Value> {
            axum::Json(serde_json::json!([
                {
                    "id": "mock-session-001",
                    "slug": "mock-session",
                    "projectID": "proj_001",
                    "directory": "/workspaces/test",
                    "title": "Mock session",
                    "version": "1.0.0",
                    "time": {"created": 1_700_000_000_000_i64, "updated": 1_700_000_100_000_i64}
                }
            ]))
        }

        /// `GET /session/:id/message` — return canned messages showing an idle agent.
        async fn mock_messages(Path(_id): Path<String>) -> axum::Json<serde_json::Value> {
            axum::Json(serde_json::json!([
                {
                    "info": {
                        "role": "assistant",
                        "time": {"created": 1_700_000_001_000_i64, "completed": 1_700_000_002_000_i64},
                        "finish": "stop"
                    },
                    "parts": [
                        {"type": "text", "text": "Ready for testing."}
                    ]
                }
            ]))
        }

        let app = Router::new()
            .route("/session", get(mock_sessions))
            .route("/session/{id}/message", get(mock_messages));

        let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
        tracing::info!("Mock opencode server listening on 0.0.0.0:{}", port);

        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("Failed to bind mock-opencode to {addr}"))?;

        axum::serve(listener, app)
            .with_graceful_shutdown(crate::web::shutdown_signal())
            .await
            .context("mock-opencode server error")?;

        tracing::info!("Mock opencode server shut down gracefully");
        Ok(())
    }

    fn name(&self) -> &str {
        "opencode"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_status_empty_messages() {
        let messages: Vec<serde_json::Value> = vec![];
        let (activity, status_line, current_tool, recent_output, last_ts) =
            derive_status_from_messages(&messages);
        assert_eq!(activity, AgentActivity::Unknown);
        assert!(status_line.is_none());
        assert!(current_tool.is_none());
        assert!(recent_output.is_empty());
        assert!(last_ts.is_none());
    }

    #[test]
    fn test_derive_status_no_assistant_message() {
        let messages = vec![serde_json::json!({
            "info": {"role": "user"},
            "parts": [{"type": "text", "text": "Hello"}]
        })];
        let (activity, ..) = derive_status_from_messages(&messages);
        assert_eq!(activity, AgentActivity::Unknown);
    }

    #[test]
    fn test_derive_status_working_no_completed_time() {
        let messages = vec![serde_json::json!({
            "info": {
                "role": "assistant",
                "time": {"created": 1234567890}
            },
            "parts": [{"type": "text", "text": "Working on it..."}]
        })];
        let (activity, status_line, _, _, _) = derive_status_from_messages(&messages);
        assert_eq!(activity, AgentActivity::Working);
        assert_eq!(status_line.as_deref(), Some("Working on it..."));
    }

    #[test]
    fn test_derive_status_idle_with_stop_finish() {
        let messages = vec![serde_json::json!({
            "info": {
                "role": "assistant",
                "time": {"created": 1234567890, "completed": 1234567891},
                "finish": "stop"
            },
            "parts": [{"type": "text", "text": "Done!"}]
        })];
        let (activity, ..) = derive_status_from_messages(&messages);
        assert_eq!(activity, AgentActivity::Idle);
    }

    #[test]
    fn test_derive_status_working_with_tool_calls_finish() {
        let messages = vec![serde_json::json!({
            "info": {
                "role": "assistant",
                "time": {"created": 1234567890, "completed": 1234567891},
                "finish": "tool-calls"
            },
            "parts": [{"type": "text", "text": "Making tool call..."}]
        })];
        let (activity, ..) = derive_status_from_messages(&messages);
        assert_eq!(activity, AgentActivity::Working);
    }

    #[test]
    fn test_derive_status_working_with_incomplete_tool() {
        let messages = vec![serde_json::json!({
            "info": {
                "role": "assistant",
                "time": {"created": 1234567890, "completed": 1234567891}
            },
            "parts": [
                {"type": "text", "text": "Running a tool..."},
                {"type": "tool", "name": "bash", "state": {"status": "running"}}
            ]
        })];
        let (activity, _, current_tool, _, _) = derive_status_from_messages(&messages);
        assert_eq!(activity, AgentActivity::Working);
        assert_eq!(current_tool.as_deref(), Some("bash"));
    }

    #[test]
    fn test_derive_status_idle_with_completed_tool() {
        let messages = vec![serde_json::json!({
            "info": {
                "role": "assistant",
                "time": {"created": 1234567890, "completed": 1234567891}
            },
            "parts": [
                {"type": "text", "text": "Tool result..."},
                {"type": "tool", "name": "bash", "state": {"status": "completed"}}
            ]
        })];
        let (activity, _, current_tool, _, _) = derive_status_from_messages(&messages);
        assert_eq!(activity, AgentActivity::Idle);
        assert!(
            current_tool.is_none(),
            "completed tool should not appear as current"
        );
    }

    #[test]
    fn test_derive_status_recent_output_truncates_long_lines() {
        let long_line = "x".repeat(100);
        let messages = vec![serde_json::json!({
            "info": {
                "role": "assistant",
                "time": {"created": 1234567890}
            },
            "parts": [{"type": "text", "text": long_line}]
        })];
        let (_, _, _, recent_output, _) = derive_status_from_messages(&messages);
        assert!(!recent_output.is_empty());
        assert!(
            recent_output[0].len() <= 80,
            "long line should be truncated to 80 chars, got {}",
            recent_output[0].len()
        );
        assert!(
            recent_output[0].ends_with("..."),
            "truncated line should end with ellipsis"
        );
    }

    #[test]
    fn test_derive_status_recent_output_includes_tool_entries() {
        let messages = vec![serde_json::json!({
            "info": {
                "role": "assistant",
                "time": {"created": 1234567890}
            },
            "parts": [
                {"type": "tool", "name": "read", "state": {"status": "completed"}}
            ]
        })];
        let (_, _, _, recent_output, _) = derive_status_from_messages(&messages);
        assert!(!recent_output.is_empty());
        assert!(
            recent_output[0].contains("read"),
            "tool entry should appear in recent_output"
        );
    }

    #[test]
    fn test_derive_status_last_message_timestamp() {
        let messages = vec![
            serde_json::json!({
                "info": {
                    "role": "user",
                    "time": {"created": 1000}
                },
                "parts": []
            }),
            serde_json::json!({
                "info": {
                    "role": "assistant",
                    "time": {"created": 2000, "completed": 3000}
                },
                "parts": [{"type": "text", "text": "Done"}]
            }),
        ];
        let (_, _, _, _, last_ts) = derive_status_from_messages(&messages);
        assert_eq!(last_ts, Some(3000), "should pick the max timestamp");
    }

    #[test]
    fn test_derive_status_status_line_truncates() {
        let long_status = "a".repeat(80);
        let messages = vec![serde_json::json!({
            "info": {
                "role": "assistant",
                "time": {"created": 1234567890}
            },
            "parts": [{"type": "text", "text": long_status}]
        })];
        let (_, status_line, _, _, _) = derive_status_from_messages(&messages);
        let sl = status_line.unwrap();
        assert!(
            sl.len() <= 60,
            "status_line should be truncated to 60 chars"
        );
        assert!(
            sl.ends_with("..."),
            "truncated status_line should end with ..."
        );
    }

    #[test]
    fn test_derive_status_multiple_messages_uses_last_assistant() {
        let messages = vec![
            serde_json::json!({
                "info": {
                    "role": "assistant",
                    "time": {"created": 1000, "completed": 1001},
                    "finish": "stop"
                },
                "parts": [{"type": "text", "text": "First response"}]
            }),
            serde_json::json!({
                "info": {"role": "user"},
                "parts": [{"type": "text", "text": "Do more"}]
            }),
            serde_json::json!({
                "info": {
                    "role": "assistant",
                    "time": {"created": 2000}
                },
                "parts": [{"type": "text", "text": "Working on more..."}]
            }),
        ];
        let (activity, status_line, _, _, _) = derive_status_from_messages(&messages);
        assert_eq!(
            activity,
            AgentActivity::Working,
            "should use last assistant message (no completed time)"
        );
        assert_eq!(status_line.as_deref(), Some("Working on more..."));
    }

    #[test]
    fn test_opencode_backend_name() {
        let backend = OpenCodeBackend::new();
        assert_eq!(backend.name(), "opencode");
    }

    #[test]
    fn test_opencode_backend_startup_command() {
        let backend = OpenCodeBackend::new();
        let config = backend.startup_command("/home/devenv", "/var/lib/devaipod-state.json");
        assert_eq!(config.listen_port, Some(DEFAULT_PORT));
        assert!(config.startup_script.contains("opencode serve"));
        assert!(
            config
                .startup_script
                .contains(&format!("--port {}", DEFAULT_PORT))
        );
    }

    #[test]
    fn test_opencode_backend_container_env_yolo() {
        let backend = OpenCodeBackend::new();
        let env_config = AgentEnvConfig {
            agent_home: "/home/devenv".to_string(),
            auto_approve: true,
            enable_gator: false,
            gator_port: 8765,
            enable_orchestration: false,
            worker_port: 4098,
            mcp_servers: vec![],
        };
        let env = backend.container_env(&env_config);
        let has_permission = env
            .iter()
            .any(|(k, v)| k == "OPENCODE_PERMISSION" && v.contains("allow"));
        assert!(
            has_permission,
            "should set OPENCODE_PERMISSION in YOLO mode"
        );
    }

    #[test]
    fn test_opencode_backend_container_env_no_yolo() {
        let backend = OpenCodeBackend::new();
        let env_config = AgentEnvConfig {
            agent_home: "/home/devenv".to_string(),
            auto_approve: false,
            enable_gator: false,
            gator_port: 8765,
            enable_orchestration: false,
            worker_port: 4098,
            mcp_servers: vec![],
        };
        let env = backend.container_env(&env_config);
        let has_permission = env.iter().any(|(k, _)| k == "OPENCODE_PERMISSION");
        assert!(
            !has_permission,
            "should not set OPENCODE_PERMISSION when auto_approve is false"
        );
    }

    #[test]
    fn test_opencode_backend_container_env_gator() {
        let backend = OpenCodeBackend::new();
        let env_config = AgentEnvConfig {
            agent_home: "/home/devenv".to_string(),
            auto_approve: false,
            enable_gator: true,
            gator_port: 8765,
            enable_orchestration: false,
            worker_port: 4098,
            mcp_servers: vec![],
        };
        let env = backend.container_env(&env_config);
        let config_content = env
            .iter()
            .find(|(k, _)| k == "OPENCODE_CONFIG_CONTENT")
            .map(|(_, v)| v.clone());
        assert!(
            config_content.is_some(),
            "should set OPENCODE_CONFIG_CONTENT"
        );
        let content = config_content.unwrap();
        assert!(
            content.contains("service-gator"),
            "MCP config should include service-gator"
        );
        assert!(
            content.contains("8765"),
            "MCP config should include gator port"
        );
    }

    #[test]
    fn test_opencode_backend_container_env_orchestration() {
        let backend = OpenCodeBackend::new();
        let env_config = AgentEnvConfig {
            agent_home: "/home/devenv".to_string(),
            auto_approve: false,
            enable_gator: false,
            gator_port: 8765,
            enable_orchestration: true,
            worker_port: 4098,
            mcp_servers: vec![],
        };
        let env = backend.container_env(&env_config);
        let worker_url = env
            .iter()
            .find(|(k, _)| k == "OPENCODE_WORKER_URL")
            .map(|(_, v)| v.clone());
        assert!(worker_url.is_some(), "should set OPENCODE_WORKER_URL");
        assert!(
            worker_url.unwrap().contains("4098"),
            "worker URL should include worker port"
        );
    }

    #[test]
    fn test_opencode_backend_mock_config() {
        let backend = OpenCodeBackend::new();
        let config = backend.mock_config(4096);
        assert_eq!(config.port, 4096);
        assert!(config.is_http);
    }

    #[test]
    fn test_opencode_backend_is_object_safe() {
        // Verify Box<dyn AgentBackend> works with OpenCodeBackend.
        let _: Box<dyn AgentBackend> = Box::new(OpenCodeBackend::new());
    }
}
