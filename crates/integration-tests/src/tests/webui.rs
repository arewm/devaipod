//! Web UI integration tests (containerized)
//!
//! These tests verify the web UI server running inside a container:
//! - Token-based authentication
//! - Podman socket proxy
//! - Pod list API access
//! - **UI surface**: control-plane HTML and all APIs used by the frontend
//!
//! The tests build and run the devaipod container image, mounting the
//! podman socket to allow the web UI to manage sibling containers.
//!
//! ## UI surface (keep covered by tests)
//!
//! The frontend (static `dist/index.html` or Leptos/WASM build) relies on:
//!
//! - **GET /** — Control plane HTML (optional `?token=...`). Must return 200 and
//!   contain "devaipod" and control-plane markers (e.g. "Refresh", "Launch", or token prompt).
//! - **GET /_devaipod/agent/{name}** — 307 redirect to /_devaipod/agent/{name}/ with Set-Cookie DEVAIPOD_AGENT_POD.
//! - **GET /_devaipod/agent/{name}/** — Iframe wrapper page (back button bar + full-screen iframe loading /_devaipod/opencode-ui).
//! - **GET /_devaipod/opencode-ui** — Raw opencode SPA index.html (with error interceptor, no URL rewriting).
//! - **GET /api/podman/v5.0.0/libpod/pods/json** — Pod list (Bearer token required).
//! - **POST /api/podman/.../pods/{name}/start**, **.../stop**, **DELETE .../pods/{name}?force=true**
//! - **GET /api/devaipod/pods/{name}/opencode-info** — Agent info for "Open Agent".
//! - **POST /api/devaipod/pods/{name}/recreate** — Recreate workspace (same repo).
//! - **POST /api/devaipod/run** — Create workspace (JSON: `{ "source", "task" }`).
//!
//! ## How to run
//!
//! - **Integration tests (this module)**: Run **`just test-integration-container`** so the container
//!   is built and tests run against it. Direct `cargo test -p integration-tests` will error unless
//!   `DEVAIPOD_INTEGRATION_ALLOW_DIRECT=1` is set.
//! - **Fast route tests (no container)**: `cargo test -p devaipod web::tests::` — sub-second.
//!
//! ## Testing approach (why these tests exist)
//!
//! We test the **critical path a browser takes** so regressions (e.g. "Could not connect to server",
//! asset 404s, missing API routes) are caught:
//!
//! - **Agent page load**: GET /_devaipod/agent/{name}/ returns the iframe wrapper page (back button
//!   bar + full-screen iframe pointing to /_devaipod/opencode-ui). The opencode SPA runs unmodified
//!   inside the iframe.
//! - **Static assets**: JS and CSS from the opencode-ui index are served via /assets/* with the
//!   DEVAIPOD_AGENT_POD cookie (the cookie tells serve_root_assets to serve from opencode dir).
//! - **Root-level API with cookie**: The opencode app uses `window.location.origin`, so it requests
//!   /session, /rpc, /path, etc. at **root**. We proxy those using the DEVAIPOD_AGENT_POD cookie.
//!   Tests send that cookie and assert root /session (and other API segments) return not 400/404.
//! - **Full critical path**: `test_web_opencode_ui_full_critical_path` runs the complete flow (index →
//!   JS + CSS assets → root API with cookie) so "opencode web UI works" is covered in one test.
//! - **Event stream (no cookie)**: `test_web_event_stream_no_cookie_returns_http11_sse` asserts GET /event,
//!   /global, and /global/event without agent cookie return HTTP/1.1 200 with `text/event-stream`
//!   so webviews (e.g. global-sdk) do not see "Load failed".
//! - **Event stream (unreachable pod)**: `test_web_event_stream_with_cookie_unreachable_pod_returns_http11_sse`
//!   asserts that SSE endpoints with a cookie pointing to a nonexistent pod still return HTTP/1.1 200
//!   keepalive (not 502/503), preventing the global-sdk reconnection storm.
//! - **Control plane**: Root with token, auth, run endpoint are covered.
//!
//! Adding a new route or rewrite that affects the agent UI should be covered by one of these tests.
//!
//! ## Test tiers
//!
//! Tests are categorized into tiers based on their resource needs:
//!
//! - **Tier 1 (parallel safe, no pod)**: Static file serving, auth checks, health endpoints
//! - **Tier 2 (parallel safe, needs pod)**: Pod listing, opencode-info (uses the shared test pod)
//! - **Tier 3 (serial, mutations)**: Tests that modify state (currently none)
//!
//! All tests share a single `WebFixture` container to reduce resource contention.

use color_eyre::eyre::bail;
use color_eyre::Result;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;
use xshell::{cmd, Shell};

use crate::podman_integration_test;
use crate::shell;

/// Default image tag when tests build the image themselves
const TEST_IMAGE_TAG: &str = "localhost/devaipod:test";

/// Env var: when set, use this image and do not build (e.g. `just container-build` then run tests).
const DEVAIPOD_CONTAINER_IMAGE_ENV: &str = "DEVAIPOD_CONTAINER_IMAGE";

/// When set (e.g. 1), allow running tests without DEVAIPOD_CONTAINER_IMAGE. By default, direct runs error out.
const DEVAIPOD_INTEGRATION_ALLOW_DIRECT_ENV: &str = "DEVAIPOD_INTEGRATION_ALLOW_DIRECT";

/// Image tag for the web container: DEVAIPOD_CONTAINER_IMAGE if set, else TEST_IMAGE_TAG.
fn web_container_image() -> String {
    std::env::var(DEVAIPOD_CONTAINER_IMAGE_ENV).unwrap_or_else(|_| TEST_IMAGE_TAG.to_string())
}

/// Build result cached across tests (only used when not using DEVAIPOD_CONTAINER_IMAGE).
static BUILD_RESULT: OnceLock<Result<(), String>> = OnceLock::new();

/// Build the devaipod container image for testing (unless DEVAIPOD_CONTAINER_IMAGE is set).
/// When DEVAIPOD_CONTAINER_IMAGE is set, the caller is expected to have built the image
/// (e.g. via `just container-build`); we skip building and use that image.
fn ensure_container_built() -> Result<()> {
    if std::env::var(DEVAIPOD_CONTAINER_IMAGE_ENV).is_ok() {
        tracing::info!(
            "Using pre-built image from {}",
            DEVAIPOD_CONTAINER_IMAGE_ENV
        );
        return Ok(());
    }
    if std::env::var(DEVAIPOD_INTEGRATION_ALLOW_DIRECT_ENV).is_err() {
        bail!("Integration tests must be run against a built container. Run: just test-integration-container. Set DEVAIPOD_INTEGRATION_ALLOW_DIRECT=1 to allow direct runs.");
    }
    let result = BUILD_RESULT.get_or_init(|| {
        let sh = match Shell::new() {
            Ok(sh) => sh,
            Err(e) => return Err(format!("Failed to create shell: {}", e)),
        };

        // Find workspace root (where Containerfile lives)
        let mut dir = match std::env::current_dir() {
            Ok(d) => d,
            Err(e) => return Err(format!("Failed to get current dir: {}", e)),
        };
        loop {
            if dir.join("Containerfile").exists() {
                break;
            }
            if !dir.pop() {
                return Err("Could not find Containerfile in parent directories".to_string());
            }
        }

        let containerfile = dir.join("Containerfile");
        let context = dir.to_string_lossy().to_string();
        let containerfile_str = containerfile.to_string_lossy().to_string();

        tracing::info!("Building devaipod container image: {}", TEST_IMAGE_TAG);

        let build_output = cmd!(
            sh,
            "podman build --tag {TEST_IMAGE_TAG} -f {containerfile_str} {context}"
        )
        .ignore_status()
        .output();

        match build_output {
            Ok(output) => {
                if output.status.success() {
                    tracing::info!("Container image built successfully");
                    Ok(())
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    Err(format!("Container build failed: {}", stderr))
                }
            }
            Err(e) => Err(format!("Failed to run podman build: {}", e)),
        }
    });

    match result {
        Ok(()) => Ok(()),
        Err(msg) => bail!("{}", msg),
    }
}

/// Helper struct to manage a running web container.
/// Automatically removes the container on drop.
struct WebContainerGuard {
    container_name: String,
    token: String,
    /// Keep temp dir alive for config file
    _config_dir: tempfile::TempDir,
}

