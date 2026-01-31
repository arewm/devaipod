//! TUI (Text User Interface) for devaipod
//!
//! Provides a real-time dashboard for managing devaipod instances using async Rust
//! with ratatui for rendering and bollard for container API access.

use std::collections::HashMap;
use std::io::{self, IsTerminal, Stdout};
use std::time::Duration;

use tokio::sync::mpsc;

use bollard::container::ListContainersOptions;
use bollard::models::ContainerSummary;
use bollard::Docker;
use color_eyre::eyre::{Context, Result};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::Terminal;
use tokio::time::interval;

/// State file name for persistent TUI/instance state
const STATE_FILE_NAME: &str = "state.json";

/// Cache for TUI state, versioned for compatibility
#[derive(serde::Serialize, serde::Deserialize)]
struct TuiStateCache {
    /// Application version for compatibility check
    version: String,
    /// Cached state per instance (keyed by instance name)
    instances: HashMap<String, CachedInstanceState>,
}

/// Cached state for a single instance
#[derive(serde::Serialize, serde::Deserialize)]
struct CachedInstanceState {
    /// Cached git repository state
    git_state: Option<GitState>,
    /// Cached agent activity state
    agent_state: Option<AgentState>,
    /// Unix timestamp when this cache entry was last updated
    updated_at: i64,
}

/// Get the persistent state file path.
///
/// Uses XDG_DATA_HOME (typically ~/.local/share), falling back to ~/.local/share.
/// Creates the directory if it doesn't exist.
fn state_file_path() -> std::path::PathBuf {
    let data_dir = std::env::var("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|_| {
            std::env::var("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share"))
        })
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
        .join("devaipod");

    // Ensure directory exists
    let _ = std::fs::create_dir_all(&data_dir);

    data_dir.join(STATE_FILE_NAME)
}

/// Load the TUI state from disk.
/// Returns None if state doesn't exist, is corrupt, or has a version mismatch.
fn load_state() -> Option<TuiStateCache> {
    let path = state_file_path();

    let contents = std::fs::read_to_string(&path).ok()?;
    let cache: TuiStateCache = serde_json::from_str(&contents).ok()?;

    // Version mismatch - ignore cache
    if cache.version != env!("CARGO_PKG_VERSION") {
        return None;
    }

    Some(cache)
}

/// Save the TUI state to disk.
/// Silently ignores any errors (state persistence is best-effort).
fn save_state(state: &TuiStateCache) {
    let path = state_file_path();

    // Write atomically via temp file
    let tmp_path = path.with_extension("tmp");
    if let Ok(contents) = serde_json::to_string_pretty(state) {
        if std::fs::write(&tmp_path, contents).is_ok() {
            let _ = std::fs::rename(&tmp_path, &path);
        }
    }
}

/// Build a cache from current instance state
fn build_cache(instances: &[InstanceInfo]) -> TuiStateCache {
    let now = chrono::Utc::now().timestamp();
    let entries: HashMap<String, CachedInstanceState> = instances
        .iter()
        .filter_map(|i| {
            // Only cache instances that have some fetched state
            if i.git_state.is_some() || i.agent_state != AgentState::default() {
                Some((
                    i.name.clone(),
                    CachedInstanceState {
                        git_state: i.git_state.clone(),
                        agent_state: Some(i.agent_state.clone()),
                        updated_at: now,
                    },
                ))
            } else {
                None
            }
        })
        .collect();

    TuiStateCache {
        version: env!("CARGO_PKG_VERSION").to_string(),
        instances: entries,
    }
}

/// Prefix for all devaipod pod names
const POD_NAME_PREFIX: &str = "devaipod-";

/// Minimum interval between git state refreshes for a single instance.
/// Prevents excessive git command execution when multiple refresh triggers occur.
const GIT_REFRESH_RATE_LIMIT: Duration = Duration::from_secs(10);

/// Agent activity state (idle, working, etc.)
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AgentState {
    /// Agent is running but waiting for input
    Idle,
    /// Agent is actively processing a task
    Working,
    /// Agent container is not running
    #[default]
    Stopped,
    /// Could not determine agent state (API error, etc.)
    Unknown,
}

/// Git repository state
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct GitState {
    /// Current branch name
    pub branch: Option<String>,
    /// Whether there are uncommitted changes
    pub dirty: bool,
    /// Number of commits ahead of remote
    pub ahead: u32,
    /// Number of commits behind remote
    pub behind: u32,
    /// Short summary (e.g., "main ✓" or "feature-x *+2-1")
    pub summary: String,
}

impl GitState {
    /// Create a summary string for display
    fn compute_summary(&mut self) {
        let branch = self.branch.as_deref().unwrap_or("detached");
        let dirty_indicator = if self.dirty { "*" } else { "" };

        let ahead_behind = match (self.ahead, self.behind) {
            (0, 0) => String::new(),
            (a, 0) => format!(" ↑{}", a),
            (0, b) => format!(" ↓{}", b),
            (a, b) => format!(" ↑{}↓{}", a, b),
        };

        self.summary = format!("{}{}{}", branch, dirty_indicator, ahead_behind);
    }
}

/// Information about a devaipod instance gathered from bollard
#[derive(Debug, Clone)]
pub struct InstanceInfo {
    /// Short name (without devaipod- prefix)
    pub name: String,
    /// Full pod name
    pub full_name: String,
    /// Pod status (Running, Exited, Degraded)
    pub status: String,
    /// Repository URL from labels
    pub repo: Option<String>,
    /// Current task description from labels
    pub task: Option<String>,
    /// Mode (up, run, etc.)
    pub mode: Option<String>,
    /// Whether the agent is healthy
    pub agent_healthy: Option<bool>,
    /// Created timestamp (formatted for display)
    pub created: Option<String>,
    /// Raw creation timestamp (Unix seconds) for sorting
    pub created_ts: Option<i64>,
    /// Git repository state (fetched async)
    pub git_state: Option<GitState>,
    /// Workspace directory path inside container
    pub workspace_path: Option<String>,
    /// Last time git state was refreshed for this instance (for rate-limiting)
    pub last_git_refresh: Option<std::time::Instant>,
    /// Agent activity state (fetched async)
    pub agent_state: AgentState,
    /// Last time agent state was refreshed for this instance
    pub last_agent_refresh: Option<std::time::Instant>,
    /// API password for the opencode server (from pod labels)
    pub api_password: Option<String>,
    /// Published host port for the opencode API
    pub api_port: Option<u16>,
}

/// Mode of the TUI (normal browsing vs delete selection)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TuiMode {
    /// Normal browsing mode
    #[default]
    Normal,
    /// Delete selection mode - allows selecting multiple instances
    DeleteSelect,
    /// Confirming deletion of selected instances
    DeleteConfirm,
}

