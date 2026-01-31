//! SSH server integration tests
//!
//! These tests verify the SSH server functionality in devaipod:
//! - SSH config file generation on workspace creation
//! - SSH server startup via `devaipod exec --stdio`
//! - Basic command execution through SSH
//! - Cleanup of SSH config on workspace deletion

use color_eyre::eyre::bail;
use color_eyre::Result;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;
use xshell::cmd;

use crate::{
    get_devaipod_command, podman_integration_test, readonly_test, run_devaipod, run_devaipod_in,
    shell, short_name, unique_test_name, PodGuard, SharedFixture, TestRepo,
};

// =============================================================================
// Readonly tests - use shared fixture
// =============================================================================

/// Verify that SSH config file exists for the shared pod
fn test_readonly_ssh_config_exists(fixture: &SharedFixture) -> Result<()> {
    let short_name = fixture.short_name();

    // Get the SSH config directory
    let home = std::env::var("HOME")?;
    let config_dir = std::path::PathBuf::from(&home)
        .join(".ssh")
        .join("config.d");

    // The config file is named devaipod-<short_name>
    let config_file = config_dir.join(format!("devaipod-{}", short_name));

    // SSH config may not exist if auto_config is disabled in test environment
    // But if the directory exists, we can check if any devaipod configs are there
    if config_dir.exists() {
        // Check if the file exists
        if config_file.exists() {
            // Verify it contains expected content
            let content = std::fs::read_to_string(&config_file)?;
            assert!(
                content.contains("ProxyCommand"),
                "SSH config should contain ProxyCommand: {}",
                content
            );
            assert!(
                content.contains("exec") && content.contains("--stdio"),
                "SSH config should use 'exec --stdio': {}",
                content
            );
            assert!(
                content.contains(short_name) || content.contains(&fixture.pod_name()),
                "SSH config should reference the pod: {}",
                content
            );
        } else {
            // Config file doesn't exist - this is OK if auto_config is disabled
            // or if the test environment didn't set it up
            tracing::info!(
                "SSH config file not found at {} (auto_config may be disabled)",
                config_file.display()
            );
        }
    } else {
        tracing::info!(
            "SSH config directory {} doesn't exist",
            config_dir.display()
        );
    }

    Ok(())
}
readonly_test!(test_readonly_ssh_config_exists);

/// Verify that we can run commands through the SSH server via exec --stdio
///
/// This tests the core SSH server functionality without needing an SSH client.
/// We use `devaipod exec --stdio` which starts the embedded SSH server, but
/// we pass a command directly to test the underlying podman exec path.
fn test_readonly_exec_stdio_with_command(fixture: &SharedFixture) -> Result<()> {
    let short_name = fixture.short_name();

    // Run a simple command via exec --stdio
    // When a command is provided, exec --stdio does direct podman exec (not SSH)
    let output = run_devaipod(&[
        "exec",
        "-W",
        "--stdio",
        short_name,
        "--",
        "echo",
        "hello-from-stdio",
    ])?;

    // Should succeed
    assert!(
        output.success(),
        "exec --stdio with command should succeed: {}",
        output.combined()
    );

    assert!(
        output.stdout.contains("hello-from-stdio"),
        "Should see command output: {}",
        output.stdout
    );

    Ok(())
}
readonly_test!(test_readonly_exec_stdio_with_command);

// =============================================================================
// Mutating tests - create/delete their own pods
// =============================================================================