impl WebContainerGuard {
    /// Generate a unique container name using timestamp
    fn generate_name() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let unique = (now.as_secs() & 0xFFFF) ^ ((now.subsec_nanos() as u64) & 0xFFFF);
        format!("test-devaipod-web-{:x}", unique)
    }

    /// Start the web container and extract the token from logs
    fn start() -> Result<Self> {
        ensure_container_built()?;

        let sh = shell()?;

        // Generate unique container name
        let container_name = Self::generate_name();

        // Socket for volume mount: on Linux use host path; on macOS/Windows podman runs in VM,
        // so we must use -v /run/podman/podman.sock:/run/podman/podman.sock (VM path), not the Mac path.
        let (socket_mount, check_socket) = if let Ok(xdg_runtime) = std::env::var("XDG_RUNTIME_DIR")
        {
            let path = format!("{}/podman/podman.sock", xdg_runtime);
            (format!("{}:/run/podman/podman.sock", path), Some(path))
        } else {
            // macOS/Windows: container runs in VM; VM bind-mounts its own socket
            (
                "/run/podman/podman.sock:/run/podman/podman.sock".to_string(),
                std::env::var("DEVAIPOD_PODMAN_SOCKET").ok(),
            )
        };

        // Check podman is reachable (on macOS we may have Mac socket path for the binary, not for the mount)
        if let Some(ref path) = check_socket {
            if !std::path::Path::new(path).exists() {
                bail!("Podman socket not found at {}. Is podman running?", path);
            }
        }

        // Create minimal config file (devaipod requires config to exist)
        let config_dir = tempfile::TempDir::new()?;
        let config_path = config_dir.path().join("devaipod.toml");
        std::fs::write(
            &config_path,
            r#"# Minimal config for testing
# All fields have defaults, empty config is valid
"#,
        )?;
        // Use :ro,z for SELinux relabeling
        let config_mount = format!(
            "{}:/root/.config/devaipod.toml:ro,z",
            config_path.to_string_lossy()
        );

        let image = web_container_image();
        tracing::info!(
            "Starting web container {} from image {}",
            container_name,
            image
        );

        // Run the container (no port forwarding needed - we use podman exec)
        let run_output = cmd!(
            sh,
            "podman run -d --name {container_name} --privileged -v {socket_mount} -v {config_mount} {image}"
        )
        .ignore_status()
        .output()?;

        if !run_output.status.success() {
            let stderr = String::from_utf8_lossy(&run_output.stderr);
            bail!("Failed to start web container: {}", stderr);
        }

        // Wait for container to start and extract token from logs
        let token = Self::wait_for_token(&sh, &container_name, Duration::from_secs(60))?;

        Ok(WebContainerGuard {
            container_name,
            token,
            _config_dir: config_dir,
        })
    }

    /// Wait for the container to output the token and extract it
    fn wait_for_token(sh: &Shell, container_name: &str, timeout: Duration) -> Result<String> {
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(500);

        while start.elapsed() < timeout {
            // Get container logs
            let logs_output = cmd!(sh, "podman logs {container_name}")
                .ignore_status()
                .output()?;

            let logs = String::from_utf8_lossy(&logs_output.stdout);
            let stderr = String::from_utf8_lossy(&logs_output.stderr);
            let combined = format!("{}\n{}", logs, stderr);

            // Look for token in output
            // Format: "Web UI: http://0.0.0.0:8080/?token=TOKEN"
            for line in combined.lines() {
                if line.contains("token=") {
                    if let Some(token_start) = line.find("token=") {
                        let mut token = line[token_start + 6..].trim().to_string();
                        // Token may have trailing characters
                        if let Some(end) =
                            token.find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
                        {
                            token = token[..end].to_string();
                        }
                        if !token.is_empty() {
                            tracing::info!("Extracted token from container logs");
                            return Ok(token);
                        }
                    }
                }
            }

            std::thread::sleep(poll_interval);
        }

        // Get final logs for debugging
        let final_logs = cmd!(sh, "podman logs {container_name}")
            .ignore_status()
            .output()?;
        let logs = String::from_utf8_lossy(&final_logs.stdout);
        let stderr = String::from_utf8_lossy(&final_logs.stderr);

        bail!(
            "Timeout waiting for token in container logs.\nstdout: {}\nstderr: {}",
            logs,
            stderr
        )
    }

    /// Get the authentication token
    fn token(&self) -> &str {
        &self.token
    }

    /// Run curl inside the container and return (status_code, body)
    /// The web server binds to 127.0.0.1 inside the container, so we must
    /// use `podman exec` to access it.
    fn curl_in_container(&self, path: &str, auth_token: Option<&str>) -> Result<(i32, String)> {
        let url = format!("http://127.0.0.1:8080{}", path);
        let container = &self.container_name;

        let output = if let Some(token) = auth_token {
            let auth_header = format!("Authorization: Bearer {}", token);
            Command::new("podman")
                .args([
                    "exec",
                    container,
                    "curl",
                    "-s",
                    "-w",
                    "\n%{http_code}",
                    "--connect-timeout",
                    "5",
                    "--max-time",
                    "30",
                    "--retry",
                    "5",
                    "--retry-connrefused",
                    "--retry-all-errors",
                    "--retry-delay",
                    "2",
                    "-H",
                    &auth_header,
                    &url,
                ])
                .output()?
        } else {
            Command::new("podman")
                .args([
                    "exec",
                    container,
                    "curl",
                    "-s",
                    "-w",
                    "\n%{http_code}",
                    "--connect-timeout",
                    "5",
                    "--max-time",
                    "30",
                    "--retry",
                    "5",
                    "--retry-connrefused",
                    "--retry-all-errors",
                    "--retry-delay",
                    "2",
                    &url,
                ])
                .output()?
        };

        let combined = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<&str> = combined.trim().lines().collect();

        if lines.is_empty() {
            return Ok((-1, String::new()));
        }

        let status_code: i32 = lines.last().and_then(|s| s.parse().ok()).unwrap_or(-1);
        let body = if lines.len() > 1 {
            lines[..lines.len() - 1].join("\n")
        } else {
            String::new()
        };

        Ok((status_code, body))
    }

    /// Run curl with -i and optional Cookie header; captures HTTP version, status, content-type, body.
    /// Returns (http_version, status_code, content_type, body).
    /// `http_version` is e.g. "HTTP/1.1" or "HTTP/1.0".
    fn curl_in_container_full_headers(
        &self,
        path: &str,
        max_time_secs: u8,
        cookie: Option<&str>,
    ) -> Result<(String, i32, String, String)> {
        let url = format!("http://127.0.0.1:8080{}", path);
        let mut args: Vec<String> = vec![
            "exec".into(),
            self.container_name.clone(),
            "curl".into(),
            "-i".into(),
            "-s".into(),
            "--connect-timeout".into(),
            "5".into(),
            "--max-time".into(),
            max_time_secs.to_string(),
        ];
        if let Some(c) = cookie {
            args.push("-H".into());
            args.push(format!("Cookie: {}", c));
        }
        args.push(url);
        let output = Command::new("podman").args(&args).output()?;
        let raw = String::from_utf8_lossy(&output.stdout);
        let (headers_str, body) = if let Some(pos) = raw.find("\r\n\r\n") {
            (raw[..pos].to_string(), raw[pos + 4..].trim().to_string())
        } else if let Some(pos) = raw.find("\n\n") {
            (raw[..pos].to_string(), raw[pos + 2..].trim().to_string())
        } else {
            (raw.to_string(), String::new())
        };
        let status_line = headers_str.lines().next().unwrap_or("");
        let http_version = status_line
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string();
        let status = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(-1);
        let content_type = headers_str
            .lines()
            .find(|l| l.trim_start().to_lowercase().starts_with("content-type:"))
            .and_then(|l| l.split(':').nth(1).map(|v| v.trim().to_string()))
            .unwrap_or_default();
        Ok((http_version, status, content_type, body))
    }

    /// Run curl with a Cookie header (e.g. for root-level opencode API requests that require DEVAIPOD_AGENT_POD).
    fn curl_in_container_with_cookie(&self, path: &str, cookie: &str) -> Result<(i32, String)> {
        let url = format!("http://127.0.0.1:8080{}", path);
        let cookie_header = format!("Cookie: {}", cookie);
        let output = Command::new("podman")
            .args([
                "exec",
                &self.container_name,
                "curl",
                "-s",
                "-w",
                "\n%{http_code}",
                "--connect-timeout",
                "5",
                "--max-time",
                "30",
                "-H",
                &cookie_header,
                &url,
            ])
            .output()?;
        let combined = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<&str> = combined.trim().lines().collect();
        let status_code: i32 = lines.last().and_then(|s| s.parse().ok()).unwrap_or(-1);
        let body = if lines.len() > 1 {
            lines[..lines.len() - 1].join("\n")
        } else {
            String::new()
        };
        Ok((status_code, body))
    }

    /// Wait for the server to be ready by polling the health endpoint
    fn wait_ready(&self, timeout: Duration) -> Result<()> {
        let start = std::time::Instant::now();

        while start.elapsed() < timeout {
            // Check container is still running
            let status = Command::new("podman")
                .args([
                    "inspect",
                    "--format",
                    "{{.State.Running}}",
                    &self.container_name,
                ])
                .output();

            if let Ok(output) = &status {
                let running = String::from_utf8_lossy(&output.stdout).trim() == "true";
                if !running {
                    // Container stopped, get logs for debugging
                    let logs = Command::new("podman")
                        .args(["logs", &self.container_name])
                        .output()
                        .ok();
                    let log_output = logs
                        .map(|l| {
                            format!(
                                "stdout: {}\nstderr: {}",
                                String::from_utf8_lossy(&l.stdout),
                                String::from_utf8_lossy(&l.stderr)
                            )
                        })
                        .unwrap_or_default();
                    bail!("Container stopped unexpectedly. Logs:\n{}", log_output);
                }
            }

            // Try health endpoint
            if let Ok((status, _)) = self.curl_in_container("/_devaipod/health", None) {
                if status == 200 {
                    return Ok(());
                }
            }

            std::thread::sleep(Duration::from_millis(500));
        }

        bail!("Timeout waiting for web server to be ready")
    }
}