/// Action to perform after exiting TUI
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Just quit, no further action
    Quit,
    /// Attach to the specified instance (opens tmux with agent + shell)
    Attach(String),
    /// Trigger a refresh
    Refresh,
    /// Delete selected instances
    Delete(Vec<String>),
}

/// Application state for the TUI
pub struct App {
    /// Docker/Podman client
    docker: Docker,
    /// List of instances
    instances: Vec<InstanceInfo>,
    /// Table selection state
    table_state: TableState,
    /// Last refresh time
    last_refresh: std::time::Instant,
    /// Status message
    status_message: Option<String>,
    /// Cached TUI state (loaded on startup, updated on refreshes)
    cache: Option<TuiStateCache>,
    /// Current TUI mode (normal, delete select, etc.)
    mode: TuiMode,
    /// Instances selected for deletion (by name)
    selected_for_delete: std::collections::HashSet<String>,
}

impl App {
    /// Create a new App instance
    pub async fn new() -> Result<Self> {
        // Connect to podman socket using XDG_RUNTIME_DIR or uid-based path
        let uid = rustix::process::getuid().as_raw();
        let socket_path = std::env::var("XDG_RUNTIME_DIR")
            .map(|dir| format!("{}/podman/podman.sock", dir))
            .unwrap_or_else(|_| format!("/run/user/{}/podman/podman.sock", uid));

        let docker = Docker::connect_with_unix(
            &format!("unix://{}", socket_path),
            120,
            bollard::API_DEFAULT_VERSION,
        )
        .or_else(|_| {
            // Try default docker socket as fallback
            Docker::connect_with_local_defaults()
        })
        .context("Failed to connect to podman/docker")?;

        // Load cached state for instant display
        let cache = load_state();

        let mut app = Self {
            docker,
            instances: Vec::new(),
            table_state: TableState::default(),
            last_refresh: std::time::Instant::now(),
            status_message: None,
            cache,
            mode: TuiMode::Normal,
            selected_for_delete: std::collections::HashSet::new(),
        };

        // Initial data fetch
        app.refresh_instances().await?;

        Ok(app)
    }

    /// Refresh the list of instances from podman
    pub async fn refresh_instances(&mut self) -> Result<()> {
        // Preserve git state and rate-limit timestamps from existing instances
        let old_git_data: HashMap<String, (Option<GitState>, Option<std::time::Instant>)> = self
            .instances
            .iter()
            .map(|i| (i.name.clone(), (i.git_state.clone(), i.last_git_refresh)))
            .collect();

        // Preserve agent state and rate-limit timestamps
        let old_agent_data: HashMap<String, (AgentState, Option<std::time::Instant>)> = self
            .instances
            .iter()
            .map(|i| {
                (
                    i.name.clone(),
                    (i.agent_state.clone(), i.last_agent_refresh),
                )
            })
            .collect();

        let filter = format!("{}*", POD_NAME_PREFIX);
        let mut filters = HashMap::new();
        filters.insert("name", vec![filter.as_str()]);

        let options = ListContainersOptions {
            all: true,
            filters,
            ..Default::default()
        };

        let containers = self
            .docker
            .list_containers(Some(options))
            .await
            .context("Failed to list containers")?;

        // Group containers by pod (using the pod label or name prefix)
        let mut pod_containers: HashMap<String, Vec<ContainerSummary>> = HashMap::new();

        for container in containers {
            // Extract pod name from container name
            // Container names look like: /devaipod-foo-workspace, /devaipod-foo-agent
            if let Some(names) = &container.names {
                for name in names {
                    let name = name.trim_start_matches('/');
                    if name.starts_with(POD_NAME_PREFIX) {
                        // Extract the pod name (everything before -workspace, -agent, -infra)
                        let pod_name = extract_pod_name(name);
                        pod_containers
                            .entry(pod_name.to_string())
                            .or_default()
                            .push(container.clone());
                        break;
                    }
                }
            }
        }

        // Build instance info from grouped containers
        let mut instances: Vec<InstanceInfo> = Vec::new();

        for (full_name, containers) in pod_containers {
            // Skip if this doesn't have a workspace container (e.g., orphaned gator containers)
            if !is_valid_instance(&containers) {
                continue;
            }

            let short_name = full_name
                .strip_prefix(POD_NAME_PREFIX)
                .unwrap_or(&full_name)
                .to_string();

            // Find the workspace container to extract labels
            let workspace = containers.iter().find(|c| {
                c.names
                    .as_ref()
                    .is_some_and(|n| n.iter().any(|name| name.ends_with("-workspace")))
            });

            // Extract labels from workspace container
            let labels = workspace.and_then(|w| w.labels.as_ref());

            let repo = labels.and_then(|l| l.get("io.devaipod.repo")).cloned();
            let task = labels.and_then(|l| l.get("io.devaipod.task")).cloned();
            let mode = labels.and_then(|l| l.get("io.devaipod.mode")).cloned();

            // Determine overall status
            let running_count = containers
                .iter()
                .filter(|c| c.state.as_deref() == Some("running"))
                .count();
            let total = containers.len();

            let status = if running_count == total && total > 0 {
                "Running".to_string()
            } else if running_count == 0 {
                "Exited".to_string()
            } else {
                "Degraded".to_string()
            };

            // Check agent health (simplified - just check if agent container is running)
            let agent_healthy = containers.iter().any(|c| {
                c.names
                    .as_ref()
                    .is_some_and(|n| n.iter().any(|name| name.contains("-agent")))
                    && c.state.as_deref() == Some("running")
            });

            // Get created time from workspace container
            let created_ts = workspace.and_then(|w| w.created);
            let created = created_ts.map(|ts| {
                chrono::DateTime::from_timestamp(ts, 0)
                    .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "-".to_string())
            });

            // Extract workspace path from labels
            let workspace_path = labels.and_then(|l| l.get("io.devaipod.workspace")).cloned();

            // Restore git state and rate-limit timestamp from previous instance data
            // Fall back to cache if no in-memory state exists
            let (git_state, last_git_refresh) = old_git_data
                .get(&short_name)
                .cloned()
                .or_else(|| {
                    // Try loading from cache
                    self.cache
                        .as_ref()
                        .and_then(|c| c.instances.get(&short_name))
                        .map(|cached| (cached.git_state.clone(), None))
                })
                .unwrap_or((None, None));

            // Restore agent state and rate-limit timestamp from previous instance data
            // Default to Stopped if agent is not running, otherwise preserve or set Unknown
            // Fall back to cache if no in-memory state exists
            let (agent_state, last_agent_refresh) = if agent_healthy {
                old_agent_data
                    .get(&short_name)
                    .cloned()
                    .or_else(|| {
                        // Try loading from cache
                        self.cache
                            .as_ref()
                            .and_then(|c| c.instances.get(&short_name))
                            .and_then(|cached| cached.agent_state.clone().map(|s| (s, None)))
                    })
                    .unwrap_or((AgentState::Unknown, None))
            } else {
                (AgentState::Stopped, None)
            };

            // Extract API password from labels (stored on workspace container)
            let api_password = labels
                .and_then(|l| l.get("io.devaipod.api-password"))
                .cloned();

            // Get published port from agent container's port mappings
            let agent_container = containers.iter().find(|c| {
                c.names
                    .as_ref()
                    .is_some_and(|n| n.iter().any(|name| name.ends_with("-agent")))
            });
            let api_port = agent_container.and_then(|c| {
                c.ports.as_ref().and_then(|ports| {
                    ports.iter().find_map(|p| {
                        // Looking for the port mapped from container port 4096
                        if p.private_port == 4096 {
                            p.public_port
                        } else {
                            None
                        }
                    })
                })
            });

            instances.push(InstanceInfo {
                name: short_name,
                full_name,
                status,
                repo,
                task,
                mode,
                agent_healthy: Some(agent_healthy),
                created,
                created_ts,
                git_state,
                workspace_path,
                last_git_refresh,
                agent_state,
                last_agent_refresh,
                api_password,
                api_port,
            });
        }