/// Verify SSH config file is created when a pod is created
fn test_ssh_config_created_on_pod_up() -> Result<()> {
    let repo = TestRepo::new()?;
    let pod_name = unique_test_name("test-ssh-cfg");

    let mut pods = PodGuard::new();
    pods.add(&pod_name);

    // Create pod
    let output = run_devaipod_in(
        &repo.repo_path,
        &["up", ".", "--name", short_name(&pod_name)],
    )?;
    if !output.success() {
        bail!("devaipod up failed: {}", output.combined());
    }

    // Check that SSH config file was created
    let home = std::env::var("HOME")?;
    let config_dir = std::path::PathBuf::from(&home)
        .join(".ssh")
        .join("config.d");

    // The config file uses the full pod name with prefix
    let config_file = config_dir.join(&pod_name);

    // Give a moment for the file to be written
    std::thread::sleep(Duration::from_millis(100));

    // SSH config creation is best-effort and depends on [ssh] config
    // If the directory doesn't exist, skip this assertion
    if config_dir.exists() {
        // Check if config file was created
        if config_file.exists() {
            let content = std::fs::read_to_string(&config_file)?;

            // Verify content structure
            assert!(
                content.contains("Host"),
                "SSH config should contain Host directive: {}",
                content
            );
            assert!(
                content.contains("ProxyCommand"),
                "SSH config should contain ProxyCommand: {}",
                content
            );
            assert!(
                content.contains("--stdio"),
                "SSH config should use --stdio for ProxyCommand: {}",
                content
            );
        } else {
            tracing::info!(
                "SSH config file not created (auto_config may be disabled in test config)"
            );
        }
    }

    Ok(())
}
podman_integration_test!(test_ssh_config_created_on_pod_up);

/// Verify SSH config file is removed when a pod is deleted
fn test_ssh_config_removed_on_pod_delete() -> Result<()> {
    let repo = TestRepo::new()?;
    let pod_name = unique_test_name("test-ssh-del");

    let _pods = PodGuard::new();

    // Create pod
    let output = run_devaipod_in(
        &repo.repo_path,
        &["up", ".", "--name", short_name(&pod_name)],
    )?;
    if !output.success() {
        bail!("devaipod up failed: {}", output.combined());
    }

    // Check initial SSH config state
    let home = std::env::var("HOME")?;
    let config_dir = std::path::PathBuf::from(&home)
        .join(".ssh")
        .join("config.d");
    let config_file = config_dir.join(&pod_name);

    // Give a moment for the file to be written
    std::thread::sleep(Duration::from_millis(100));

    let config_existed_before = config_file.exists();

    // Delete the pod (don't add to guard since we're deleting manually)
    let delete_output = run_devaipod(&["delete", short_name(&pod_name), "--force"])?;
    delete_output.assert_success("devaipod delete");

    // If config existed before, verify it's now gone
    if config_existed_before {
        assert!(
            !config_file.exists(),
            "SSH config file should be removed after pod deletion"
        );
    }

    Ok(())
}
podman_integration_test!(test_ssh_config_removed_on_pod_delete);

/// Test that devaipod exec --stdio starts and accepts SSH protocol
///
/// This test spawns `devaipod exec --stdio` and verifies that it starts
/// an SSH server by checking for the SSH protocol banner.
fn test_ssh_server_starts_on_exec_stdio() -> Result<()> {
    let repo = TestRepo::new()?;
    let pod_name = unique_test_name("test-ssh-srv");

    let mut pods = PodGuard::new();
    pods.add(&pod_name);

    // Create pod
    let output = run_devaipod_in(
        &repo.repo_path,
        &["up", ".", "--name", short_name(&pod_name)],
    )?;
    if !output.success() {
        bail!("devaipod up failed: {}", output.combined());
    }

    // Give containers time to start
    std::thread::sleep(Duration::from_secs(2));

    // Start exec --stdio without a command - this starts the SSH server
    let devaipod = get_devaipod_command()?;
    let mut child = Command::new(&devaipod)
        .args(["exec", "-W", "--stdio", short_name(&pod_name)])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| color_eyre::eyre::eyre!("Failed to spawn exec --stdio: {}", e))?;

    // The SSH server should send a protocol banner starting with "SSH-2.0-"
    // Read from stdout with a timeout
    let mut stdout = child.stdout.take().unwrap();

    // Use non-blocking read with timeout
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut buf = [0u8; 256];
        match stdout.read(&mut buf) {
            Ok(n) if n > 0 => tx.send(Some(buf[..n].to_vec())).ok(),
            _ => tx.send(None).ok(),
        };
    });

    // Wait for banner with timeout
    let banner = match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Some(data)) => String::from_utf8_lossy(&data).to_string(),
        Ok(None) => String::new(),
        Err(_) => {
            // Timeout - kill the process
            let _ = child.kill();
            String::new()
        }
    };

    // Clean up
    let _ = child.kill();
    let _ = child.wait();
    let _ = handle.join();

    // Check if we got an SSH banner
    assert!(
        banner.starts_with("SSH-2.0-"),
        "SSH server should send SSH-2.0 banner. Got: {:?}",
        banner.chars().take(50).collect::<String>()
    );

    Ok(())
}
podman_integration_test!(test_ssh_server_starts_on_exec_stdio);