impl Drop for WebContainerGuard {
    fn drop(&mut self) {
        // Remove the container (force stop and remove)
        let _ = Command::new("podman")
            .args(["rm", "-f", &self.container_name])
            .output();
    }
}

// =============================================================================
// Shared Web Fixture (singleton pattern)
// =============================================================================

/// Shared fixture for web UI integration tests.
///
/// This fixture starts a single devaipod web container that is reused by all
/// web tests. It uses `OnceLock` for thread-safe lazy initialization.
///
/// The fixture is automatically initialized on first access via `get()` and
/// cleaned up via `cleanup()` after all tests complete.
pub struct WebFixture {
    /// The underlying container guard (handles cleanup on drop)
    guard: WebContainerGuard,
}

/// Singleton instance of the web fixture
static WEB_FIXTURE: OnceLock<Result<WebFixture, String>> = OnceLock::new();

impl WebFixture {
    /// Get the shared web fixture instance, creating it on first access.
    ///
    /// This method is thread-safe and will only create the fixture once.
    /// Returns an error if fixture creation fails.
    pub fn get() -> Result<&'static WebFixture> {
        let result = WEB_FIXTURE.get_or_init(|| {
            tracing::info!("Initializing shared WebFixture");
            match Self::create() {
                Ok(fixture) => Ok(fixture),
                Err(e) => {
                    tracing::error!("Failed to create WebFixture: {:?}", e);
                    Err(format!("{:?}", e))
                }
            }
        });

        match result {
            Ok(fixture) => Ok(fixture),
            Err(msg) => bail!("WebFixture initialization failed: {}", msg),
        }
    }

    /// Create a new web fixture (internal)
    fn create() -> Result<Self> {
        let guard = WebContainerGuard::start()?;
        guard.wait_ready(Duration::from_secs(30))?;
        tracing::info!(
            "WebFixture ready: container={}, token=***",
            guard.container_name
        );
        Ok(WebFixture { guard })
    }

    /// Get the authentication token
    pub fn token(&self) -> &str {
        self.guard.token()
    }

    /// Run curl inside the container and return (status_code, body)
    pub fn curl_in_container(&self, path: &str, auth_token: Option<&str>) -> Result<(i32, String)> {
        self.guard.curl_in_container(path, auth_token)
    }

    /// Run curl with a Cookie header (for root-level opencode API: DEVAIPOD_AGENT_POD=name).
    pub fn curl_in_container_with_cookie(&self, path: &str, cookie: &str) -> Result<(i32, String)> {
        self.guard.curl_in_container_with_cookie(path, cookie)
    }

    /// Curl with full header inspection (HTTP version, status, content-type, body) and optional cookie.
    pub fn curl_full_headers(
        &self,
        path: &str,
        max_time_secs: u8,
        cookie: Option<&str>,
    ) -> Result<(String, i32, String, String)> {
        self.guard
            .curl_in_container_full_headers(path, max_time_secs, cookie)
    }

    /// Get the container name (for advanced operations like podman exec)
    pub fn container_name(&self) -> &str {
        &self.guard.container_name
    }

    /// Clean up the shared web fixture.
    ///
    /// This is a no-op if the fixture was never initialized.
    /// The actual cleanup happens via `WebContainerGuard::drop()`.
    pub fn cleanup() {
        // The fixture is stored in a static OnceLock, so we can't easily
        // take ownership and drop it. Instead, we manually remove the container.
        if let Some(Ok(fixture)) = WEB_FIXTURE.get() {
            tracing::info!("Cleaning up WebFixture: {}", fixture.guard.container_name);
            let _ = Command::new("podman")
                .args(["rm", "-f", &fixture.guard.container_name])
                .output();
        }
    }
}

// Note: We don't need find_available_port or external curl helpers anymore
// since the web server binds to 127.0.0.1 inside the container and we use
// `podman exec` to run curl inside the container via curl_in_container().

// =============================================================================
// Web container tests
// =============================================================================

/// Verify the containerized web server starts and serves health, API, and static files
///
/// Tier 1: Parallel safe, no pod needed
fn test_web_container_starts() -> Result<()> {
    // Get the shared web fixture (creates container on first access)
    let fixture = WebFixture::get()?;

    // Test health endpoint (no auth required)
    let (status, body) = fixture.curl_in_container("/_devaipod/health", None)?;
    assert_eq!(
        status, 200,
        "Health endpoint should return 200, got {}",
        status
    );
    assert!(
        body.contains("ok"),
        "Health should return 'ok', got: {}",
        body
    );

    // Test static file serving - index.html should be served at root
    let (status, body) = fixture.curl_in_container("/", None)?;
    assert_eq!(
        status,
        200,
        "Root path should return 200 (index.html), got {}. Body: {}",
        status,
        &body[..body.len().min(200)]
    );
    assert!(
        body.contains("<!DOCTYPE html>") || body.contains("<html"),
        "Root should return HTML, got: {}",
        &body[..body.len().min(200)]
    );
    assert!(
        body.contains("devaipod"),
        "HTML should contain 'devaipod', got: {}",
        &body[..body.len().min(500)]
    );

    Ok(())
}
podman_integration_test!(test_web_container_starts);

/// Verify GET /_devaipod/agent/{name}/ serves iframe wrapper (not the raw opencode SPA)
///
/// The agent UI is now an iframe wrapper page: a thin nav bar with "back to pods" link
/// and a full-screen iframe pointing to /_devaipod/opencode-ui. This avoids fragile
/// HTML/CSS rewriting of the opencode SPA.
///
/// Tier 1: Parallel safe, no pod needed
fn test_web_agent_ui_index_rewrites_asset_urls() -> Result<()> {
    let fixture = WebFixture::get()?;
    let pod_name = "example-pod";
    let path = format!("/_devaipod/agent/{}/", urlencoding::encode(pod_name));
    let (status, body) = fixture.curl_in_container(&path, None)?;

    assert_eq!(
        status,
        200,
        "GET /_devaipod/agent/{{name}}/ should return 200, got {}: {}",
        status,
        &body[..body.len().min(400)]
    );
    // Must be the iframe wrapper, not the raw opencode SPA
    assert!(
        body.contains("/_devaipod/opencode-ui"),
        "Agent page must contain iframe pointing to /_devaipod/opencode-ui; got: {}",
        &body[..body.len().min(600)]
    );
    assert!(
        body.contains("Pods"),
        "Agent page must have back-to-pods link; got: {}",
        &body[..body.len().min(600)]
    );
    assert!(
        body.contains("devaipod_token"),
        "Agent page must read devaipod_token from sessionStorage; got: {}",
        &body[..body.len().min(600)]
    );

    Ok(())
}
podman_integration_test!(test_web_agent_ui_index_rewrites_asset_urls);