        // Sort by creation time (newest first), with fallback to name for ties
        instances.sort_by(|a, b| match (b.created_ts, a.created_ts) {
            (Some(b_ts), Some(a_ts)) => b_ts.cmp(&a_ts),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.name.cmp(&b.name),
        });

        self.instances = instances;
        self.last_refresh = std::time::Instant::now();

        // NOTE: Git state is fetched separately via refresh_git_states_background()
        // to avoid blocking the initial display

        // Ensure selection is valid and within bounds
        if let Some(selected) = self.table_state.selected() {
            if selected >= self.instances.len() {
                self.table_state.select(if self.instances.is_empty() {
                    None
                } else {
                    Some(self.instances.len() - 1)
                });
            }
        } else if !self.instances.is_empty() {
            self.table_state.select(Some(0));
        }

        Ok(())
    }

    /// Move selection up
    pub fn previous(&mut self) {
        if self.instances.is_empty() {
            return;
        }
        let i = match self.table_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.instances.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    /// Move selection down
    pub fn next(&mut self) {
        if self.instances.is_empty() {
            return;
        }
        let i = match self.table_state.selected() {
            Some(i) => {
                if i >= self.instances.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    /// Get the currently selected instance
    pub fn selected_instance(&self) -> Option<&InstanceInfo> {
        self.table_state
            .selected()
            .and_then(|i| self.instances.get(i))
    }

    /// Update and persist the cache with current instance state
    fn update_cache(&mut self) {
        let new_cache = build_cache(&self.instances);
        save_state(&new_cache);
        self.cache = Some(new_cache);
    }
}

/// Fetch git state from a container by exec'ing git commands
async fn fetch_git_state(
    docker: &Docker,
    container_name: &str,
    workspace_path: &str,
) -> Option<GitState> {
    use bollard::exec::{CreateExecOptions, StartExecResults};
    use futures_util::TryStreamExt;

    // Helper to run a git command and get stdout
    async fn git_exec(
        docker: &Docker,
        container: &str,
        workdir: &str,
        args: &[&str],
    ) -> Option<String> {
        let mut cmd = vec!["git", "-C", workdir];
        cmd.extend(args);

        let exec = docker
            .create_exec(
                container,
                CreateExecOptions {
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    cmd: Some(cmd.iter().map(|s| s.to_string()).collect()),
                    ..Default::default()
                },
            )
            .await
            .ok()?;

        let output = docker.start_exec(&exec.id, None).await.ok()?;

        if let StartExecResults::Attached { mut output, .. } = output {
            let mut stdout = String::new();
            while let Ok(Some(msg)) = output.try_next().await {
                stdout.push_str(&msg.to_string());
            }
            Some(stdout.trim().to_string())
        } else {
            None
        }
    }

    // Find the actual repo directory
    // First, try to find directories in /workspaces
    let repo_dir = {
        // List directories in /workspaces
        let ls_exec = docker
            .create_exec(
                container_name,
                CreateExecOptions {
                    attach_stdout: Some(true),
                    cmd: Some(vec![
                        "ls".to_string(),
                        "-1".to_string(),
                        "/workspaces".to_string(),
                    ]),
                    ..Default::default()
                },
            )
            .await
            .ok();

        let dirs: Vec<String> = if let Some(exec) = ls_exec {
            if let Ok(StartExecResults::Attached { mut output, .. }) =
                docker.start_exec(&exec.id, None).await
            {
                let mut stdout = String::new();
                while let Ok(Some(msg)) = output.try_next().await {
                    stdout.push_str(&msg.to_string());
                }
                stdout
                    .lines()
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect()
            } else {
                vec![]
            }
        } else {
            vec![]
        };

        // Use the first directory found, or fall back to workspace_path
        if let Some(first_dir) = dirs.first() {
            format!("/workspaces/{}", first_dir)
        } else if workspace_path != "/workspaces" {
            workspace_path.to_string()
        } else {
            return None; // No workspace found
        }
    };

    // Get current branch
    let branch = git_exec(
        docker,
        container_name,
        &repo_dir,
        &["rev-parse", "--abbrev-ref", "HEAD"],
    )
    .await
    .filter(|s| !s.is_empty() && s != "HEAD");

    // Check for uncommitted changes (dirty state)
    let status_output = git_exec(
        docker,
        container_name,
        &repo_dir,
        &["status", "--porcelain"],
    )
    .await;
    let dirty = status_output.as_ref().is_some_and(|s| !s.is_empty());

    // Get ahead/behind counts
    let (ahead, behind) = if let Some(ref branch_name) = branch {
        let upstream = format!("origin/{}", branch_name);
        let rev_list = git_exec(
            docker,
            container_name,
            &repo_dir,
            &[
                "rev-list",
                "--left-right",
                "--count",
                &format!("{}...{}", branch_name, upstream),
            ],
        )
        .await;

        if let Some(counts) = rev_list {
            let parts: Vec<&str> = counts.split_whitespace().collect();
            if parts.len() == 2 {
                let a = parts[0].parse().unwrap_or(0);
                let b = parts[1].parse().unwrap_or(0);
                (a, b)
            } else {
                (0, 0)
            }
        } else {
            (0, 0)
        }
    } else {
        (0, 0)
    };

    let mut state = GitState {
        branch,
        dirty,
        ahead,
        behind,
        summary: String::new(),
    };
    state.compute_summary();

    Some(state)
}

/// Minimum interval between agent state refreshes for a single instance.
const AGENT_REFRESH_RATE_LIMIT: Duration = Duration::from_secs(3);

/// Derive agent status (busy/idle) from session messages.
///
/// This mirrors the logic from workspace_monitor.py's derive_status_from_messages().
/// We check the last assistant message for:
/// - time.completed: if absent, agent is still processing
/// - finish: if "tool-calls", agent will continue (but may be between calls)
/// - parts with type="tool" and state.status != "completed": tool in progress
fn derive_agent_state_from_messages(messages: &[serde_json::Value]) -> AgentState {
    if messages.is_empty() {
        return AgentState::Unknown;
    }

    // Find the last assistant message
    let last_assistant = messages.iter().rev().find(|msg| {
        msg.get("info")
            .and_then(|i| i.get("role"))
            .and_then(|r| r.as_str())
            == Some("assistant")
    });

    let Some(last_assistant) = last_assistant else {
        return AgentState::Unknown;
    };

    let info = match last_assistant.get("info") {
        Some(i) => i,
        None => return AgentState::Unknown,
    };

    // Check if message is still being processed (no completed time)
    if info.get("time").and_then(|t| t.get("completed")).is_none() {
        return AgentState::Working;
    }

    // Check if there are any incomplete tool calls in parts
    if let Some(parts) = last_assistant.get("parts").and_then(|p| p.as_array()) {
        for part in parts {
            if part.get("type").and_then(|t| t.as_str()) == Some("tool") {
                let status = part
                    .get("state")
                    .and_then(|s| s.get("status"))
                    .and_then(|s| s.as_str());
                if status != Some("completed") && status != Some("error") {
                    return AgentState::Working;
                }
            }
        }
    }

    // Message completed - check finish reason
    let finish = info.get("finish").and_then(|f| f.as_str()).unwrap_or("");
    if finish == "stop" {
        AgentState::Idle
    } else if finish == "tool-calls" {
        // Agent made tool calls but those are done; waiting for next turn
        // This is a brief transitional state
        AgentState::Working
    } else {
        AgentState::Idle
    }
}

/// Fetch agent state by querying the opencode API
async fn fetch_agent_state(api_port: u16, api_password: &str) -> AgentState {
    // Build HTTP client with timeout
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return AgentState::Unknown,
    };

    let base_url = format!("http://127.0.0.1:{}", api_port);

    // First, get the list of sessions
    let sessions_url = format!("{}/session", base_url);
    let sessions_resp = match client
        .get(&sessions_url)
        .basic_auth("opencode", Some(api_password))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        _ => return AgentState::Unknown,
    };

    let sessions: Vec<serde_json::Value> = match sessions_resp.json().await {
        Ok(s) => s,
        Err(_) => return AgentState::Unknown,
    };

    if sessions.is_empty() {
        // No sessions yet - agent is idle/waiting for input
        return AgentState::Idle;
    }

    // Find the root session (no parent)
    let root_session = sessions.iter().find(|s| {
        s.get("parentID").is_none() || s.get("parentID").map(|p| p.is_null()).unwrap_or(false)
    });

    let Some(root_session) = root_session else {
        return AgentState::Unknown;
    };

    let session_id = match root_session.get("id").and_then(|id| id.as_str()) {
        Some(id) => id,
        None => return AgentState::Unknown,
    };

    // Fetch recent messages from the session
    let messages_url = format!("{}/session/{}/message?limit=3", base_url, session_id);
    let messages_resp = match client
        .get(&messages_url)
        .basic_auth("opencode", Some(api_password))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        _ => return AgentState::Unknown,
    };

    let messages: Vec<serde_json::Value> = match messages_resp.json().await {
        Ok(m) => m,
        Err(_) => return AgentState::Unknown,
    };

    derive_agent_state_from_messages(&messages)
}

