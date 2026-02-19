//! Integration tests for devaipod
//!
//! Run with: cargo test -p integration-tests
//! Or: DEVAIPOD_PATH=./target/debug/devaipod cargo test -p integration-tests

use std::path::PathBuf;
use std::process::{Command, Output};

use color_eyre::eyre::{eyre, Context, Result};
use libtest_mimic::{Arguments, Trial};
use xshell::{cmd, Shell};

// Re-export from lib for test registration
pub(crate) use integration_tests::{
    integration_test, podman_integration_test, readonly_test, SharedFixture, INTEGRATION_TESTS,
    READONLY_INTEGRATION_TESTS,
};

mod tests;

// Re-export WebFixture for cleanup
use tests::WebFixture;

/// Create a new xshell Shell for running commands
pub(crate) fn shell() -> Result<Shell> {
    Shell::new().map_err(|e| eyre!("Failed to create shell: {}", e))
}

/// Get the workspace root directory by finding the Cargo.lock file
fn find_workspace_root() -> Option<std::path::PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join("Cargo.lock").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Check if the devaipod container is running (not just existing; exec requires running state)
fn devaipod_container_running() -> bool {
    let output = Command::new("podman")
        .args(["inspect", "--format", "{{.State.Running}}", "devaipod"])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).trim() == "true"
        }
        _ => false,
    }
}

/// Describes how to run the devaipod command
pub(crate) enum DevaipodCommand {
    /// Run via podman exec into the devaipod container
    Container,
    /// Run the binary directly on the host (with DEVAIPOD_HOST_MODE=1)
    Host(String),
}

/// Get the devaipod command configuration
///
/// If the devaipod container is running, returns Container mode.
/// Otherwise checks DEVAIPOD_PATH env var, then looks for the binary in target directories.
pub(crate) fn get_devaipod_command() -> Result<DevaipodCommand> {
    // If DEVAIPOD_HOST_MODE is explicitly set, use host mode
    let explicit_host_mode = std::env::var("DEVAIPOD_HOST_MODE").is_ok();

    // Check if devaipod container is running (preferred for integration tests)
    if !explicit_host_mode && devaipod_container_running() {
        return Ok(DevaipodCommand::Container);
    }

    // Fall back to host mode with binary path
    if let Ok(path) = std::env::var("DEVAIPOD_PATH") {
        // Convert to absolute path if relative
        let path = std::path::PathBuf::from(&path);
        if path.is_relative() {
            // Resolve relative to workspace root (where Cargo.lock is)
            if let Some(workspace_root) = find_workspace_root() {
                let abs_path = workspace_root.join(&path);
                if abs_path.exists() {
                    return Ok(DevaipodCommand::Host(
                        abs_path.canonicalize()?.to_string_lossy().to_string(),
                    ));
                }
            }
            // Try current directory as fallback
            let cwd = std::env::current_dir()?;
            let abs_path = cwd.join(&path);
            if abs_path.exists() {
                return Ok(DevaipodCommand::Host(
                    abs_path.canonicalize()?.to_string_lossy().to_string(),
                ));
            }
            return Err(eyre!("Cannot find devaipod binary at {}", path.display()));
        }
        return Ok(DevaipodCommand::Host(path.to_string_lossy().to_string()));
    }

    // Look for the binary in target directories (e.g. container not running; fall back to host binary)
    let workspace_root = find_workspace_root();
    let candidates = ["target/debug/devaipod", "target/release/devaipod"];
    for candidate in candidates {
        let path = if let Some(ref root) = workspace_root {
            root.join(candidate)
        } else {
            std::path::PathBuf::from(candidate)
        };
        if path.exists() {
            return Ok(DevaipodCommand::Host(
                path.canonicalize()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| path.to_string_lossy().to_string()),
            ));
        }
    }

    // Fall back to hoping it's in PATH
    Ok(DevaipodCommand::Host("devaipod".to_string()))
}