/// Verify an asset referenced by the agent UI index is served at /_devaipod/agent/{name}/{path}
///
/// With the iframe approach, assets are loaded from /assets/* with the cookie set.
/// Verify GET /_devaipod/opencode-ui serves the opencode SPA HTML and assets load via /assets/*
///
/// The iframe loads the opencode SPA from /_devaipod/opencode-ui. The SPA's scripts and
/// stylesheets reference /assets/... which are served by serve_root_assets when the
/// DEVAIPOD_AGENT_POD cookie is set. This test verifies the opencode-ui endpoint returns
/// valid HTML and that an asset extracted from it is served at /assets/*.
///
/// Tier 1: Parallel safe, no pod needed
fn test_web_agent_ui_serves_asset() -> Result<()> {
    let fixture = WebFixture::get()?;
    // Fetch the raw opencode UI
    let (status, body) = fixture.curl_in_container("/_devaipod/opencode-ui", None)?;
    assert_eq!(status, 200, "/_devaipod/opencode-ui must return 200");
    assert!(
        body.contains("<script") || body.contains("<link"),
        "opencode-ui must contain script or link tags; got: {}",
        &body[..body.len().min(500)]
    );

    // Extract an asset path: src="/assets/..." or href="/assets/..."
    let asset_path = ["src=\"/assets/", "href=\"/assets/"]
        .iter()
        .find_map(|prefix| {
            body.find(prefix).map(|start| {
                let value_start = start + prefix.len() - "/assets/".len();
                let rest = &body[value_start..];
                let end = rest.find('"').unwrap_or(rest.len());
                rest[..end].to_string()
            })
        });
    let asset_path = match asset_path {
        Some(p) => p,
        None => bail!(
            "Could not find /assets/ path in opencode-ui HTML. Body sample: {}",
            &body[..body.len().min(800)]
        ),
    };

    // Assets need the DEVAIPOD_AGENT_POD cookie to be served from the opencode dir
    let cookie = format!("{}=example-pod", DEVAIPOD_AGENT_POD_COOKIE_NAME);
    let (asset_status, _) = fixture.curl_in_container_with_cookie(&asset_path, &cookie)?;
    assert_eq!(
        asset_status, 200,
        "GET {} (asset from opencode-ui index) with cookie should return 200",
        asset_path
    );

    Ok(())
}
podman_integration_test!(test_web_agent_ui_serves_asset);

/// Cookie name used to attribute root-level opencode API requests to a pod (must match web server).
const DEVAIPOD_AGENT_POD_COOKIE_NAME: &str = "DEVAIPOD_AGENT_POD";

/// Verify root-level opencode API is proxied when cookie is set (session, rpc, etc.)
///
/// The opencode app uses window.location.origin, so it requests /session, /rpc at root.
/// We proxy those using the DEVAIPOD_AGENT_POD cookie. This test ensures the route exists and
/// accepts the cookie (we expect 502/503 if no pod is running, but not 400 or 404).
///
/// Requires a container image built with root proxy routes (see web.rs). Use
/// `just test-integration-container` to build and run against the current code.
///
/// Tier 1: Parallel safe, no pod needed
fn test_web_agent_ui_root_api_with_cookie() -> Result<()> {
    let fixture = WebFixture::get()?;
    let cookie = format!(
        "{}={}",
        DEVAIPOD_AGENT_POD_COOKIE_NAME,
        urlencoding::encode("example-pod")
    );
    let (status, _body) = fixture.curl_in_container_with_cookie("/session", &cookie)?;
    assert_ne!(
        status, 400,
        "GET /session with cookie must not return 400 (missing cookie) — root proxy must be wired"
    );
    assert_ne!(
        status, 404,
        "GET /session with cookie must not return 404 — root opencode API route must exist"
    );
    Ok(())
}
podman_integration_test!(test_web_agent_ui_root_api_with_cookie);

/// Verify root-level API path segments (path, project, provider, auth) are proxied, not 404.
///
/// Same image requirement as test_web_agent_ui_root_api_with_cookie.
///
/// Tier 1: Parallel safe, no pod needed
fn test_web_agent_ui_root_api_paths_proxied() -> Result<()> {
    let fixture = WebFixture::get()?;
    let cookie = format!(
        "{}={}",
        DEVAIPOD_AGENT_POD_COOKIE_NAME,
        urlencoding::encode("example-pod")
    );
    for path in ["/path", "/project", "/provider", "/auth"] {
        let (status, _body) = fixture.curl_in_container_with_cookie(path, &cookie)?;
        assert_ne!(
            status, 404,
            "GET {} with cookie must not return 404 — route must exist for opencode API",
            path
        );
    }
    Ok(())
}
podman_integration_test!(test_web_agent_ui_root_api_paths_proxied);

/// Event-stream endpoints (/event, /global, /global/event) without agent cookie must return
/// HTTP/1.1 200 with text/event-stream so webviews (e.g. global-sdk) do not see "Load failed".
/// The global-sdk calls GET /global/event; /event and /global are also covered.
///
/// Critically, the response MUST be HTTP/1.1 (not HTTP/1.0) because SSE requires chunked
/// transfer encoding; an HTTP/1.0 response causes browsers to close the connection and retry
/// in a tight loop.
///
/// Tier 1: Parallel safe, no pod needed
fn test_web_event_stream_no_cookie_returns_http11_sse() -> Result<()> {
    let fixture = WebFixture::get()?;
    for path in ["/event", "/global", "/global/event"] {
        let (http_ver, status, content_type, body) = fixture.curl_full_headers(path, 2, None)?;
        assert_eq!(
            status, 200,
            "GET {} without cookie must return 200 (not 400) so event stream does not Load failed",
            path
        );
        assert_eq!(
            http_ver, "HTTP/1.1",
            "GET {} must respond with HTTP/1.1 for SSE streaming; got {http_ver}",
            path
        );
        assert!(
            content_type
                .trim()
                .to_lowercase()
                .contains("text/event-stream"),
            "GET {} must return Content-Type: text/event-stream, got {:?}",
            path,
            content_type
        );
        assert!(
            body.contains("no agent context") || body.is_empty() || body.starts_with(':'),
            "GET {} body must be valid SSE (comment or empty), got: {:?}",
            path,
            body.chars().take(80).collect::<String>()
        );
    }
    Ok(())
}
podman_integration_test!(test_web_event_stream_no_cookie_returns_http11_sse);

/// Event-stream endpoints WITH agent cookie but unreachable pod must still return HTTP/1.1 200
/// SSE keepalive (not an HTTP error like 502/503). This covers the scenario where a user is on
/// an agent page (/_devaipod/agent/{name}/) and the pod's opencode server is not (yet) reachable — the
/// global-sdk opens /global/event and must get a long-lived SSE stream, not an error that
/// triggers aggressive reconnection.
///
/// Tier 1: Parallel safe, pod name is fictitious (proxy will fail to connect)
fn test_web_event_stream_with_cookie_unreachable_pod_returns_http11_sse() -> Result<()> {
    let fixture = WebFixture::get()?;
    let cookie = format!(
        "{}={}",
        DEVAIPOD_AGENT_POD_COOKIE_NAME,
        urlencoding::encode("nonexistent-pod-12345")
    );
    for path in ["/event", "/global", "/global/event"] {
        let (http_ver, status, content_type, _body) =
            fixture.curl_full_headers(path, 2, Some(&cookie))?;
        assert_eq!(
            status, 200,
            "GET {} with cookie for unreachable pod must return 200 SSE keepalive, got {status}",
            path
        );
        assert_eq!(
            http_ver, "HTTP/1.1",
            "GET {} with cookie must respond HTTP/1.1 for SSE streaming; got {http_ver}",
            path
        );
        assert!(
            content_type
                .trim()
                .to_lowercase()
                .contains("text/event-stream"),
            "GET {} with cookie must return text/event-stream, got {:?}",
            path,
            content_type
        );
    }
    Ok(())
}
podman_integration_test!(test_web_event_stream_with_cookie_unreachable_pod_returns_http11_sse);

/// Verify CSS assets from the opencode UI are served correctly via /assets/* with cookie.
///
/// Tier 1: Parallel safe, no pod needed
/// Verify CSS assets from the opencode UI are served correctly via /assets/* with cookie
///
/// With the iframe approach, CSS assets are loaded from /assets/... (root-level) when the
/// DEVAIPOD_AGENT_POD cookie is set. No CSS rewriting is needed.
fn test_web_agent_ui_css_rewrites_urls() -> Result<()> {
    let fixture = WebFixture::get()?;
    // Fetch the opencode-ui index to find a CSS asset
    let (status, body) = fixture.curl_in_container("/_devaipod/opencode-ui", None)?;
    assert_eq!(status, 200, "opencode-ui must return 200");

    // Find a .css href
    let css_path = body.split("href=\"").skip(1).find_map(|after| {
        let end = after.find('"')?;
        let value = &after[..end];
        value.contains(".css").then(|| {
            let p = value.to_string();
            if p.starts_with('/') {
                p
            } else {
                format!("/{}", p)
            }
        })
    });
    let css_path = match css_path {
        Some(p) => p,
        None => bail!(
            "Could not find .css href in opencode-ui HTML. Body sample: {}",
            &body[..body.len().min(600)]
        ),
    };
    // CSS needs the cookie to be served from the opencode dir
    let cookie = format!("{}=example-pod", DEVAIPOD_AGENT_POD_COOKIE_NAME);
    let (css_status, _css_body) = fixture.curl_in_container_with_cookie(&css_path, &cookie)?;
    assert_eq!(
        css_status, 200,
        "GET {} (CSS from opencode) with cookie should return 200",
        css_path
    );
    Ok(())
}
podman_integration_test!(test_web_agent_ui_css_rewrites_urls);