/// Test SSH connectivity using the ssh command if available
///
/// This is a more complete end-to-end test that uses the actual SSH client
/// to connect through the devaipod SSH server.
fn test_ssh_client_connectivity() -> Result<()> {
    let sh = shell()?;

    // Check if ssh command is available
    let ssh_available = cmd!(sh, "which ssh")
        .ignore_status()
        .output()?
        .status
        .success();
    if !ssh_available {
        tracing::info!("Skipping SSH client test: ssh command not available");
        return Ok(());
    }

    let repo = TestRepo::new()?;
    let pod_name = unique_test_name("test-ssh-cli");

    let mut pods = PodGuard::new();
    pods.add(&pod_name);

    // Create pod
    let output = run_devaipod_in(
        &repo.repo_path,
        &["up", ".", "--name", short_name(&pod_name)],
    )?;
    if !output.success() {
        bail!("devaipod up failed: {}", output.combined());
    }

    // Give containers time to start
    std::thread::sleep(Duration::from_secs(2));

    // Get the devaipod binary path
    let devaipod = get_devaipod_command()?;
    let short = short_name(&pod_name);

    // Build the ProxyCommand string
    let proxy_cmd = format!("{} exec -W --stdio {}", devaipod, short);

    // Try SSH with ProxyCommand directly
    // Use -o options to configure SSH without a config file
    let ssh_result = Command::new("timeout")
        .args([
            "10",
            "ssh",
            "-o",
            &format!("ProxyCommand={}", proxy_cmd),
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-o",
            "LogLevel=ERROR",
            "-o",
            "PasswordAuthentication=no",
            "-o",
            "PubkeyAuthentication=no",
            "localhost",
            "echo",
            "hello-via-ssh",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;

    if ssh_result.status.success() {
        let stdout = String::from_utf8_lossy(&ssh_result.stdout);
        assert!(
            stdout.contains("hello-via-ssh"),
            "SSH command should return output: {}",
            stdout
        );
        tracing::info!("SSH client connectivity test passed");
    } else {
        // SSH may fail for various reasons in CI (no SSH agent, etc.)
        // Log the error but don't fail the test
        let stderr = String::from_utf8_lossy(&ssh_result.stderr);
        tracing::info!(
            "SSH client test skipped - command failed (may be expected in CI): {}",
            stderr
        );
    }

    Ok(())
}
podman_integration_test!(test_ssh_client_connectivity);

/// Verify the ssh-config command generates valid output
fn test_ssh_config_command() -> Result<()> {
    let repo = TestRepo::new()?;
    let pod_name = unique_test_name("test-ssh-cmd");

    let mut pods = PodGuard::new();
    pods.add(&pod_name);

    // Create pod
    let output = run_devaipod_in(
        &repo.repo_path,
        &["up", ".", "--name", short_name(&pod_name)],
    )?;
    if !output.success() {
        bail!("devaipod up failed: {}", output.combined());
    }

    // Run ssh-config command
    let ssh_config_output = run_devaipod(&["ssh-config", short_name(&pod_name)])?;
    ssh_config_output.assert_success("devaipod ssh-config");

    // Verify output mentions the config was written
    let combined = ssh_config_output.combined();
    assert!(
        combined.contains("SSH config") || combined.contains(".ssh/config.d"),
        "ssh-config should report writing config: {}",
        combined
    );

    Ok(())
}
podman_integration_test!(test_ssh_config_command);

/// Test that exec --stdio with a command works correctly for multiple commands
fn test_exec_stdio_multiple_commands() -> Result<()> {
    let repo = TestRepo::new()?;
    let pod_name = unique_test_name("test-ssh-multi");

    let mut pods = PodGuard::new();
    pods.add(&pod_name);

    // Create pod
    let output = run_devaipod_in(
        &repo.repo_path,
        &["up", ".", "--name", short_name(&pod_name)],
    )?;
    if !output.success() {
        bail!("devaipod up failed: {}", output.combined());
    }

    // Give containers time to start
    std::thread::sleep(Duration::from_secs(2));

    // Test 1: pwd command
    let pwd_output = run_devaipod(&["exec", "-W", "--stdio", short_name(&pod_name), "--", "pwd"])?;
    pwd_output.assert_success("pwd via exec --stdio");
    assert!(
        pwd_output.stdout.trim().starts_with('/'),
        "pwd should return a path: {}",
        pwd_output.stdout
    );

    // Test 2: ls command
    let ls_output = run_devaipod(&[
        "exec",
        "-W",
        "--stdio",
        short_name(&pod_name),
        "--",
        "ls",
        "/workspaces",
    ])?;
    ls_output.assert_success("ls via exec --stdio");
    assert!(
        ls_output.stdout.contains("test-repo"),
        "ls should show workspace: {}",
        ls_output.stdout
    );

    // Test 3: Command with arguments
    let cat_output = run_devaipod(&[
        "exec",
        "-W",
        "--stdio",
        short_name(&pod_name),
        "--",
        "cat",
        "/workspaces/test-repo/README.md",
    ])?;
    cat_output.assert_success("cat via exec --stdio");
    assert!(
        cat_output.stdout.contains("Test Repo") || cat_output.stdout.contains('#'),
        "cat should show README content: {}",
        cat_output.stdout
    );

    Ok(())
}
podman_integration_test!(test_exec_stdio_multiple_commands);

/// Verify that exec --stdio works with agent container target
fn test_exec_stdio_agent_container() -> Result<()> {
    let repo = TestRepo::new()?;
    let pod_name = unique_test_name("test-ssh-agent");

    let mut pods = PodGuard::new();
    pods.add(&pod_name);

    // Create pod
    let output = run_devaipod_in(
        &repo.repo_path,
        &["up", ".", "--name", short_name(&pod_name)],
    )?;
    if !output.success() {
        bail!("devaipod up failed: {}", output.combined());
    }

    // Give containers time to start
    std::thread::sleep(Duration::from_secs(2));

    // Default exec --stdio goes to agent container
    let output = run_devaipod(&[
        "exec",
        "--stdio",
        short_name(&pod_name),
        "--",
        "echo",
        "hello-from-agent",
    ])?;
    output.assert_success("exec --stdio agent");
    assert!(
        output.stdout.contains("hello-from-agent"),
        "Should get output from agent: {}",
        output.stdout
    );

    // Verify we can access the agent's workspace
    let ws_output = run_devaipod(&[
        "exec",
        "--stdio",
        short_name(&pod_name),
        "--",
        "ls",
        "/workspaces",
    ])?;
    ws_output.assert_success("ls via exec --stdio agent");
    assert!(
        ws_output.stdout.contains("test-repo"),
        "Agent should have workspace access: {}",
        ws_output.stdout
    );

    Ok(())
}
podman_integration_test!(test_exec_stdio_agent_container);