/// Check if podman is available
pub(crate) fn podman_available() -> bool {
    let Ok(sh) = Shell::new() else {
        return false;
    };
    cmd!(sh, "podman --version")
        .ignore_status()
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Captured output from a command with decoded stdout/stderr strings
pub(crate) struct CapturedOutput {
    pub output: Output,
    pub stdout: String,
    pub stderr: String,
}

impl CapturedOutput {
    /// Create from a raw Output
    pub fn new(output: Output) -> Self {
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        Self {
            output,
            stdout,
            stderr,
        }
    }

    /// Combined stdout and stderr
    pub fn combined(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }

    /// Assert that the command succeeded, printing debug info on failure
    pub fn assert_success(&self, context: &str) {
        assert!(
            self.output.status.success(),
            "{} failed:\nstdout: {}\nstderr: {}",
            context,
            self.stdout,
            self.stderr
        );
    }

    /// Check if the command succeeded
    pub fn success(&self) -> bool {
        self.output.status.success()
    }

    /// Extract pod name from "Pod 'name' ready" message in output
    ///
    /// Returns None if the pattern is not found.
    pub fn extract_pod_name(&self) -> Option<String> {
        // Look in both stdout and stderr (tracing goes to stderr)
        for line in self.combined().lines() {
            // Match pattern like: INFO Pod 'devaipod-test-repo-abc123' ready
            if line.contains("ready") {
                if let Some(rest) = line.split("Pod '").nth(1) {
                    if let Some(name) = rest.split('\'').next() {
                        return Some(name.to_string());
                    }
                }
            }
        }
        None
    }
}

/// Run the devaipod command, capturing output
///
/// This uses std::process::Command for consistent CapturedOutput handling.
/// If the devaipod container is running, uses `podman exec devaipod devaipod <args>`.
/// Otherwise runs the binary directly with DEVAIPOD_HOST_MODE=1.
pub(crate) fn run_devaipod(args: &[&str]) -> Result<CapturedOutput> {
    let cmd = get_devaipod_command()?;
    let output = match cmd {
        DevaipodCommand::Container => Command::new("podman")
            .arg("exec")
            .arg("devaipod")
            .arg("devaipod")
            .args(args)
            .output()
            .with_context(|| format!("Failed to run podman exec devaipod devaipod {:?}", args))?,
        DevaipodCommand::Host(binary) => Command::new(&binary)
            .args(args)
            .env("DEVAIPOD_HOST_MODE", "1")
            .output()
            .with_context(|| format!("Failed to run devaipod {:?}", args))?,
    };
    Ok(CapturedOutput::new(output))
}

/// Run the devaipod command in a specific directory.
///
/// When the devaipod container is running, uses `podman exec -w <dir> devaipod devaipod <args>`
/// (the directory must be at the same path inside the container, e.g. via shared volume).
/// Otherwise uses the host binary with DEVAIPOD_HOST_MODE=1.
pub(crate) fn run_devaipod_in(dir: &std::path::Path, args: &[&str]) -> Result<CapturedOutput> {
    let cmd = get_devaipod_command()?;
    let output = match cmd {
        DevaipodCommand::Container => Command::new("podman")
            .arg("exec")
            .arg("-w")
            .arg(dir)
            .arg("devaipod")
            .arg("devaipod")
            .args(args)
            .output()
            .with_context(|| format!("Failed to run podman exec devaipod devaipod {:?} in {:?}", args, dir))?,
        DevaipodCommand::Host(binary) => Command::new(&binary)
            .current_dir(dir)
            .args(args)
            .env("DEVAIPOD_HOST_MODE", "1")
            .output()
            .with_context(|| format!("Failed to run devaipod {:?} in {:?}", args, dir))?,
    };
    Ok(CapturedOutput::new(output))
}

/// Run the devaipod command in a specific directory with extra environment variables.
///
/// When the devaipod container is running, uses podman exec with -w and -e.
/// Otherwise uses the host binary with DEVAIPOD_HOST_MODE=1.
pub(crate) fn run_devaipod_in_with_env(
    dir: &std::path::Path,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<CapturedOutput> {
    let devaipod_cmd = get_devaipod_command()?;
    let output = match devaipod_cmd {
        DevaipodCommand::Container => {
            let mut cmd = Command::new("podman");
            cmd.arg("exec").arg("-w").arg(dir);
            for (k, v) in envs {
                cmd.arg("-e").arg(format!("{}={}", k, v));
            }
            cmd.arg("devaipod").arg("devaipod").args(args);
            cmd.output()
                .with_context(|| format!("Failed to run podman exec devaipod devaipod {:?} in {:?}", args, dir))?
        }
        DevaipodCommand::Host(binary) => {
            let mut cmd = Command::new(&binary);
            cmd.current_dir(dir)
                .args(args)
                .env("DEVAIPOD_HOST_MODE", "1");
            for (key, value) in envs {
                cmd.env(key, value);
            }
            cmd.output()
                .with_context(|| format!("Failed to run devaipod {:?} in {:?}", args, dir))?
        }
    };
    Ok(CapturedOutput::new(output))
}

/// Run the devaipod command with extra environment variables.
///
/// When the devaipod container is running, uses podman exec with -e for env vars.
pub(crate) fn run_devaipod_with_env(
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<CapturedOutput> {
    let devaipod_cmd = get_devaipod_command()?;
    let output = match devaipod_cmd {
        DevaipodCommand::Container => {
            let mut cmd = Command::new("podman");
            cmd.arg("exec");
            for (k, v) in envs {
                cmd.arg("-e").arg(format!("{}={}", k, v));
            }
            cmd.arg("devaipod").arg("devaipod").args(args);
            cmd.output()
                .with_context(|| format!("Failed to run podman exec devaipod devaipod {:?}", args))?
        }
        DevaipodCommand::Host(binary) => {
            let mut cmd = Command::new(&binary);
            cmd.args(args).env("DEVAIPOD_HOST_MODE", "1");
            for (key, value) in envs {
                cmd.env(key, value);
            }
            cmd.output()
                .with_context(|| format!("Failed to run devaipod {:?}", args))?
        }
    };
    Ok(CapturedOutput::new(output))
}

/// Get the path to the devaipod binary (for host mode only)
///
/// This is used when we need to run devaipod directly on the host,
/// such as when the command needs access to host filesystem paths,
/// or when spawning child processes that need to invoke devaipod.
pub(crate) fn get_devaipod_binary_path() -> Result<String> {
    if let Ok(path) = std::env::var("DEVAIPOD_PATH") {
        let path = std::path::PathBuf::from(&path);
        if path.is_relative() {
            if let Some(workspace_root) = find_workspace_root() {
                let abs_path = workspace_root.join(&path);
                if abs_path.exists() {
                    return Ok(abs_path.canonicalize()?.to_string_lossy().to_string());
                }
            }
            let cwd = std::env::current_dir()?;
            let abs_path = cwd.join(&path);
            if abs_path.exists() {
                return Ok(abs_path.canonicalize()?.to_string_lossy().to_string());
            }
            return Err(eyre!("Cannot find devaipod binary at {}", path.display()));
        }
        return Ok(path.to_string_lossy().to_string());
    }

    // Look for binary in target directories
    let workspace_root = find_workspace_root();
    let candidates = ["target/debug/devaipod", "target/release/devaipod"];
    for candidate in candidates {
        let path = if let Some(ref root) = workspace_root {
            root.join(candidate)
        } else {
            std::path::PathBuf::from(candidate)
        };
        if path.exists() {
            return Ok(path.canonicalize()?.to_string_lossy().to_string());
        }
    }

    Ok("devaipod".to_string())
}

/// Create a temporary git repository for testing
pub(crate) struct TestRepo {
    /// Keep the temp dir alive for the lifetime of the test
    #[allow(dead_code)]
    pub temp_dir: tempfile::TempDir,
    pub repo_path: PathBuf,
}

impl TestRepo {
    /// Create a new test repository with a devcontainer.json
    pub fn new() -> Result<Self> {
        let temp_dir = tempfile::TempDir::new()?;
        let repo_path = temp_dir.path().join("test-repo");
        std::fs::create_dir_all(&repo_path)?;

        let sh = shell()?;
        let repo = repo_path.to_str().unwrap();

        // Initialize git repo
        cmd!(sh, "git -C {repo} init").run()?;
        cmd!(sh, "git -C {repo} config user.email test@example.com").run()?;
        cmd!(sh, "git -C {repo} config user.name 'Test User'").run()?;

        // Create devcontainer.json - use test image from env (must have git)
        let devcontainer_dir = repo_path.join(".devcontainer");
        std::fs::create_dir_all(&devcontainer_dir)?;
        let test_image = std::env::var("DEVAIPOD_TEST_IMAGE")
            .unwrap_or_else(|_| "ghcr.io/bootc-dev/devenv-debian:latest".to_string());
        let devcontainer_json = format!(
            r#"{{
    "name": "integration-test",
    "image": "{}"
}}"#,
            test_image
        );
        std::fs::write(
            devcontainer_dir.join("devcontainer.json"),
            devcontainer_json,
        )?;
        std::fs::write(repo_path.join("README.md"), "# Test Repo\n")?;

        // Add remote (required by devaipod)
        cmd!(
            sh,
            "git -C {repo} remote add origin https://github.com/test/test-repo.git"
        )
        .run()?;

        // Commit
        cmd!(sh, "git -C {repo} add .").run()?;
        cmd!(sh, "git -C {repo} commit -m 'Initial commit'").run()?;

        Ok(TestRepo {
            temp_dir,
            repo_path,
        })
    }

    /// Create a test repository with custom devcontainer.json content
    pub fn new_with_devcontainer(devcontainer_json: &str) -> Result<Self> {
        let temp_dir = tempfile::TempDir::new()?;
        let repo_path = temp_dir.path().join("test-repo");
        std::fs::create_dir_all(&repo_path)?;

        let sh = shell()?;
        let repo = repo_path.to_str().unwrap();

        // Initialize git repo
        cmd!(sh, "git -C {repo} init").run()?;
        cmd!(sh, "git -C {repo} config user.email test@example.com").run()?;
        cmd!(sh, "git -C {repo} config user.name 'Test User'").run()?;

        // Create devcontainer.json with provided content
        let devcontainer_dir = repo_path.join(".devcontainer");
        std::fs::create_dir_all(&devcontainer_dir)?;
        std::fs::write(
            devcontainer_dir.join("devcontainer.json"),
            devcontainer_json,
        )?;
        std::fs::write(repo_path.join("README.md"), "# Test Repo\n")?;

        // Add remote (required by devaipod)
        cmd!(
            sh,
            "git -C {repo} remote add origin https://github.com/test/test-repo.git"
        )
        .run()?;

        // Commit
        cmd!(sh, "git -C {repo} add .").run()?;
        cmd!(sh, "git -C {repo} commit -m 'Initial commit'").run()?;

        Ok(TestRepo {
            temp_dir,
            repo_path,
        })
    }

    /// Create a minimal test repo (just git init, no devcontainer)
    pub fn new_minimal() -> Result<Self> {
        let temp_dir = tempfile::TempDir::new()?;
        let repo_path = temp_dir.path().join("minimal-repo");
        std::fs::create_dir_all(&repo_path)?;

        let sh = shell()?;
        let repo = repo_path.to_str().unwrap();

        // Initialize git repo
        cmd!(sh, "git -C {repo} init").run()?;
        cmd!(sh, "git -C {repo} config user.email test@example.com").run()?;
        cmd!(sh, "git -C {repo} config user.name 'Test User'").run()?;

        std::fs::write(repo_path.join("README.md"), "# Minimal Repo\n")?;

        // Add remote
        cmd!(
            sh,
            "git -C {repo} remote add origin https://github.com/test/minimal-repo.git"
        )
        .run()?;

        // Commit
        cmd!(sh, "git -C {repo} add .").run()?;
        cmd!(sh, "git -C {repo} commit -m 'Initial commit'").run()?;

        Ok(TestRepo {
            temp_dir,
            repo_path,
        })
    }
}

/// The prefix devaipod adds to all pod names
const POD_NAME_PREFIX: &str = "devaipod-";

/// Generate a unique test pod name with the devaipod prefix
///
/// Uses timestamp + random bits to ensure uniqueness across parallel test runs.
/// Returns the full pod name as it will be created by devaipod (with prefix).
pub(crate) fn unique_test_name(prefix: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    // Use lower bits of timestamp + nanos for uniqueness
    let val = (now.as_secs() & 0xFFFF) ^ ((now.subsec_nanos() as u64) & 0xFFFF);
    format!("{}{}-{:x}", POD_NAME_PREFIX, prefix, val)
}

/// Get the short name (without prefix) for passing to --name
///
/// devaipod's --name flag will add the prefix automatically
pub(crate) fn short_name(full_name: &str) -> &str {
    full_name.strip_prefix(POD_NAME_PREFIX).unwrap_or(full_name)
}

/// Pod cleanup helper - removes pods on drop
///
/// Uses std::process::Command because Shell::new() is fallible in Drop contexts.
pub(crate) struct PodGuard {
    names: Vec<String>,
}

impl PodGuard {
    pub fn new() -> Self {
        PodGuard { names: Vec::new() }
    }

    pub fn add(&mut self, name: &str) {
        self.names.push(name.to_string());
    }
}

impl Drop for PodGuard {
    fn drop(&mut self) {
        for name in &self.names {
            // Best effort cleanup - remove pod which removes all containers in it
            let _ = Command::new("podman")
                .args(["pod", "rm", "-f", name])
                .output();
            // Also try to remove associated volumes
            for suffix in integration_tests::POD_VOLUME_SUFFIXES {
                let volume_name = format!("{name}{suffix}");
                let _ = Command::new("podman")
                    .args(["volume", "rm", "-f", &volume_name])
                    .output();
            }
        }
    }
}

/// Env var set by `just test-integration-container`; if unset, we require it (fail fast unless overridden).
const DEVAIPOD_CONTAINER_IMAGE_ENV: &str = "DEVAIPOD_CONTAINER_IMAGE";
/// Set to allow running integration tests without the container (e.g. for debugging).
const DEVAIPOD_ALLOW_DIRECT_TEST_ENV: &str = "DEVAIPOD_INTEGRATION_ALLOW_DIRECT";

fn main() {
    // Require running against the built container by default (just test-integration-container sets DEVAIPOD_CONTAINER_IMAGE)
    if std::env::var(DEVAIPOD_CONTAINER_IMAGE_ENV).is_err()
        && std::env::var(DEVAIPOD_ALLOW_DIRECT_TEST_ENV).is_err()
    {
        eprintln!("error: integration tests must be run against the built container.");
        eprintln!("  Run: just test-integration-container");
        eprintln!("  To allow direct runs (e.g. for debugging), set: {}=1", DEVAIPOD_ALLOW_DIRECT_TEST_ENV);
        std::process::exit(1);
    }

    // Initialize tracing for better debug output
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let args = Arguments::from_args();

    // Check if podman is available for filtering tests
    let has_podman = podman_available();
    if !has_podman {
        eprintln!("Note: podman not available, skipping podman-dependent tests");
    }

    // Collect readonly tests - these use the shared fixture
    let readonly_tests: Vec<Trial> = if has_podman && !READONLY_INTEGRATION_TESTS.is_empty() {
        // Initialize the shared fixture before creating readonly test trials
        // We do this eagerly so any initialization errors are reported upfront
        let fixture_result = SharedFixture::get();

        if let Err(ref e) = fixture_result {
            eprintln!("Failed to create shared fixture: {:?}", e);
            eprintln!("Readonly tests will be skipped");
        }

        READONLY_INTEGRATION_TESTS
            .iter()
            .map(|test| {
                let name = test.name;
                let f = test.f;
                let fixture_ok = fixture_result.is_ok();

                let trial = Trial::test(name, move || {
                    if !fixture_ok {
                        return Err("Shared fixture initialization failed".into());
                    }
                    // Safe to unwrap since we checked fixture_ok
                    let fixture = SharedFixture::get().map_err(|e| format!("{:?}", e))?;
                    f(fixture).map_err(|e| format!("{:?}", e).into())
                });

                // Mark as ignored if fixture failed
                if !fixture_ok {
                    trial.with_ignored_flag(true)
                } else {
                    trial
                }
            })
            .collect()
    } else {
        // Skip readonly tests if no podman or no tests registered
        READONLY_INTEGRATION_TESTS
            .iter()
            .map(|test| Trial::test(test.name, || Ok(())).with_ignored_flag(true))
            .collect()
    };

    // Collect mutating tests from the distributed slice
    let mutating_tests: Vec<Trial> = INTEGRATION_TESTS
        .iter()
        .map(|test| {
            let name = test.name;
            let f = test.f;
            let requires_podman = test.requires_podman;

            let mut trial = Trial::test(name, move || f().map_err(|e| format!("{:?}", e).into()));

            // Mark podman tests as ignored if podman is not available
            if requires_podman && !has_podman {
                trial = trial.with_ignored_flag(true);
            }

            trial
        })
        .collect();

    // Combine all tests
    let all_tests: Vec<Trial> = readonly_tests.into_iter().chain(mutating_tests).collect();

    // Run the tests
    let conclusion = libtest_mimic::run(&args, all_tests);

    // Clean up the shared fixtures after all tests complete
    if has_podman {
        if !READONLY_INTEGRATION_TESTS.is_empty() {
            SharedFixture::cleanup();
        }
        // Clean up web fixture (used by webui tests)
        WebFixture::cleanup();
    }

    // Exit with the result
    conclusion.exit();
}