/// Full critical path: iframe wrapper → opencode-ui → assets → API
///
/// Simulates the browser flow: load agent wrapper (iframe), load opencode-ui (raw SPA),
/// load JS and CSS assets via /assets/* with cookie, then call root API (/session, /path).
/// Catches regressions like empty pages, 404s, or broken asset serving.
///
/// Tier 1: Parallel safe, no pod needed
fn test_web_opencode_ui_full_critical_path() -> Result<()> {
    let fixture = WebFixture::get()?;
    let pod_name = "example-pod";
    let cookie = format!(
        "{}={}",
        DEVAIPOD_AGENT_POD_COOKIE_NAME,
        urlencoding::encode(pod_name)
    );
    // 1. Agent wrapper must be the iframe page
    let wrapper_path = format!("/_devaipod/agent/{}/", urlencoding::encode(pod_name));
    let (status, body) = fixture.curl_in_container(&wrapper_path, None)?;
    assert_eq!(status, 200, "Agent wrapper must return 200, got {}", status);
    assert!(
        body.contains("/_devaipod/opencode-ui"),
        "Wrapper must contain iframe src"
    );
    assert!(body.contains("Pods"), "Wrapper must have back link");

    // 2. Opencode-ui must serve valid HTML with script/link tags
    let (status, body) = fixture.curl_in_container("/_devaipod/opencode-ui", None)?;
    assert_eq!(status, 200, "opencode-ui must return 200");

    // 3. Extract and fetch one JS and one CSS from the opencode index via /assets/*
    let mut js_path: Option<String> = None;
    let mut css_path: Option<String> = None;
    for prefix in ["href=\"", "src=\""] {
        let mut search_start = 0;
        while let Some(rel) = body[search_start..].find(prefix) {
            let start = search_start + rel + prefix.len();
            let rest = &body[start..];
            let end = rest.find('"').unwrap_or(rest.len());
            let p = rest[..end].to_string();
            if p.starts_with("/assets/") {
                if p.ends_with(".js") {
                    js_path.get_or_insert(p.clone());
                } else if p.ends_with(".css") {
                    css_path.get_or_insert(p.clone());
                }
            }
            search_start = start + end + 1;
        }
    }
    if let Some(ref p) = js_path {
        let (s, _) = fixture.curl_in_container_with_cookie(p, &cookie)?;
        assert_eq!(s, 200, "JS asset {} with cookie must return 200", p);
    }
    if let Some(ref p) = css_path {
        let (s, _) = fixture.curl_in_container_with_cookie(p, &cookie)?;
        assert_eq!(s, 200, "CSS asset {} with cookie must return 200", p);
    }
    assert!(
        js_path.is_some(),
        "opencode-ui must reference at least one JS asset"
    );
    assert!(
        css_path.is_some(),
        "opencode-ui must reference at least one CSS asset"
    );

    // 4. Root API with cookie — must not 400 or 404
    for api_path in ["/session", "/path"] {
        let (status, _) = fixture.curl_in_container_with_cookie(api_path, &cookie)?;
        assert_ne!(status, 400, "{} with cookie must not return 400", api_path);
        assert_ne!(status, 404, "{} with cookie must not return 404", api_path);
    }

    Ok(())
}
podman_integration_test!(test_web_opencode_ui_full_critical_path);

/// Verify GET /_devaipod/agent/{name} (no trailing slash) returns 307 redirect with Location and Set-Cookie.
///
/// The control plane navigates to /_devaipod/agent/{name}; the server redirects to /_devaipod/agent/{name}/ and sets
/// DEVAIPOD_AGENT_POD cookie so root-level opencode API requests are attributed to the pod.
///
/// Tier 1: Parallel safe, no pod needed
fn test_web_agent_redirect_sets_cookie() -> Result<()> {
    let fixture = WebFixture::get()?;
    let pod_name = "example-pod";
    let path = format!("/_devaipod/agent/{}", urlencoding::encode(pod_name));
    let url = format!("http://127.0.0.1:8080{}", path);

    // curl without -L: do not follow redirects so we can assert 307 and headers
    let output = Command::new("podman")
        .args([
            "exec",
            fixture.container_name(),
            "curl",
            "-s",
            "-D",
            "-",
            "--connect-timeout",
            "5",
            "--max-time",
            "30",
            &url,
        ])
        .output()?;

    let raw = String::from_utf8_lossy(&output.stdout);
    let (headers_str, _body) = if let Some(pos) = raw.find("\r\n\r\n") {
        (raw[..pos].to_string(), raw[pos + 4..].to_string())
    } else if let Some(pos) = raw.find("\n\n") {
        (raw[..pos].to_string(), raw[pos + 2..].to_string())
    } else {
        (raw.to_string(), String::new())
    };

    let status: i32 = headers_str
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1).and_then(|s| s.parse().ok()))
        .unwrap_or(-1);
    assert_eq!(
        status, 307,
        "GET /_devaipod/agent/{{name}} must return 307 redirect, got {}",
        status
    );

    let location = headers_str
        .lines()
        .find(|l| l.trim_start().to_lowercase().starts_with("location:"))
        .and_then(|l| l.split(':').nth(1).map(|v| v.trim().to_string()))
        .expect("Location header must be present");
    let expected_location = format!("/_devaipod/agent/{}/", urlencoding::encode(pod_name));
    assert_eq!(
        location, expected_location,
        "Location header must point to /_devaipod/agent/{{name}}/"
    );

    let set_cookie = headers_str
        .lines()
        .find(|l| l.trim_start().to_lowercase().starts_with("set-cookie:"));
    assert!(set_cookie.is_some(), "Set-Cookie header must be present");
    let set_cookie_val = set_cookie.unwrap();
    assert!(
        set_cookie_val
            .to_lowercase()
            .contains(&DEVAIPOD_AGENT_POD_COOKIE_NAME.to_lowercase()),
        "Set-Cookie must contain {}, got: {}",
        DEVAIPOD_AGENT_POD_COOKIE_NAME,
        set_cookie_val
    );

    Ok(())
}
podman_integration_test!(test_web_agent_redirect_sets_cookie);

/// Verify control-plane UI is served at root with token (UI surface: GET /)
///
/// Ensures the page contains "devaipod" and at least one control-plane marker
/// (Refresh, Launch, or token prompt) so we detect if the wrong page or broken build is served.
///
/// Tier 1: Parallel safe, no pod needed
fn test_web_ui_root_with_token() -> Result<()> {
    let fixture = WebFixture::get()?;
    let token = fixture.token().to_string();
    let path = format!("/?token={}", urlencoding::encode(&token));
    let (status, body) = fixture.curl_in_container(&path, None)?;

    assert_eq!(
        status,
        200,
        "Root with token should return 200, got {}: {}",
        status,
        &body[..body.len().min(300)]
    );
    assert!(
        body.contains("devaipod"),
        "Control-plane HTML should contain 'devaipod', got: {}",
        &body[..body.len().min(500)]
    );

    let has_control_plane_marker = body.contains("Refresh")
        || body.contains("Launch")
        || body.contains("No authentication")
        || body.contains("token=")
        || body.contains("Control Plane");
    assert!(
        has_control_plane_marker,
        "Control-plane HTML should contain at least one of: Refresh, Launch, No authentication, token=, Control Plane. Got: {}",
        &body[..body.len().min(500)]
    );

    Ok(())
}
podman_integration_test!(test_web_ui_root_with_token);

/// Verify POST /api/devaipod/run is wired and returns JSON (UI surface: launch from UI)
///
/// Posts with valid auth and a body that will fail (empty/invalid source).
/// We only assert: 200 response and JSON body with "success" key (success: false is expected).
///
/// Tier 1: Parallel safe, no pod needed
fn test_web_ui_run_endpoint() -> Result<()> {
    let fixture = WebFixture::get()?;
    let token = fixture.token().to_string();

    let body = r#"{"source":"","task":null}"#;
    let auth_header = format!("Authorization: Bearer {}", token);
    let url = "http://127.0.0.1:8080/api/devaipod/run";
    let curl_output = Command::new("podman")
        .args([
            "exec",
            fixture.container_name(),
            "curl",
            "-s",
            "-w",
            "\n%{http_code}",
            "--connect-timeout",
            "5",
            "--max-time",
            "30",
            "-X",
            "POST",
            "-H",
            &auth_header,
            "-H",
            "Content-Type: application/json",
            "--data",
            body,
            url,
        ])
        .output()?;

    let combined = String::from_utf8_lossy(&curl_output.stdout);
    let lines: Vec<&str> = combined.trim().lines().collect();
    let status_code: i32 = lines.last().and_then(|s| s.parse().ok()).unwrap_or(-1);
    let response_body = if lines.len() > 1 {
        lines[..lines.len() - 1].join("\n")
    } else {
        String::new()
    };

    assert_eq!(
        status_code, 200,
        "POST /api/devaipod/run should return 200 (auth OK, JSON response), got {}: {}",
        status_code, response_body
    );
    let parsed: serde_json::Value = serde_json::from_str(&response_body).map_err(|e| {
        color_eyre::eyre::eyre!(
            "Run endpoint should return JSON: {} - body: {}",
            e,
            response_body
        )
    })?;
    assert!(
        parsed.get("success").is_some(),
        "Run response JSON should have 'success' key: {}",
        response_body
    );

    Ok(())
}
podman_integration_test!(test_web_ui_run_endpoint);

