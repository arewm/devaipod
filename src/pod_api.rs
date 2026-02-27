//! Per-pod HTTP API server (sidecar container mode).
//!
//! Runs inside a sidecar container that mounts the workspace volumes directly,
//! replacing the current approach of exec'ing into containers for git/PTY
//! operations. All git commands run as direct `tokio::process::Command` calls
//! against the local filesystem, eliminating the ~200-500ms per-exec overhead.

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use color_eyre::eyre::{Context, Result};
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::sync::broadcast;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// Server state shared across all handlers.
#[derive(Clone)]
struct AppState {
    /// Path to the workspace root (default `/workspaces`).
    workspace: Arc<PathBuf>,
    /// Broadcast sender for git filesystem change events.
    git_events_tx: broadcast::Sender<GitEvent>,
}

// ---------------------------------------------------------------------------
// Response / request types (independent of web.rs)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct GitStatusResponse {
    exit_code: i32,
    output: String,
    files: Vec<GitStatusFile>,
}

#[derive(Debug, Serialize)]
struct GitStatusFile {
    status: String,
    path: String,
}

#[derive(Debug, Serialize)]
struct GitDiffResponse {
    exit_code: i32,
    diff: String,
}

#[derive(Debug, Serialize)]
struct GitCommitsResponse {
    exit_code: i32,
    commits: Vec<GitCommit>,
}

#[derive(Debug, Serialize)]
struct GitCommit {
    hash: String,
    message: String,
}

#[derive(Debug, Deserialize)]
struct GitLogQuery {
    base: Option<String>,
    head: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GitLogEntry {
    sha: String,
    short_sha: String,
    message: String,
    author: String,
    author_email: String,
    timestamp: String,
    parents: Vec<String>,
}

#[derive(Debug, Serialize)]
struct GitLogResponse {
    commits: Vec<GitLogEntry>,
}

#[derive(Debug, Deserialize)]
struct GitDiffRangeQuery {
    base: String,
    head: String,
}

#[derive(Debug, Serialize)]
struct FileDiff {
    file: String,
    before: String,
    after: String,
    additions: u32,
    deletions: u32,
    status: &'static str,
}

#[derive(Debug, Serialize)]
struct GitDiffRangeResponse {
    files: Vec<FileDiff>,
}

#[derive(Debug, Serialize)]
struct GitFetchResponse {
    success: bool,
    message: String,
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

// ---------------------------------------------------------------------------
// Git filesystem watcher (SSE event source)
// ---------------------------------------------------------------------------

/// Event emitted when the git state changes on disk.
#[derive(Clone, Debug, Serialize)]
struct GitEvent {
    head: String,
    timestamp: String,
}

/// Reads the current HEAD sha from the workspace. Returns an empty string on
/// failure (e.g. bare init with no commits yet).
async fn read_head_sha(workspace: &PathBuf) -> String {
    match run_git(workspace, &["rev-parse", "HEAD"]).await {
        Ok((0, stdout, _)) => String::from_utf8_lossy(&stdout).trim().to_string(),
        _ => String::new(),
    }
}

/// Background watcher that monitors `.git/` for ref changes and broadcasts
/// `GitEvent`s to all SSE subscribers.
///
/// The `RecommendedWatcher` is kept alive inside the spawned task — dropping it
/// would stop inotify watches.
struct GitWatcher;

impl GitWatcher {
    /// Spawn the background watcher task. Returns the broadcast sender that SSE
    /// handlers subscribe to.
    fn spawn(workspace: Arc<PathBuf>) -> broadcast::Sender<GitEvent> {
        let (tx, _) = broadcast::channel::<GitEvent>(64);
        let tx_clone = tx.clone();

        tokio::spawn(async move {
            if let Err(e) = Self::run(workspace, tx_clone).await {
                tracing::error!("GitWatcher exited with error: {e}");
            }
        });

        tx
    }