/// Extract the pod name from a container name
/// e.g., "devaipod-foo-workspace" -> "devaipod-foo"
fn extract_pod_name(container_name: &str) -> &str {
    // Order matters - check longer suffixes first
    for suffix in &[
        "-service-gator",
        "-workspace",
        "-agent",
        "-infra",
        "-gator",
        "-proxy",
    ] {
        if let Some(prefix) = container_name.strip_suffix(suffix) {
            return prefix;
        }
    }
    container_name
}

/// Check if this is a valid devaipod instance (has workspace container)
fn is_valid_instance(containers: &[ContainerSummary]) -> bool {
    containers.iter().any(|c| {
        c.names
            .as_ref()
            .is_some_and(|n| n.iter().any(|name| name.ends_with("-workspace")))
    })
}

/// Run the TUI application
pub async fn run() -> Result<()> {
    // Check if we're running in a terminal
    if !std::io::stdout().is_terminal() {
        color_eyre::eyre::bail!(
            "TUI requires a terminal. Use 'devaipod list' for non-interactive output."
        );
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app and run
    let app = App::new().await?;
    let result = run_app(&mut terminal, app).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

/// Message for git state updates from background task
struct GitStateUpdate {
    instance_name: String,
    git_state: Option<GitState>,
}

/// Spawn background tasks to fetch git state for all running instances.
/// Rate-limited per instance: only spawns a refresh if >GIT_REFRESH_RATE_LIMIT
/// has passed since the last successful refresh for that instance.
fn spawn_git_refresh(
    docker: &Docker,
    instances: &[InstanceInfo],
    tx: mpsc::Sender<GitStateUpdate>,
) {
    let now = std::time::Instant::now();

    for instance in instances {
        if instance.status != "Running" {
            continue;
        }

        // Rate-limit: skip if we refreshed this instance recently
        if let Some(last_refresh) = instance.last_git_refresh {
            if now.duration_since(last_refresh) < GIT_REFRESH_RATE_LIMIT {
                continue;
            }
        }

        let docker = docker.clone();
        let tx = tx.clone();
        let instance_name = instance.name.clone();
        let full_name = instance.full_name.clone();
        let workspace_path = instance
            .workspace_path
            .clone()
            .unwrap_or_else(|| "/workspaces".to_string());

        tokio::spawn(async move {
            let container_name = format!("{}-workspace", full_name);
            let git_state = fetch_git_state(&docker, &container_name, &workspace_path).await;
            let _ = tx
                .send(GitStateUpdate {
                    instance_name,
                    git_state,
                })
                .await;
        });
    }
}

/// Message for agent state updates from background task
struct AgentStateUpdate {
    instance_name: String,
    agent_state: AgentState,
}

/// Spawn background tasks to fetch agent state for all running instances.
/// Rate-limited per instance: only spawns a refresh if >AGENT_REFRESH_RATE_LIMIT
/// has passed since the last successful refresh for that instance.
fn spawn_agent_refresh(instances: &[InstanceInfo], tx: mpsc::Sender<AgentStateUpdate>) {
    let now = std::time::Instant::now();

    for instance in instances {
        // Only fetch for running instances with valid API credentials
        if instance.status != "Running" {
            continue;
        }

        // Skip if agent is not healthy (not running)
        if instance.agent_healthy != Some(true) {
            continue;
        }

        // Need both port and password to query the API
        let (Some(api_port), Some(ref api_password)) = (instance.api_port, &instance.api_password)
        else {
            continue;
        };

        // Rate-limit: skip if we refreshed this instance recently
        if let Some(last_refresh) = instance.last_agent_refresh {
            if now.duration_since(last_refresh) < AGENT_REFRESH_RATE_LIMIT {
                continue;
            }
        }

        let tx = tx.clone();
        let instance_name = instance.name.clone();
        let api_password = api_password.clone();

        tokio::spawn(async move {
            let agent_state = fetch_agent_state(api_port, &api_password).await;
            let _ = tx
                .send(AgentStateUpdate {
                    instance_name,
                    agent_state,
                })
                .await;
        });
    }
}

/// Main event loop
async fn run_app(terminal: &mut Terminal<CrosstermBackend<Stdout>>, mut app: App) -> Result<()> {
    let mut refresh_interval = interval(Duration::from_secs(10));

    // Channel for receiving git state updates from background tasks
    let (git_tx, mut git_rx) = mpsc::channel::<GitStateUpdate>(32);

    // Channel for receiving agent state updates from background tasks
    let (agent_tx, mut agent_rx) = mpsc::channel::<AgentStateUpdate>(32);

    // Spawn initial git state fetch
    spawn_git_refresh(&app.docker, &app.instances, git_tx.clone());

    // Spawn initial agent state fetch
    spawn_agent_refresh(&app.instances, agent_tx.clone());

    // Use async event stream instead of blocking poll
    let mut event_stream = EventStream::new();

    // Agent state refresh runs more frequently than git state
    let mut agent_refresh_interval = interval(Duration::from_secs(3));

    loop {
        // Draw the UI
        terminal.draw(|f| ui(f, &mut app))?;

        // Handle events with proper async - no blocking!
        tokio::select! {
            // Receive git state updates from background
            Some(update) = git_rx.recv() => {
                if let Some(instance) = app.instances.iter_mut().find(|i| i.name == update.instance_name) {
                    instance.git_state = update.git_state;
                    // Update timestamp to enforce rate-limiting on subsequent refresh attempts
                    instance.last_git_refresh = Some(std::time::Instant::now());
                }
                // Persist updated state to cache
                app.update_cache();
            }
            // Receive agent state updates from background
            Some(update) = agent_rx.recv() => {
                if let Some(instance) = app.instances.iter_mut().find(|i| i.name == update.instance_name) {
                    instance.agent_state = update.agent_state;
                    // Update timestamp to enforce rate-limiting on subsequent refresh attempts
                    instance.last_agent_refresh = Some(std::time::Instant::now());
                }
                // Persist updated state to cache
                app.update_cache();
            }
            // Periodic agent state refresh (more frequent)
            _ = agent_refresh_interval.tick() => {
                spawn_agent_refresh(&app.instances, agent_tx.clone());
            }
            _ = refresh_interval.tick() => {
                if let Err(e) = app.refresh_instances().await {
                    app.status_message = Some(format!("Refresh error: {}", e));
                } else {
                    // Spawn background git refresh after instance refresh
                    spawn_git_refresh(&app.docker, &app.instances, git_tx.clone());
                    // Also refresh agent state
                    spawn_agent_refresh(&app.instances, agent_tx.clone());
                }
            }
            // Async event stream - truly non-blocking
            maybe_event = event_stream.next() => {
                if let Some(Ok(event)) = maybe_event {
                    if let Some(action) = handle_event(&mut app, event) {
                        match action {
                            Action::Quit => return Ok(()),
                            Action::Refresh => {
                                app.status_message = Some("Refreshing...".to_string());
                                if let Err(e) = app.refresh_instances().await {
                                    app.status_message = Some(format!("Refresh error: {}", e));
                                } else {
                                    spawn_git_refresh(&app.docker, &app.instances, git_tx.clone());
                                    spawn_agent_refresh(&app.instances, agent_tx.clone());
                                    app.status_message = Some("Refreshed".to_string());
                                }
                            }
                            Action::Attach(name) => {
                                // Run attach in subprocess (opens tmux with agent + shell)
                                run_subprocess(terminal, &["attach", &name]).await?;
                                // Refresh after returning from subprocess
                                let _ = app.refresh_instances().await;
                                spawn_git_refresh(&app.docker, &app.instances, git_tx.clone());
                                spawn_agent_refresh(&app.instances, agent_tx.clone());
                            }
                            Action::Delete(names) => {
                                // Delete instances synchronously for now
                                let count = names.len();
                                app.status_message = Some(format!(
                                    "Deleting {} instance{}...",
                                    count,
                                    if count == 1 { "" } else { "s" }
                                ));
                                terminal.draw(|f| ui(f, &mut app))?;

                                let mut errors = Vec::new();
                                for name in &names {
                                    if let Err(e) = run_subprocess_silent(&["delete", "--force", name]).await {
                                        errors.push(format!("{}: {}", name, e));
                                    }
                                }

                                // Refresh after deletions
                                let _ = app.refresh_instances().await;
                                spawn_git_refresh(&app.docker, &app.instances, git_tx.clone());
                                spawn_agent_refresh(&app.instances, agent_tx.clone());

                                if errors.is_empty() {
                                    app.status_message = Some(format!(
                                        "Deleted {} instance{}",
                                        count,
                                        if count == 1 { "" } else { "s" }
                                    ));
                                } else {
                                    app.status_message = Some(format!("Errors: {}", errors.join(", ")));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Handle a terminal event, returning an action if the TUI should exit
fn handle_event(app: &mut App, event: Event) -> Option<Action> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => match app.mode {
            TuiMode::Normal => handle_normal_mode(app, key.code),
            TuiMode::DeleteSelect => handle_delete_select_mode(app, key.code),
            TuiMode::DeleteConfirm => handle_delete_confirm_mode(app, key.code),
        },
        _ => None,
    }
}

/// Handle key events in normal mode
fn handle_normal_mode(app: &mut App, code: KeyCode) -> Option<Action> {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
        KeyCode::Char('j') | KeyCode::Down => {
            app.next();
            None
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.previous();
            None
        }
        KeyCode::Char('r') => Some(Action::Refresh),
        KeyCode::Enter | KeyCode::Char('a') => {
            if let Some(instance) = app.selected_instance() {
                Some(Action::Attach(instance.name.clone()))
            } else {
                app.status_message = Some("No instance selected".to_string());
                None
            }
        }
        KeyCode::Char('d') => {
            // Enter delete select mode
            app.mode = TuiMode::DeleteSelect;
            app.selected_for_delete.clear();
            app.status_message =
                Some("Delete mode: Space to select, Enter to confirm, Esc to cancel".to_string());
            None
        }
        _ => None,
    }
}

/// Handle key events in delete select mode
fn handle_delete_select_mode(app: &mut App, code: KeyCode) -> Option<Action> {
    match code {
        KeyCode::Esc | KeyCode::Char('q') => {
            // Cancel delete mode
            app.mode = TuiMode::Normal;
            app.selected_for_delete.clear();
            app.status_message = None;
            None
        }
        KeyCode::Char('j') | KeyCode::Down => {
            app.next();
            None
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.previous();
            None
        }
        KeyCode::Char(' ') => {
            // Toggle selection of current instance
            if let Some(instance) = app.selected_instance() {
                let name = instance.name.clone();
                if app.selected_for_delete.contains(&name) {
                    app.selected_for_delete.remove(&name);
                } else {
                    app.selected_for_delete.insert(name);
                }
                let count = app.selected_for_delete.len();
                app.status_message = Some(format!(
                    "Delete mode: {} selected. Space to toggle, Enter to confirm, Esc to cancel",
                    count
                ));
            }
            None
        }
        KeyCode::Enter => {
            if app.selected_for_delete.is_empty() {
                app.status_message = Some("No instances selected for deletion".to_string());
                None
            } else {
                // Enter confirmation mode
                app.mode = TuiMode::DeleteConfirm;
                let count = app.selected_for_delete.len();
                app.status_message = Some(format!(
                    "Delete {} instance{}? y to confirm, n/Esc to cancel",
                    count,
                    if count == 1 { "" } else { "s" }
                ));
                None
            }
        }
        _ => None,
    }
}

/// Handle key events in delete confirm mode
fn handle_delete_confirm_mode(app: &mut App, code: KeyCode) -> Option<Action> {
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            // Confirm deletion
            let names: Vec<String> = app.selected_for_delete.drain().collect();
            app.mode = TuiMode::Normal;
            Some(Action::Delete(names))
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            // Cancel - go back to delete select mode
            app.mode = TuiMode::DeleteSelect;
            let count = app.selected_for_delete.len();
            app.status_message = Some(format!(
                "Delete mode: {} selected. Space to toggle, Enter to confirm, Esc to cancel",
                count
            ));
            None
        }
        _ => None,
    }
}

/// Run a subprocess with terminal properly suspended, then resume TUI
async fn run_subprocess(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    args: &[&str],
) -> Result<()> {
    use std::process::Stdio;
    use tokio::process::Command;

    // Restore terminal for the subprocess
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    // Get the path to ourselves
    let exe = std::env::current_exe().context("Failed to get current executable")?;

    // Spawn subprocess and wait
    let status = Command::new(&exe)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .context("Failed to run subprocess")?;

    if !status.success() {
        tracing::warn!("Subprocess exited with status: {:?}", status.code());
    }

    // Restore TUI terminal state
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    terminal.hide_cursor()?;
    terminal.clear()?;

    Ok(())
}

/// Run a subprocess silently (no terminal restore needed), capturing stderr for errors
async fn run_subprocess_silent(args: &[&str]) -> Result<()> {
    use std::process::Stdio;
    use tokio::process::Command;

    let exe = std::env::current_exe().context("Failed to get current executable")?;

    let output = Command::new(&exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("Failed to run subprocess")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = stderr.trim();
        if msg.is_empty() {
            color_eyre::eyre::bail!("exit code {:?}", output.status.code());
        } else {
            color_eyre::eyre::bail!("{}", msg);
        }
    }

    Ok(())
}

/// Render the UI
fn ui(frame: &mut ratatui::Frame, app: &mut App) {
    let area = frame.area();

    // Layout: header, main table, footer
    let chunks = Layout::vertical([
        Constraint::Length(3), // Header
        Constraint::Min(5),    // Table
        Constraint::Length(3), // Footer
    ])
    .split(area);

    // Header
    let header = Paragraph::new(Line::from(vec![
        Span::styled(" devaipod ", Style::default().fg(Color::Cyan).bold()),
        Span::raw("│ "),
        Span::styled(
            format!("{} instances", app.instances.len()),
            Style::default().fg(Color::Green),
        ),
        Span::raw(" │ Last refresh: "),
        Span::styled(
            format!("{}s ago", app.last_refresh.elapsed().as_secs()),
            Style::default().fg(Color::Yellow),
        ),
    ]))
    .block(Block::default().borders(Borders::ALL).title(" Dashboard "));
    frame.render_widget(header, chunks[0]);

    // Instance table
    render_table(frame, app, chunks[1]);

    // Footer with help and status
    let status = app
        .status_message
        .as_deref()
        .map(|s| format!(" │ {}", s))
        .unwrap_or_default();

    // Footer with help and status (mode-dependent)
    let (help_base, footer_style) = match app.mode {
        TuiMode::Normal => (
            " q: Quit │ j/k: Navigate │ Enter/a: Attach (tmux) │ d: Delete │ r: Refresh",
            Style::default().fg(Color::DarkGray),
        ),
        TuiMode::DeleteSelect => (
            " Esc: Cancel │ j/k: Navigate │ Space: Toggle selection │ Enter: Confirm delete",
            Style::default().fg(Color::Yellow),
        ),
        TuiMode::DeleteConfirm => (
            " y: Confirm delete │ n/Esc: Cancel",
            Style::default().fg(Color::Red),
        ),
    };
    let help_text = format!("{}{}", help_base, status);
    let footer = Paragraph::new(help_text)
        .style(footer_style)
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, chunks[2]);
}

/// Render the instances table
fn render_table(frame: &mut ratatui::Frame, app: &mut App, area: Rect) {
    // In delete mode, add a selection column
    let in_delete_mode = matches!(app.mode, TuiMode::DeleteSelect | TuiMode::DeleteConfirm);

    let header_labels: Vec<&str> = if in_delete_mode {
        vec![
            "SEL", "NAME", "STATUS", "AGENT", "CREATED", "GIT", "MODE", "REPO", "TASK",
        ]
    } else {
        vec![
            "NAME", "STATUS", "AGENT", "CREATED", "GIT", "MODE", "REPO", "TASK",
        ]
    };
    let header_cells = header_labels
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().fg(Color::Yellow).bold()));
    let header = Row::new(header_cells).height(1);

    let selected_for_delete = &app.selected_for_delete;
    let rows = app.instances.iter().map(|instance| {
        let is_selected = selected_for_delete.contains(&instance.name);
        // Status with color
        let status_style = match instance.status.as_str() {
            "Running" => Style::default().fg(Color::Green),
            "Exited" => Style::default().fg(Color::Red),
            "Degraded" => Style::default().fg(Color::Yellow),
            _ => Style::default(),
        };

        let status_text = instance.status.clone();

        // Agent state with color coding
        let (agent_text, agent_style) = match &instance.agent_state {
            AgentState::Working => ("working", Style::default().fg(Color::Green)),
            AgentState::Idle => ("idle", Style::default().fg(Color::Blue)),
            AgentState::Stopped => ("stopped", Style::default().fg(Color::DarkGray)),
            AgentState::Unknown => {
                if instance.status == "Running" && instance.agent_healthy == Some(true) {
                    // Still loading
                    ("...", Style::default().fg(Color::DarkGray))
                } else {
                    ("-", Style::default().fg(Color::DarkGray))
                }
            }
        };

        // Git state with color coding
        let (git_text, git_style) = match &instance.git_state {
            Some(state) => {
                let style = if state.dirty {
                    Style::default().fg(Color::Yellow)
                } else if state.ahead > 0 || state.behind > 0 {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default().fg(Color::Green)
                };
                (state.summary.clone(), style)
            }
            None if instance.status == "Running" => {
                // Still loading
                ("...".to_string(), Style::default().fg(Color::DarkGray))
            }
            None => ("-".to_string(), Style::default().fg(Color::DarkGray)),
        };

        // Truncate task to fit
        let task = instance
            .task
            .as_deref()
            .unwrap_or("-")
            .chars()
            .take(25)
            .collect::<String>();

        let task = if instance.task.as_ref().is_some_and(|t| t.len() > 25) {
            format!("{}...", task)
        } else {
            task
        };

        // Truncate repo (remove common prefixes)
        let repo = instance
            .repo
            .as_deref()
            .map(|r| {
                r.strip_prefix("https://")
                    .or_else(|| r.strip_prefix("git@"))
                    .unwrap_or(r)
            })
            .unwrap_or("-");

        let mut cells = Vec::new();

        // Selection column in delete mode
        if in_delete_mode {
            let sel_text = if is_selected { "[x]" } else { "[ ]" };
            let sel_style = if is_selected {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            cells.push(Cell::from(sel_text).style(sel_style));
        }

        cells.extend(vec![
            Cell::from(instance.name.clone()),
            Cell::from(status_text).style(status_style),
            Cell::from(agent_text).style(agent_style),
            Cell::from(instance.created.as_deref().unwrap_or("-")),
            Cell::from(git_text).style(git_style),
            Cell::from(instance.mode.as_deref().unwrap_or("-")),
            Cell::from(repo.to_string()),
            Cell::from(task),
        ]);

        Row::new(cells)
    });

    let constraints: Vec<Constraint> = if in_delete_mode {
        vec![
            Constraint::Length(4),      // SEL
            Constraint::Min(14),        // NAME
            Constraint::Length(10),     // STATUS
            Constraint::Length(8),      // AGENT
            Constraint::Length(16),     // CREATED (YYYY-MM-DD HH:MM)
            Constraint::Length(18),     // GIT
            Constraint::Length(5),      // MODE
            Constraint::Percentage(12), // REPO
            Constraint::Percentage(16), // TASK
        ]
    } else {
        vec![
            Constraint::Min(16),        // NAME
            Constraint::Length(10),     // STATUS
            Constraint::Length(8),      // AGENT
            Constraint::Length(16),     // CREATED (YYYY-MM-DD HH:MM)
            Constraint::Length(18),     // GIT
            Constraint::Length(5),      // MODE
            Constraint::Percentage(14), // REPO
            Constraint::Percentage(18), // TASK
        ]
    };
    let table = Table::new(rows, constraints)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(" Instances "))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(table, area, &mut app.table_state);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyEventState, KeyModifiers};

    /// Create a test app for UI testing (no Docker connection needed)
    fn create_test_app_for_ui() -> TestApp {
        TestApp {
            instances: Vec::new(),
            table_state: TableState::default(),
            last_refresh: std::time::Instant::now(),
            status_message: None,
        }
    }

    /// Minimal app struct for testing without Docker
    struct TestApp {
        instances: Vec<InstanceInfo>,
        table_state: TableState,
        #[allow(dead_code)]
        last_refresh: std::time::Instant,
        status_message: Option<String>,
    }

    impl TestApp {
        fn next(&mut self) {
            if self.instances.is_empty() {
                return;
            }
            let i = match self.table_state.selected() {
                Some(i) => {
                    if i >= self.instances.len() - 1 {
                        0
                    } else {
                        i + 1
                    }
                }
                None => 0,
            };
            self.table_state.select(Some(i));
        }

        fn previous(&mut self) {
            if self.instances.is_empty() {
                return;
            }
            let i = match self.table_state.selected() {
                Some(i) => {
                    if i == 0 {
                        self.instances.len() - 1
                    } else {
                        i - 1
                    }
                }
                None => 0,
            };
            self.table_state.select(Some(i));
        }

        fn selected_instance(&self) -> Option<&InstanceInfo> {
            self.table_state
                .selected()
                .and_then(|i| self.instances.get(i))
        }
    }

    /// Create a key event for testing
    fn key_event(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    }

    /// Handle event for TestApp (mirrors real handle_event)
    fn handle_test_event(app: &mut TestApp, event: Event) -> Option<Action> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
                KeyCode::Char('j') | KeyCode::Down => {
                    app.next();
                    None
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    app.previous();
                    None
                }
                KeyCode::Enter | KeyCode::Char('a') => {
                    if let Some(instance) = app.selected_instance() {
                        Some(Action::Attach(instance.name.clone()))
                    } else {
                        app.status_message = Some("No instance selected".to_string());
                        None
                    }
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// Create sample instances for testing
    fn sample_instances() -> Vec<InstanceInfo> {
        vec![
            InstanceInfo {
                name: "myproject-abc123".to_string(),
                full_name: "devaipod-myproject-abc123".to_string(),
                status: "Running".to_string(),
                repo: Some("github.com/user/myproject".to_string()),
                task: Some("Implement new feature".to_string()),
                mode: Some("up".to_string()),
                agent_healthy: Some(true),
                created: Some("2024-01-15 10:30".to_string()),
                created_ts: Some(1705315800), // 2024-01-15 10:30
                git_state: Some(GitState {
                    branch: Some("main".to_string()),
                    dirty: false,
                    ahead: 0,
                    behind: 0,
                    summary: "main".to_string(),
                }),
                workspace_path: Some("/workspaces/myproject".to_string()),
                last_git_refresh: None,
                agent_state: AgentState::Working,
                last_agent_refresh: None,
                api_password: None,
                api_port: None,
            },
            InstanceInfo {
                name: "otherrepo-def456".to_string(),
                full_name: "devaipod-otherrepo-def456".to_string(),
                status: "Exited".to_string(),
                repo: Some("github.com/org/otherrepo".to_string()),
                task: None,
                mode: Some("run".to_string()),
                agent_healthy: Some(false),
                created: Some("2024-01-14 14:00".to_string()),
                created_ts: Some(1705240800), // 2024-01-14 14:00
                git_state: None,
                workspace_path: Some("/workspaces/otherrepo".to_string()),
                last_git_refresh: None,
                agent_state: AgentState::Stopped,
                last_agent_refresh: None,
                api_password: None,
                api_port: None,
            },
            InstanceInfo {
                name: "degraded-pod".to_string(),
                full_name: "devaipod-degraded-pod".to_string(),
                status: "Degraded".to_string(),
                repo: None,
                task: Some("Fix bug in authentication".to_string()),
                mode: None,
                agent_healthy: None,
                created: None,
                created_ts: None,
                git_state: Some(GitState {
                    branch: Some("feature-x".to_string()),
                    dirty: true,
                    ahead: 2,
                    behind: 1,
                    summary: "feature-x* ↑2↓1".to_string(),
                }),
                workspace_path: None,
                last_git_refresh: None,
                agent_state: AgentState::Idle,
                last_agent_refresh: None,
                api_password: None,
                api_port: None,
            },
        ]
    }

    #[test]
    fn test_extract_pod_name() {
        assert_eq!(extract_pod_name("devaipod-foo-workspace"), "devaipod-foo");
        assert_eq!(extract_pod_name("devaipod-foo-agent"), "devaipod-foo");
        assert_eq!(
            extract_pod_name("devaipod-foo-bar-workspace"),
            "devaipod-foo-bar"
        );
        assert_eq!(
            extract_pod_name("devaipod-foo-service-gator"),
            "devaipod-foo"
        );
        assert_eq!(extract_pod_name("unknown-container"), "unknown-container");
    }

    #[test]
    fn test_navigation_next() {
        let mut app = create_test_app_for_ui();
        app.instances = sample_instances();
        app.table_state.select(Some(0));

        app.next();
        assert_eq!(app.table_state.selected(), Some(1));

        app.next();
        assert_eq!(app.table_state.selected(), Some(2));

        // Wrap around
        app.next();
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn test_navigation_previous() {
        let mut app = create_test_app_for_ui();
        app.instances = sample_instances();
        app.table_state.select(Some(0));

        // Wrap around to end
        app.previous();
        assert_eq!(app.table_state.selected(), Some(2));

        app.previous();
        assert_eq!(app.table_state.selected(), Some(1));
    }

    #[test]
    fn test_navigation_empty_list() {
        let mut app = create_test_app_for_ui();
        app.instances = vec![];

        app.next();
        assert_eq!(app.table_state.selected(), None);

        app.previous();
        assert_eq!(app.table_state.selected(), None);
    }

    #[test]
    fn test_handle_event_quit() {
        let mut app = create_test_app_for_ui();

        let action = handle_test_event(&mut app, key_event(KeyCode::Char('q')));
        assert_eq!(action, Some(Action::Quit));

        let action = handle_test_event(&mut app, key_event(KeyCode::Esc));
        assert_eq!(action, Some(Action::Quit));
    }

    #[test]
    fn test_handle_event_navigation() {
        let mut app = create_test_app_for_ui();
        app.instances = sample_instances();
        app.table_state.select(Some(0));

        let action = handle_test_event(&mut app, key_event(KeyCode::Char('j')));
        assert_eq!(action, None);
        assert_eq!(app.table_state.selected(), Some(1));

        let action = handle_test_event(&mut app, key_event(KeyCode::Char('k')));
        assert_eq!(action, None);
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn test_handle_event_attach() {
        let mut app = create_test_app_for_ui();
        app.instances = sample_instances();
        app.table_state.select(Some(0));

        let action = handle_test_event(&mut app, key_event(KeyCode::Enter));
        assert_eq!(action, Some(Action::Attach("myproject-abc123".to_string())));
    }

    #[test]
    fn test_handle_event_no_selection() {
        let mut app = create_test_app_for_ui();
        app.instances = sample_instances();
        // No selection

        let action = handle_test_event(&mut app, key_event(KeyCode::Enter));
        assert_eq!(action, None);
        assert!(app.status_message.is_some());
    }

    #[test]
    fn test_selected_instance() {
        let mut app = create_test_app_for_ui();
        app.instances = sample_instances();

        assert!(app.selected_instance().is_none());

        app.table_state.select(Some(1));
        let selected = app.selected_instance().unwrap();
        assert_eq!(selected.name, "otherrepo-def456");
    }

    #[test]
    fn test_derive_agent_state_empty_messages() {
        let messages: Vec<serde_json::Value> = vec![];
        assert_eq!(
            derive_agent_state_from_messages(&messages),
            AgentState::Unknown
        );
    }

    #[test]
    fn test_derive_agent_state_no_assistant_message() {
        let messages = vec![serde_json::json!({
            "info": {"role": "user"},
            "parts": [{"type": "text", "text": "Hello"}]
        })];
        assert_eq!(
            derive_agent_state_from_messages(&messages),
            AgentState::Unknown
        );
    }

    #[test]
    fn test_derive_agent_state_working_no_completed_time() {
        // Message without completed time indicates agent is still working
        let messages = vec![serde_json::json!({
            "info": {
                "role": "assistant",
                "time": {"created": 1234567890}
            },
            "parts": [{"type": "text", "text": "Working on it..."}]
        })];
        assert_eq!(
            derive_agent_state_from_messages(&messages),
            AgentState::Working
        );
    }

    #[test]
    fn test_derive_agent_state_idle_with_stop_finish() {
        // Completed message with finish=stop indicates idle
        let messages = vec![serde_json::json!({
            "info": {
                "role": "assistant",
                "time": {"created": 1234567890, "completed": 1234567891},
                "finish": "stop"
            },
            "parts": [{"type": "text", "text": "Done!"}]
        })];
        assert_eq!(
            derive_agent_state_from_messages(&messages),
            AgentState::Idle
        );
    }

    #[test]
    fn test_derive_agent_state_working_with_tool_calls_finish() {
        // Completed message with finish=tool-calls indicates still working
        let messages = vec![serde_json::json!({
            "info": {
                "role": "assistant",
                "time": {"created": 1234567890, "completed": 1234567891},
                "finish": "tool-calls"
            },
            "parts": [{"type": "text", "text": "Making tool call..."}]
        })];
        assert_eq!(
            derive_agent_state_from_messages(&messages),
            AgentState::Working
        );
    }

    #[test]
    fn test_derive_agent_state_working_with_incomplete_tool() {
        // Message with tool part that's not completed
        let messages = vec![serde_json::json!({
            "info": {
                "role": "assistant",
                "time": {"created": 1234567890, "completed": 1234567891}
            },
            "parts": [
                {"type": "text", "text": "Running a tool..."},
                {"type": "tool", "state": {"status": "running"}}
            ]
        })];
        assert_eq!(
            derive_agent_state_from_messages(&messages),
            AgentState::Working
        );
    }

    #[test]
    fn test_derive_agent_state_idle_with_completed_tool() {
        // Message with completed tool part
        let messages = vec![serde_json::json!({
            "info": {
                "role": "assistant",
                "time": {"created": 1234567890, "completed": 1234567891}
            },
            "parts": [
                {"type": "text", "text": "Tool result..."},
                {"type": "tool", "state": {"status": "completed"}}
            ]
        })];
        assert_eq!(
            derive_agent_state_from_messages(&messages),
            AgentState::Idle
        );
    }
}