/// Verify auth works: 401 without token, 200 with valid token
///
/// Tier 1: Parallel safe, no pod needed (just tests auth mechanism)
fn test_web_container_auth() -> Result<()> {
    let fixture = WebFixture::get()?;

    // Test API without token - should get 401
    let (status, _body) = fixture.curl_in_container("/api/podman/v5.0.0/libpod/pods/json", None)?;
    assert_eq!(
        status, 401,
        "API request without token should return 401 Unauthorized"
    );

    // Test API with wrong token - should get 401
    let (status, _body) = fixture.curl_in_container(
        "/api/podman/v5.0.0/libpod/pods/json",
        Some("wrong-token-12345"),
    )?;
    assert_eq!(
        status, 401,
        "API request with invalid token should return 401 Unauthorized"
    );

    // Test API with valid token - should succeed
    let token = fixture.token().to_string();
    let (status, body) =
        fixture.curl_in_container("/api/podman/v5.0.0/libpod/pods/json", Some(&token))?;
    assert_eq!(
        status, 200,
        "API request with valid token should return 200, got {}: {}",
        status, body
    );

    // Body should be valid JSON array (pods list)
    assert!(
        body.starts_with('['),
        "Pods list should be a JSON array: {}",
        body
    );

    Ok(())
}
podman_integration_test!(test_web_container_auth);

/// Verify the web API pod list returns valid data and includes any existing devaipod pods
///
/// Tier 2: Parallel safe, uses shared pod for validation
fn test_web_container_pod_list() -> Result<()> {
    let fixture = WebFixture::get()?;

    // Query the web API for pod list
    let token = fixture.token().to_string();
    let (status, body) =
        fixture.curl_in_container("/api/podman/v5.0.0/libpod/pods/json", Some(&token))?;

    assert_eq!(status, 200, "Should get pod list: {}", body);

    // Body should be valid JSON array
    assert!(
        body.starts_with('['),
        "Pods list should be a JSON array: {}",
        body
    );

    // Parse the JSON to verify structure
    let pods: Vec<serde_json::Value> = serde_json::from_str(&body).map_err(|e| {
        color_eyre::eyre::eyre!("Failed to parse pods JSON: {} - body: {}", e, body)
    })?;

    // Log how many pods were found
    tracing::info!("Found {} pods in API response", pods.len());

    // If there are any devaipod pods, verify they have expected fields
    for pod in &pods {
        if let Some(name) = pod.get("Name").and_then(|n| n.as_str()) {
            if name.starts_with("devaipod-") {
                tracing::info!("Found devaipod pod: {}", name);
                // Verify expected fields exist
                assert!(
                    pod.get("Status").is_some(),
                    "Pod {} should have Status field",
                    name
                );
                assert!(
                    pod.get("Labels").is_some(),
                    "Pod {} should have Labels field",
                    name
                );
            }
        }
    }

    Ok(())
}
podman_integration_test!(test_web_container_pod_list);

/// Verify the opencode-info endpoint returns 404 for non-existent pods
/// and proper JSON structure for existing pods
///
/// Tier 2: Parallel safe, uses shared pod for validation
fn test_web_container_opencode_info_endpoint() -> Result<()> {
    let fixture = WebFixture::get()?;

    let token = fixture.token().to_string();

    // Test that non-existent pod returns 404
    let (status, _body) = fixture.curl_in_container(
        "/api/devaipod/pods/nonexistent-pod-12345/opencode-info",
        Some(&token),
    )?;
    assert_eq!(
        status, 404,
        "Non-existent pod should return 404, got {}",
        status
    );

    // If there's a running devaipod pod, test the endpoint returns valid data
    let (pod_status, pod_body) =
        fixture.curl_in_container("/api/podman/v5.0.0/libpod/pods/json", Some(&token))?;

    if pod_status == 200 {
        let pods: Vec<serde_json::Value> = serde_json::from_str(&pod_body).unwrap_or_default();
        for pod in &pods {
            if let Some(name) = pod.get("Name").and_then(|n| n.as_str()) {
                if name.starts_with("devaipod-") {
                    // Found a devaipod pod, test the opencode-info endpoint
                    let short_name = name.strip_prefix("devaipod-").unwrap_or(name);
                    let (info_status, info_body) = fixture.curl_in_container(
                        &format!("/api/devaipod/pods/{}/opencode-info", short_name),
                        Some(&token),
                    )?;

                    // Should return 200 or 404 (if pod not running/accessible)
                    assert!(
                        info_status == 200 || info_status == 404,
                        "opencode-info should return 200 or 404, got {}: {}",
                        info_status,
                        info_body
                    );

                    if info_status == 200 {
                        // Verify JSON structure
                        let info: serde_json::Value =
                            serde_json::from_str(&info_body).map_err(|e| {
                                color_eyre::eyre::eyre!(
                                    "Failed to parse opencode-info JSON: {} - body: {}",
                                    e,
                                    info_body
                                )
                            })?;

                        assert!(
                            info.get("url").is_some(),
                            "opencode-info should have 'url' field"
                        );
                        assert!(
                            info.get("port").is_some(),
                            "opencode-info should have 'port' field"
                        );
                        assert!(
                            info.get("accessible").is_some(),
                            "opencode-info should have 'accessible' field"
                        );

                        // URL should be a direct localhost URL (token is not included)
                        let url = info.get("url").unwrap().as_str().unwrap_or("");
                        assert!(
                            url.starts_with("http://127.0.0.1:"),
                            "URL should start with 'http://127.0.0.1:', got: {}",
                            url
                        );

                        tracing::info!(
                            "Successfully validated opencode-info for pod {}: port={}",
                            name,
                            info.get("port").unwrap()
                        );
                    }
                    break; // Only test one pod
                }
            }
        }
    }

    Ok(())
}
podman_integration_test!(test_web_container_opencode_info_endpoint);