    async fn run(workspace: Arc<PathBuf>, tx: broadcast::Sender<GitEvent>) -> Result<()> {
        use notify::{RecursiveMode, Watcher};

        let (fs_tx, mut fs_rx) = tokio::sync::mpsc::channel::<()>(128);

        // The watcher must live as long as we want events, so we bind it here
        // and keep it alive for the lifetime of this task.
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            match res {
                Ok(_) => {
                    // Non-blocking send; if the channel is full we just skip
                    // (the debounce will catch the next one).
                    let _ = fs_tx.try_send(());
                }
                Err(e) => tracing::warn!("filesystem watch error: {e}"),
            }
        })
        .context("Failed to create filesystem watcher")?;

        let git_dir = workspace.join(".git");

        // Watch .git/refs/ recursively (branch updates, remote refs, tags).
        let refs_dir = git_dir.join("refs");
        if refs_dir.is_dir() {
            watcher
                .watch(&refs_dir, RecursiveMode::Recursive)
                .with_context(|| format!("Failed to watch {}", refs_dir.display()))?;
        }

        // Watch .git/HEAD (branch switches).
        let head_file = git_dir.join("HEAD");
        if head_file.exists() {
            watcher
                .watch(&head_file, RecursiveMode::NonRecursive)
                .with_context(|| format!("Failed to watch {}", head_file.display()))?;
        }

        // Watch .git/FETCH_HEAD if it exists (created on first fetch).
        let fetch_head = git_dir.join("FETCH_HEAD");
        if fetch_head.exists() {
            if let Err(e) = watcher.watch(&fetch_head, RecursiveMode::NonRecursive) {
                tracing::debug!("FETCH_HEAD watch skipped (will retry): {e}");
            }
        }

        tracing::info!("GitWatcher started for {}", workspace.display());

        // Debounced event loop: collect events for 200ms, then emit one
        // GitEvent with the current HEAD.
        loop {
            // Wait for at least one filesystem notification.
            if fs_rx.recv().await.is_none() {
                // Channel closed — watcher was dropped.
                break;
            }

            // Debounce: drain any further events that arrive within 200ms.
            tokio::time::sleep(Duration::from_millis(200)).await;
            while fs_rx.try_recv().is_ok() {}

            let head = read_head_sha(&workspace).await;
            let event = GitEvent {
                head,
                timestamp: chrono::Utc::now().to_rfc3339(),
            };

            // Ignore send errors (no active receivers is fine).
            let _ = tx.send(event);
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// SSE handler
// ---------------------------------------------------------------------------

/// `GET /git/events` — Server-Sent Events stream of git state changes.
///
/// Sends an initial event with the current HEAD sha, then pushes `git.updated`
/// events whenever the filesystem watcher detects ref changes. A keepalive
/// comment is sent every 30 seconds to prevent proxy timeouts.
async fn git_events_sse(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let workspace = state.workspace.clone();
    let mut rx = state.git_events_tx.subscribe();

    let stream = async_stream::stream! {
        // Send initial HEAD so the client has a baseline.
        let head = read_head_sha(&workspace).await;
        let initial = GitEvent {
            head,
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        if let Ok(data) = serde_json::to_string(&initial) {
            yield Ok(Event::default().event("git.updated").data(data));
        }

        // Stream subsequent events from the broadcast channel.
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Ok(data) = serde_json::to_string(&event) {
                        yield Ok(Event::default().event("git.updated").data(data));
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!("SSE client lagged, skipped {n} events");
                    // Continue — the next event will have the latest HEAD anyway.
                }
                Err(broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    };

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(30))
            .text("keepalive"),
    )
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

/// Validate that a string looks like a safe git ref (no shell metacharacters).
fn is_valid_git_ref(s: &str) -> bool {
    !s.is_empty()
        && !s.contains("..")
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '/' | '.' | '_' | '~' | '^'))
}

/// Maximum number of files allowed in a diff-range response.
const DIFF_RANGE_MAX_FILES: usize = 100;

// ---------------------------------------------------------------------------
// Git command helpers
// ---------------------------------------------------------------------------

/// Run a git command in the workspace directory and return (exit_code, stdout, stderr).
async fn run_git(workspace: &PathBuf, args: &[&str]) -> Result<(i32, Vec<u8>, Vec<u8>)> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .await
        .with_context(|| format!("Failed to spawn git {}", args.first().unwrap_or(&"")))?;

