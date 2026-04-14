//! ACP (Agent Client Protocol) backend stub.
//!
//! This module implements [`AgentBackend`] for the ACP protocol over stdio.
//! It is currently a stub that compiles but does not perform real ACP
//! communication. Phase 2 will wire it up to spawn the agent process and
//! manage JSON-RPC sessions.
//!
//! ACP is an open standard (Apache-2.0, JSON-RPC 2.0 over stdio) that
//! standardizes communication between frontends and coding agents. See
//! <https://agentclientprotocol.com> for the specification.

use color_eyre::eyre::Result;

use crate::agent::{
    AgentActivity, AgentBackend, AgentEnvConfig, AgentStartupConfig, AgentStatusSummary,
    MockServerConfig,
};

/// ACP backend over stdio.
///
/// When fully implemented (Phase 2), this backend will:
/// - Spawn the agent process with ACP over stdio
/// - Manage JSON-RPC sessions
/// - Track agent status from ACP event notifications
/// - Expose events over WebSocket for the frontend
///
/// Currently this is a compilable stub returning placeholder values.
#[derive(Debug, Clone)]
pub(crate) struct AcpBackend {
    /// The command to start the agent (e.g., `["opencode", "acp"]`).
    #[allow(dead_code)] // Will be used in Phase 2
    command: Vec<String>,
}

impl AcpBackend {
    /// Create a new ACP backend with the given agent command.
    pub(crate) fn new(command: Vec<String>) -> Self {
        Self { command }
    }
}

#[async_trait::async_trait]
impl AgentBackend for AcpBackend {
    async fn query_status(&self, _password: &str, _port: u16) -> AgentStatusSummary {
        // Stub: ACP agents use stdio, not HTTP. Pod-api bypasses this trait
        // method and uses AcpClient directly to track session state via
        // JSON-RPC events. This stub exists to satisfy the AgentBackend trait
        // and is never called in production.
        //
        // If pod-api's health check needs ACP session state in the future,
        // the implementation should query the AcpClient (which is stored in
        // AppState, not accessible here).
        AgentStatusSummary {
            activity: AgentActivity::Unknown,
            status_line: Some("ACP backend not yet connected".to_string()),
            ..Default::default()
        }
    }

    fn startup_command(&self, agent_home: &str, state_path: &str) -> AgentStartupConfig {
        // ACP agents communicate via stdio, not HTTP. The agent process is
        // started on-demand by pod-api via `podman exec -i`, not as a
        // long-running server. The container just needs to stay alive so
        // pod-api can exec into it.
        let startup_script = format!(
            r#"mkdir -p {home}/.config {home}/.local/share {home}/.local/bin {home}/.cache

# Wait for devaipod to finish setup (dotfiles, task config).
while [ ! -f {state} ]; do
    sleep 0.1
done

# Keep the container alive. Pod-api will start the ACP session on-demand
# via `podman exec -i <agent-container> <agent-command>`.
while true; do
    sleep 3600
done"#,
            home = agent_home,
            state = state_path,
        );

        AgentStartupConfig {
            startup_script,
            // ACP uses stdio, not a network port.
            listen_port: None,
        }
    }

    fn container_env(&self, config: &AgentEnvConfig) -> Vec<(String, String)> {
        // ACP backends configure tool permissions through the ACP protocol
        // (session/request_permission), not through environment variables.
        // However, agents still need MCP server config and internal YOLO
        // settings so they don't double-prompt.
        //
        // The specific env vars depend on which agent binary is being used.
        // For now (Phase 1 stub), we produce nothing. Phase 2 will add
        // per-agent-profile env var generation.
        let _ = config;
        vec![]
    }

    fn mock_config(&self, port: u16) -> MockServerConfig {
        MockServerConfig {
            port,
            // ACP uses stdio, not HTTP. The mock will be a stdio-based
            // JSON-RPC server in Phase 2.
            is_http: false,
        }
    }

    async fn run_mock_server(&self, _port: u16) -> Result<()> {
        // Stub: Phase 2 will implement a mock ACP server over stdio.
        tracing::warn!("ACP mock server not yet implemented");
        // Sleep indefinitely to keep the process alive (matches mock server behavior).
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    fn name(&self) -> &str {
        "acp"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_acp_backend_name() {
        let backend = AcpBackend::new(vec!["opencode".to_string(), "acp".to_string()]);
        assert_eq!(backend.name(), "acp");
    }

    #[test]
    fn test_acp_backend_startup_command() {
        let backend = AcpBackend::new(vec!["opencode".to_string(), "acp".to_string()]);
        let config = backend.startup_command("/home/devenv", "/var/lib/state.json");
        assert!(config.listen_port.is_none(), "ACP uses stdio, not a port");
        assert!(
            config.startup_script.contains("while true"),
            "startup script should keep container alive with infinite loop"
        );
        assert!(
            !config.startup_script.contains("opencode"),
            "agent command should not be in startup script (run via podman exec instead)"
        );
    }

    #[test]
    fn test_acp_backend_startup_command_default() {
        let backend = AcpBackend::new(vec![]);
        let config = backend.startup_command("/home/devenv", "/var/lib/state.json");
        assert!(
            config.startup_script.contains("sleep 3600"),
            "startup script should keep container alive"
        );
        assert!(
            !config.startup_script.contains("opencode"),
            "agent command should not be in startup script (run via podman exec instead)"
        );
    }

    #[test]
    fn test_acp_backend_container_env_is_empty() {
        let backend = AcpBackend::new(vec!["opencode".to_string(), "acp".to_string()]);
        let env_config = AgentEnvConfig {
            agent_home: "/home/devenv".to_string(),
            auto_approve: true,
            enable_gator: true,
            gator_port: 8765,
            enable_orchestration: false,
            worker_port: 4098,
            mcp_servers: vec![],
        };
        let env = backend.container_env(&env_config);
        assert!(env.is_empty(), "ACP stub should not produce env vars yet");
    }

    #[test]
    fn test_acp_backend_mock_config() {
        let backend = AcpBackend::new(vec![]);
        let config = backend.mock_config(4096);
        assert_eq!(config.port, 4096);
        assert!(!config.is_http, "ACP mock should use stdio, not HTTP");
    }

    #[tokio::test]
    async fn test_acp_backend_query_status_returns_unknown() {
        let backend = AcpBackend::new(vec![]);
        let status = backend.query_status("", 0).await;
        assert_eq!(status.activity, AgentActivity::Unknown);
        assert!(status.status_line.is_some());
    }

    #[test]
    fn test_acp_backend_is_object_safe() {
        let _: Box<dyn AgentBackend> = Box::new(AcpBackend::new(vec![]));
    }
}