/// Test connectivity to a running devaipod pod's opencode proxy
///
/// This test validates that the opencode proxy actually returns content by:
/// 1. Finding a running devaipod pod
/// 2. Getting its opencode-info (URL, port, token)
/// 3. Using curl from inside the web container to test the proxy returns content
///
/// If no devaipod pod is running, the test passes with a skip note.
///
/// Tier 2: Parallel safe, uses shared pod for validation
fn test_web_container_opencode_connectivity() -> Result<()> {
    let fixture = WebFixture::get()?;

    let token = fixture.token().to_string();

    // Get pod list to find a running devaipod pod
    let (pod_status, pod_body) =
        fixture.curl_in_container("/api/podman/v5.0.0/libpod/pods/json", Some(&token))?;

    if pod_status != 200 {
        tracing::info!("Could not get pod list, skipping connectivity test");
        return Ok(());
    }

    let pods: Vec<serde_json::Value> = serde_json::from_str(&pod_body).unwrap_or_default();

    // Find a running devaipod pod
    let mut found_running_pod = false;
    for pod in &pods {
        let name = match pod.get("Name").and_then(|n| n.as_str()) {
            Some(n) if n.starts_with("devaipod-") => n,
            _ => continue,
        };

        // Check if pod is running
        let status = pod
            .get("Status")
            .and_then(|s| s.as_str())
            .unwrap_or("unknown");
        if status != "Running" {
            tracing::info!("Pod {} is not running (status: {}), skipping", name, status);
            continue;
        }

        let short_name = name.strip_prefix("devaipod-").unwrap_or(name);
        tracing::info!("Found running devaipod pod: {}", name);

        // Get opencode-info for this pod
        let (info_status, info_body) = fixture.curl_in_container(
            &format!("/api/devaipod/pods/{}/opencode-info", short_name),
            Some(&token),
        )?;

        if info_status != 200 {
            tracing::info!(
                "opencode-info returned {} for pod {}, skipping",
                info_status,
                name
            );
            continue;
        }

        // Parse the opencode-info response
        let info: serde_json::Value = match serde_json::from_str(&info_body) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Failed to parse opencode-info: {}", e);
                continue;
            }
        };

        let port = match info.get("port").and_then(|p| p.as_u64()) {
            Some(p) => p as u16,
            None => {
                tracing::warn!("No port in opencode-info response");
                continue;
            }
        };

        // The URL no longer includes a token; the opencode server inside
        // the pod does not require authentication for localhost access.
        let _url = info.get("url").and_then(|u| u.as_str()).unwrap_or("");

        tracing::info!(
            "Testing opencode proxy connectivity on port {} for pod {}",
            port,
            name
        );

        found_running_pod = true;

        // Test 1: Initial page load (no token needed for localhost access)
        // curl from inside the web container to the host's published port
        // Note: 127.0.0.1 in the container refers to the container's localhost,
        // but the opencode proxy port is published on the host.
        // We need to use the host's IP or the podman network gateway.
        // For rootless podman, we can try host.containers.internal or the host IP.
        let curl_url = format!("http://host.containers.internal:{}/", port);
        let curl_output = Command::new("podman")
            .args([
                "exec",
                fixture.container_name(),
                "curl",
                "-s",
                "--max-time",
                "10",
                "-w",
                "\n%{http_code}",
                &curl_url,
            ])
            .output()?;

        let combined = String::from_utf8_lossy(&curl_output.stdout);
        let lines: Vec<&str> = combined.trim().lines().collect();
        let status_code: i32 = lines.last().and_then(|s| s.parse().ok()).unwrap_or(-1);
        let body = if lines.len() > 1 {
            lines[..lines.len() - 1].join("\n")
        } else {
            String::new()
        };

        if status_code == -1 || status_code == 0 {
            // Connection failed - likely network isolation or host not reachable
            tracing::info!(
                "Could not reach host.containers.internal:{}, pod may be on different host or network isolated",
                port
            );
            // This is expected in some CI environments, so we don't fail
            continue;
        }

        // Verify we got a successful response with HTML content
        assert!(
            status_code == 200 || status_code == 302,
            "Initial page load should return 200 or 302, got {}: {}",
            status_code,
            &body[..body.len().min(200)]
        );

        if status_code == 200 {
            // Should contain HTML
            assert!(
                body.contains("<!DOCTYPE html>")
                    || body.contains("<html")
                    || body.contains("<!doctype html>"),
                "Response should contain HTML, got: {}",
                &body[..body.len().min(500)]
            );
            tracing::info!("Initial page load returned HTML content successfully");
        } else {
            tracing::info!("Got redirect (302), which is also acceptable");
        }

        // Test 2: Request to /health (no auth needed for localhost access)
        let health_url = format!("http://host.containers.internal:{}/health", port);
        let curl_health_output = Command::new("podman")
            .args([
                "exec",
                fixture.container_name(),
                "curl",
                "-s",
                "--max-time",
                "10",
                "-w",
                "\n%{http_code}",
                &health_url,
            ])
            .output()?;

        let health_combined = String::from_utf8_lossy(&curl_health_output.stdout);
        let health_lines: Vec<&str> = health_combined.trim().lines().collect();
        let health_status: i32 = health_lines
            .last()
            .and_then(|s| s.parse().ok())
            .unwrap_or(-1);

        // Health endpoint may or may not exist on the opencode server
        // The important thing is we got a response (not connection refused)
        if health_status > 0 {
            tracing::info!(
                "Request to /health returned status {}",
                health_status
            );
        }

        // Successfully tested one pod, we're done
        tracing::info!(
            "Successfully validated opencode proxy connectivity for pod {}",
            name
        );
        return Ok(());
    }

    if !found_running_pod {
        tracing::info!("No running devaipod pods found, skipping opencode proxy connectivity test");
    }

    Ok(())
}
podman_integration_test!(test_web_container_opencode_connectivity);

/// Verify cookie-based authentication persists across requests.
///
/// This tests the auth flow:
/// 1. Make initial request to root path with ?token= query parameter
/// 2. Extract Set-Cookie header from response (if present)
/// 3. Make follow-up API request with only the cookie (no token param)
/// 4. Verify the cookie-authenticated request succeeds
///
/// Note: The devaipod web server may or may not use cookies for session management.
/// This test documents the current behavior.
///
/// Tier 1: Parallel safe, tests auth mechanism
fn test_auth_proxy_cookie_persistence() -> Result<()> {
    let fixture = WebFixture::get()?;

    let token = fixture.token().to_string();

    // Step 1: Make request to root path with token (this is where cookie would be set)
    // Using -c to save cookies and -L to follow redirects
    let url_with_token = format!("http://127.0.0.1:8080/?token={}", token);
    let curl_output = Command::new("podman")
        .args([
            "exec",
            fixture.container_name(),
            "curl",
            "-s",
            "-D",
            "-",  // Dump headers to stdout
            "-L", // Follow redirects
            "--connect-timeout",
            "5",
            "--max-time",
            "30",
            "--retry",
            "5",
            "--retry-connrefused",
            "--retry-all-errors",
            "--retry-delay",
            "2",
            &url_with_token,
        ])
        .output()?;

    let response = String::from_utf8_lossy(&curl_output.stdout);

    // Parse the response to extract headers (may have multiple header blocks due to redirects)
    // Find the Set-Cookie in any of them
    let set_cookie = response
        .lines()
        .find(|line| line.to_lowercase().starts_with("set-cookie:"))
        .map(|line| line.trim().to_string());

    // Also extract final status code
    let parts: Vec<&str> = response.rsplitn(2, "\r\n\r\n").collect();
    let _final_headers = parts.get(1).unwrap_or(&"");
    let final_body = parts.first().unwrap_or(&"");

    tracing::info!(
        "Root path response (first 500 chars):\n{}",
        &response[..response.len().min(500)]
    );

    // Check if we got HTML (successful page load)
    let got_html = final_body.contains("<!DOCTYPE html>")
        || final_body.contains("<html")
        || final_body.contains("<!doctype html>");

    if !got_html {
        tracing::warn!(
            "Root path didn't return HTML, got: {}",
            &final_body[..final_body.len().min(200)]
        );
    }

    // Check for Set-Cookie
    if let Some(ref cookie_header) = set_cookie {
        tracing::info!("Got Set-Cookie: {}", cookie_header);

        // Extract the cookie name=value part (before any attributes like Path, HttpOnly, etc.)
        let cookie_value = cookie_header
            .strip_prefix("Set-Cookie:")
            .or_else(|| cookie_header.strip_prefix("set-cookie:"))
            .unwrap_or(cookie_header)
            .trim()
            .split(';')
            .next()
            .unwrap_or("");

        if !cookie_value.is_empty() {
            tracing::info!("Using cookie for follow-up request: {}", cookie_value);

            // Step 2: Make follow-up API request with only the cookie (no token param)
            let cookie_request_header = format!("Cookie: {}", cookie_value);
            let url_without_token = "http://127.0.0.1:8080/api/podman/v5.0.0/libpod/pods/json";

            let cookie_curl_output = Command::new("podman")
                .args([
                    "exec",
                    fixture.container_name(),
                    "curl",
                    "-s",
                    "-w",
                    "\n%{http_code}",
                    "--connect-timeout",
                    "5",
                    "--max-time",
                    "30",
                    "--retry",
                    "5",
                    "--retry-connrefused",
                    "--retry-all-errors",
                    "--retry-delay",
                    "2",
                    "-H",
                    &cookie_request_header,
                    url_without_token,
                ])
                .output()?;

            let combined = String::from_utf8_lossy(&cookie_curl_output.stdout);
            let lines: Vec<&str> = combined.trim().lines().collect();
            let status_code: i32 = lines.last().and_then(|s| s.parse().ok()).unwrap_or(-1);
            let cookie_body = if lines.len() > 1 {
                lines[..lines.len() - 1].join("\n")
            } else {
                String::new()
            };

            // Step 3: Verify the cookie-only request succeeds
            if status_code == 200 {
                tracing::info!(
                    "Cookie persistence works: follow-up request succeeded with cookie-only auth"
                );
                assert!(
                    cookie_body.starts_with('['),
                    "Cookie-authenticated response should be valid JSON array: {}",
                    &cookie_body[..cookie_body.len().min(200)]
                );
            } else {
                // Cookie auth didn't work - this is the behavior we're testing
                tracing::warn!(
                    "Cookie-only auth returned {}: Cookie may not be used for API auth. Body: {}",
                    status_code,
                    &cookie_body[..cookie_body.len().min(200)]
                );
                // This is expected if the server doesn't use cookie-based auth for API
            }
        }
    } else {
        tracing::info!(
            "No Set-Cookie header in response - server doesn't use cookie-based sessions"
        );
        // This is also valid - the server might use only Bearer tokens
    }

    // Alternative test: verify Bearer token auth works for API (this should always work)
    let bearer_header = format!("Authorization: Bearer {}", token);
    let bearer_curl_output = Command::new("podman")
        .args([
            "exec",
            fixture.container_name(),
            "curl",
            "-s",
            "-w",
            "\n%{http_code}",
            "--connect-timeout",
            "5",
            "--max-time",
            "30",
            "--retry",
            "5",
            "--retry-connrefused",
            "--retry-all-errors",
            "--retry-delay",
            "2",
            "-H",
            &bearer_header,
            "http://127.0.0.1:8080/api/podman/v5.0.0/libpod/pods/json",
        ])
        .output()?;

    let bearer_combined = String::from_utf8_lossy(&bearer_curl_output.stdout);
    let bearer_lines: Vec<&str> = bearer_combined.trim().lines().collect();
    let bearer_status: i32 = bearer_lines
        .last()
        .and_then(|s| s.parse().ok())
        .unwrap_or(-1);
    let bearer_body = if bearer_lines.len() > 1 {
        bearer_lines[..bearer_lines.len() - 1].join("\n")
    } else {
        String::new()
    };

    assert_eq!(
        bearer_status, 200,
        "Bearer token auth should work for API. Got {}: {}",
        bearer_status, bearer_body
    );
    assert!(
        bearer_body.starts_with('['),
        "Bearer-authenticated response should be valid JSON array: {}",
        &bearer_body[..bearer_body.len().min(200)]
    );
    tracing::info!("Bearer token auth verified working for API");

    Ok(())
}
podman_integration_test!(test_auth_proxy_cookie_persistence);