    let exit_code = output.status.code().unwrap_or(-1);
    Ok((exit_code, output.stdout, output.stderr))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /git/status` — `git status --porcelain`
async fn git_status(
    State(state): State<AppState>,
) -> Result<Json<GitStatusResponse>, StatusCode> {
    let (exit_code, stdout, _stderr) =
        run_git(&state.workspace, &["status", "--porcelain"])
            .await
            .map_err(|e| {
                tracing::error!("git status failed: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

    let output = String::from_utf8_lossy(&stdout).to_string();

    let files: Vec<GitStatusFile> = output
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
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

/// `GET /git/diff` — `git diff HEAD`
async fn git_diff(State(state): State<AppState>) -> Result<Json<GitDiffResponse>, StatusCode> {
    let (exit_code, stdout, _stderr) = run_git(&state.workspace, &["diff", "HEAD"])
        .await
        .map_err(|e| {
            tracing::error!("git diff failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let diff = String::from_utf8_lossy(&stdout).to_string();

    Ok(Json(GitDiffResponse { exit_code, diff }))
}

/// `GET /git/commits` — `git log --oneline -20`
async fn git_commits(
    State(state): State<AppState>,
) -> Result<Json<GitCommitsResponse>, StatusCode> {
    let (exit_code, stdout, _stderr) =
        run_git(&state.workspace, &["log", "--oneline", "-20"])
            .await
            .map_err(|e| {
                tracing::error!("git log failed: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

    let output = String::from_utf8_lossy(&stdout);

    let commits: Vec<GitCommit> = output
        .lines()
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let mut parts = line.splitn(2, ' ');
            let hash = parts.next()?.to_string();
            let message = parts.next().unwrap_or("").to_string();
            Some(GitCommit { hash, message })
        })
        .collect();

    Ok(Json(GitCommitsResponse { exit_code, commits }))
}

/// `GET /git/log` — structured git log with optional range filtering.
///
/// When no `base`/`head` are provided, shows recent commits from HEAD.
/// The pod-api mounts the agent's workspace directly, so HEAD reflects
/// the agent's latest work. Returns an empty list if the ref doesn't exist.
async fn git_log(
    State(state): State<AppState>,
    Query(params): Query<GitLogQuery>,
) -> Result<Json<GitLogResponse>, (StatusCode, String)> {
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

    // Build the format arg and range specification.
    let format_arg =
        "--format=%H%x00%h%x00%s%n%b%x00%an%x00%ae%x00%aI%x00%P%x1e".to_string();
    let range_arg: String;

    let mut args: Vec<&str> = vec!["log", &format_arg];

    match (&params.base, &params.head) {
        (Some(base), Some(head)) => {
            range_arg = format!("{base}..{head}");
            args.push(&range_arg);
            args.push("-500");
        }
        (None, Some(head)) => {
            args.push(head.as_str());
            args.push("-50");
        }
        (Some(_), None) => {
            return Err((
                StatusCode::BAD_REQUEST,
                "'base' requires 'head' to also be specified".to_string(),
            ));
        }
        // Default: show recent commits on current branch.
        // The pod-api container mounts the agent's workspace directly,
        // so HEAD is already the agent's latest commit.
        (None, None) => {
            args.push("HEAD");
            args.push("-50");
        }
    }

    let (exit_code, stdout, stderr) = run_git(&state.workspace, &args).await.map_err(|e| {
        tracing::error!("git log failed: {e}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to run git log: {e}"),
        )
    })?;

    if exit_code != 0 {
        let stderr_text = String::from_utf8_lossy(&stderr);
        // Unknown ref → empty list rather than 500.
        if stderr_text.contains("unknown revision") || stderr_text.contains("bad default revision")
        {
            return Ok(Json(GitLogResponse {
                commits: Vec::new(),
            }));
        }
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("git log failed: {stderr_text}"),
        ));
    }

    let output = String::from_utf8_lossy(&stdout);

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

/// `GET /git/diff-range?base=X&head=Y` — structured per-file diffs.
///
/// Returns before/after file content, addition/deletion counts, and change
/// status for each file changed between `base` and `head`. File content is
/// fetched concurrently via `git show ref:path`.
async fn git_diff_range(
    State(state): State<AppState>,
    Query(params): Query<GitDiffRangeQuery>,
) -> Result<Json<GitDiffRangeResponse>, (StatusCode, String)> {
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

    // Get name-status and numstat in parallel.
    let ws = &state.workspace;
    let ns_args = [
        "diff",
        "--name-status",
        "--no-renames",
        &params.base,
        &params.head,
    ];
    let num_args = [
        "diff",
        "--numstat",
        "--no-renames",
        &params.base,
        &params.head,
    ];
    let (ns_result, num_result) = tokio::join!(
        run_git(ws, &ns_args),
        run_git(ws, &num_args),
    );

    let (ns_exit, ns_stdout, ns_stderr) = ns_result.map_err(|e| {
        tracing::error!("git diff --name-status failed: {e}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to run git diff --name-status: {e}"),
        )
    })?;

    if ns_exit != 0 {
        let stderr_text = String::from_utf8_lossy(&ns_stderr);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("git diff --name-status failed: {stderr_text}"),
        ));
    }

    let (num_exit, num_stdout, num_stderr) = num_result.map_err(|e| {
        tracing::error!("git diff --numstat failed: {e}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to run git diff --numstat: {e}"),
        )
    })?;

    if num_exit != 0 {
        let stderr_text = String::from_utf8_lossy(&num_stderr);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("git diff --numstat failed: {stderr_text}"),
        ));
    }

    let ns_output = String::from_utf8_lossy(&ns_stdout);
    let num_output = String::from_utf8_lossy(&num_stdout);

    if ns_output.trim().is_empty() {
        return Ok(Json(GitDiffRangeResponse { files: Vec::new() }));
    }

    // Parse --name-status
    let mut file_statuses: Vec<(String, &'static str)> = Vec::new();
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
        file_statuses.push((file_path.to_string(), status));
    }

    if file_statuses.len() > DIFF_RANGE_MAX_FILES {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "Too many changed files ({}, max {DIFF_RANGE_MAX_FILES})",
                file_statuses.len(),
            ),
        ));
    }

    // Parse --numstat
    let mut numstat_map: HashMap<String, (u32, u32)> = HashMap::new();
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
        numstat_map.insert(fields[2].trim().to_string(), (adds, dels));
    }

    // Fetch before/after content concurrently using `git show ref:path`.
    // Since we're running direct commands (no exec overhead), we can spawn
    // one task per file and it's still fast.
    let base_files: Vec<&str> = file_statuses
        .iter()
        .filter(|(_, status)| *status != "added")
        .map(|(path, _)| path.as_str())
        .collect();
    let head_files: Vec<&str> = file_statuses
        .iter()
        .filter(|(_, status)| *status != "deleted")
        .map(|(path, _)| path.as_str())
        .collect();

    let (base_contents, head_contents) = tokio::join!(
        fetch_file_contents(ws, &params.base, &base_files),
        fetch_file_contents(ws, &params.head, &head_files),
    );

    let mut files = Vec::with_capacity(file_statuses.len());
    for (file_path, status) in &file_statuses {
        let (adds, dels) = numstat_map.get(file_path.as_str()).copied().unwrap_or((0, 0));
        files.push(FileDiff {
            file: file_path.clone(),
            before: base_contents.get(file_path.as_str()).cloned().unwrap_or_default(),
            after: head_contents.get(file_path.as_str()).cloned().unwrap_or_default(),
            additions: adds,
            deletions: dels,
            status,
        });
    }

    Ok(Json(GitDiffRangeResponse { files }))
}

/// Fetch the content of multiple files at a given git ref, concurrently.
///
/// Returns a map from file path to content. Missing files map to empty strings.
async fn fetch_file_contents(
    workspace: &PathBuf,
    git_ref: &str,
    files: &[&str],
) -> HashMap<String, String> {
    if files.is_empty() {
        return HashMap::new();
    }

    let mut tasks = Vec::with_capacity(files.len());
    for &file in files {
        let ws = workspace.clone();
        let ref_path = format!("{git_ref}:{file}");
        let file_owned = file.to_string();
        tasks.push(tokio::spawn(async move {
            let output = Command::new("git")
                .args(["show", &ref_path])
                .current_dir(&ws)
                .output()
                .await;
            let content = match output {
                Ok(o) if o.status.success() => {
                    String::from_utf8(o.stdout)
                        .unwrap_or_else(|e| String::from_utf8_lossy(&e.into_bytes()).into_owned())
                }
                _ => String::new(),
            };
            (file_owned, content)
        }));
    }

    let mut map = HashMap::with_capacity(tasks.len());
    for task in tasks {
        if let Ok((path, content)) = task.await {
            map.insert(path, content);
        }
    }
    map
}

/// `POST /git/fetch-agent` — `git fetch agent`
async fn git_fetch_agent(
    State(state): State<AppState>,
) -> Result<Json<GitFetchResponse>, (StatusCode, String)> {
    let (exit_code, _stdout, stderr) =
        run_git(&state.workspace, &["fetch", "agent"])
            .await
            .map_err(|e| {
                tracing::error!("git fetch agent failed: {e}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to run git fetch agent: {e}"),
                )
            })?;

    if exit_code != 0 {
        let stderr_text = String::from_utf8_lossy(&stderr);
        return Ok(Json(GitFetchResponse {
            success: false,
            message: format!("git fetch agent failed: {stderr_text}"),
        }));
    }

    Ok(Json(GitFetchResponse {
        success: true,
        message: "Fetched latest agent commits".to_string(),
    }))
}

/// `POST /git/push` — `git push origin <branch>`
async fn git_push(
    State(state): State<AppState>,
    Json(body): Json<GitPushRequest>,
) -> Result<Json<GitPushResponse>, (StatusCode, String)> {
    if !is_valid_git_ref(&body.branch) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Invalid branch name: {}", body.branch),
        ));
    }

    let (exit_code, _stdout, stderr) =
        run_git(&state.workspace, &["push", "origin", &body.branch])
            .await
            .map_err(|e| {
                tracing::error!("git push failed: {e}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to run git push: {e}"),
                )
            })?;

    if exit_code != 0 {
        let stderr_text = String::from_utf8_lossy(&stderr);
        return Ok(Json(GitPushResponse {
            success: false,
            message: format!("git push failed: {stderr_text}"),
        }));
    }

    Ok(Json(GitPushResponse {
        success: true,
        message: format!("Pushed branch '{}' to origin", body.branch),
    }))
}

// ---------------------------------------------------------------------------
// PTY stubs (to be implemented in a follow-up)
// ---------------------------------------------------------------------------

async fn pty_create() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}

async fn pty_resize() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}

async fn pty_websocket() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}

// ---------------------------------------------------------------------------
// Server entrypoint
// ---------------------------------------------------------------------------

/// CLI arguments for the `pod-api` subcommand.
#[derive(Debug, clap::Args)]
pub(crate) struct PodApiArgs {
    /// Port to listen on
    #[arg(long, default_value = "8090")]
    port: u16,
    /// Path to the workspace directory
    #[arg(long, default_value = "/workspaces")]
    workspace: PathBuf,
}

/// Build the axum router (public for testing).
fn build_router(state: AppState) -> Router {
    Router::new()
        // Git endpoints
        .route("/git/status", get(git_status))
        .route("/git/diff", get(git_diff))
        .route("/git/commits", get(git_commits))
        .route("/git/log", get(git_log))
        .route("/git/diff-range", get(git_diff_range))
        .route("/git/events", get(git_events_sse))
        .route("/git/fetch-agent", post(git_fetch_agent))
        .route("/git/push", post(git_push))
        // PTY stubs
        .route("/pty/create", post(pty_create))
        .route("/pty/resize", post(pty_resize))
        .route("/pty/ws", get(pty_websocket))
        .with_state(state)
}

/// Run the pod-api HTTP server.
pub(crate) async fn run(args: PodApiArgs) -> Result<()> {
    let workspace = Arc::new(args.workspace.clone());
    let git_events_tx = GitWatcher::spawn(Arc::clone(&workspace));

    let state = AppState {
        workspace,
        git_events_tx,
    };

    let app = build_router(state);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], args.port));
    tracing::info!("pod-api listening on {addr} (workspace: {})", args.workspace.display());

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("Failed to bind to {addr}"))?;

    axum::serve(listener, app)
        .await
        .context("pod-api server error")?;

    Ok(())
}