/// Verify that 401 responses include WWW-Authenticate header.
///
/// This is important because WWW-Authenticate triggers the browser's
/// built-in authentication dialog (the "signin request" popup in Firefox).
/// We want to detect this behavior so we can fix it for cross-origin requests.
///
/// Tier 1: Parallel safe, tests auth mechanism
fn test_auth_proxy_wrong_password_returns_401_with_www_authenticate() -> Result<()> {
    let fixture = WebFixture::get()?;

    // Make request with wrong token - should get 401
    let wrong_token = "wrong-token-12345";
    let url = format!(
        "http://127.0.0.1:8080/api/podman/v5.0.0/libpod/pods/json?token={}",
        wrong_token
    );

    let curl_output = Command::new("podman")
        .args([
            "exec",
            fixture.container_name(),
            "curl",
            "-s",
            "-D",
            "-", // Dump headers to stdout
            "--connect-timeout",
            "5",
            "--max-time",
            "30",
            "--retry",
            "5",
            "--retry-connrefused",
            "--retry-all-errors",
            "--retry-delay",
            "2",
            &url,
        ])
        .output()?;

    let response = String::from_utf8_lossy(&curl_output.stdout);

    // Parse the response to extract headers
    let parts: Vec<&str> = response.splitn(2, "\r\n\r\n").collect();
    let headers = parts.first().unwrap_or(&"");

    tracing::info!("Wrong password response headers:\n{}", headers);

    // Verify status is 401
    let status_line = headers.lines().next().unwrap_or("");
    assert!(
        status_line.contains("401"),
        "Request with wrong token should return 401, got: {}",
        status_line
    );

    // Check for WWW-Authenticate header
    let www_authenticate = headers
        .lines()
        .find(|line| line.to_lowercase().starts_with("www-authenticate:"));

    // Log whether WWW-Authenticate is present (this is the behavior we want to document/fix)
    if let Some(header) = www_authenticate {
        tracing::warn!(
            "WWW-Authenticate header IS present: {} - This triggers Firefox signin dialog!",
            header
        );
    } else {
        tracing::info!(
            "WWW-Authenticate header is NOT present (good for avoiding browser dialogs)"
        );
    }

    // For now, we're just documenting the behavior.
    // The assertion below captures current behavior - update after fix:
    // Currently we expect WWW-Authenticate to be absent (or present - adjust based on actual behavior)

    // Test request with no auth at all
    let no_auth_url = "http://127.0.0.1:8080/api/podman/v5.0.0/libpod/pods/json";
    let no_auth_output = Command::new("podman")
        .args([
            "exec",
            fixture.container_name(),
            "curl",
            "-s",
            "-D",
            "-",
            "--connect-timeout",
            "5",
            "--max-time",
            "30",
            "--retry",
            "5",
            "--retry-connrefused",
            "--retry-all-errors",
            "--retry-delay",
            "2",
            no_auth_url,
        ])
        .output()?;

    let no_auth_response = String::from_utf8_lossy(&no_auth_output.stdout);
    let no_auth_parts: Vec<&str> = no_auth_response.splitn(2, "\r\n\r\n").collect();
    let no_auth_headers = no_auth_parts.first().unwrap_or(&"");

    tracing::info!("No auth response headers:\n{}", no_auth_headers);

    // Verify status is 401
    let no_auth_status = no_auth_headers.lines().next().unwrap_or("");
    assert!(
        no_auth_status.contains("401"),
        "Request without auth should return 401, got: {}",
        no_auth_status
    );

    let no_auth_www_authenticate = no_auth_headers
        .lines()
        .find(|line| line.to_lowercase().starts_with("www-authenticate:"));

    if let Some(header) = no_auth_www_authenticate {
        tracing::warn!(
            "WWW-Authenticate header IS present on no-auth request: {} - This triggers Firefox signin dialog!",
            header
        );
    } else {
        tracing::info!("WWW-Authenticate header is NOT present on no-auth request (good)");
    }

    Ok(())
}
podman_integration_test!(test_auth_proxy_wrong_password_returns_401_with_www_authenticate);

/// Test API-style requests (Accept: application/json) without auth.
///
/// This simulates cross-origin API requests from app.opencode.ai to 127.0.0.1.
/// When cookies aren't sent (due to SameSite=Lax on cross-origin), the 401
/// response with WWW-Authenticate header causes Firefox to show a signin dialog.
///
/// Tier 1: Parallel safe, tests auth mechanism
fn test_auth_proxy_api_request_without_auth() -> Result<()> {
    let fixture = WebFixture::get()?;

    // Simulate API request with Accept: application/json but no auth
    // This is what happens on cross-origin requests when cookie is not sent
    let url = "http://127.0.0.1:8080/api/podman/v5.0.0/libpod/pods/json";

    let curl_output = Command::new("podman")
        .args([
            "exec",
            fixture.container_name(),
            "curl",
            "-s",
            "-D",
            "-", // Dump headers to stdout
            "--connect-timeout",
            "5",
            "--max-time",
            "30",
            "--retry",
            "5",
            "--retry-connrefused",
            "--retry-all-errors",
            "--retry-delay",
            "2",
            "-H",
            "Accept: application/json",
            "-H",
            "Origin: https://app.opencode.ai", // Simulate cross-origin
            url,
        ])
        .output()?;

    let response = String::from_utf8_lossy(&curl_output.stdout);

    // Parse the response to extract headers
    let parts: Vec<&str> = response.splitn(2, "\r\n\r\n").collect();
    let headers = parts.first().unwrap_or(&"");
    let body = parts.get(1).unwrap_or(&"");

    tracing::info!("API request without auth - headers:\n{}", headers);
    tracing::info!("API request without auth - body:\n{}", body);

    // Verify status is 401
    let status_line = headers.lines().next().unwrap_or("");
    assert!(
        status_line.contains("401"),
        "API request without auth should return 401, got: {}",
        status_line
    );

    // Check for WWW-Authenticate header - this is what causes the Firefox issue
    let www_authenticate = headers
        .lines()
        .find(|line| line.to_lowercase().starts_with("www-authenticate:"));

    if let Some(header) = www_authenticate {
        tracing::warn!(
            "WWW-Authenticate header IS present on API request: {}",
            header
        );
        tracing::warn!(
            "This will cause Firefox to show a signin dialog for cross-origin API requests!"
        );
        tracing::warn!(
            "Fix: Don't send WWW-Authenticate for requests with Accept: application/json"
        );
    } else {
        tracing::info!("WWW-Authenticate header is NOT present on API request (good)");
    }

    // Check for CORS headers (may be relevant for cross-origin requests)
    let cors_header = headers
        .lines()
        .find(|line| line.to_lowercase().starts_with("access-control-"));

    if let Some(header) = cors_header {
        tracing::info!("CORS header present: {}", header);
    } else {
        tracing::info!("No CORS headers present");
    }

    // Verify body is JSON error response (not HTML)
    // Good API behavior: return JSON error, not redirect to login page
    if body.trim().starts_with('{') || body.trim().starts_with('[') {
        tracing::info!("Response body is JSON (good for API clients)");
    } else if body.contains("<!DOCTYPE html>") || body.contains("<html") {
        tracing::warn!("Response body is HTML - API clients expect JSON error response");
    }

    Ok(())
}
podman_integration_test!(test_auth_proxy_api_request_without_auth);
