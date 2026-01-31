//! Container/pod tests that require podman
//!
//! These tests verify that devaipod correctly creates and manages pods.
//!
//! Tests are organized into two categories:
//! - Readonly tests: Use the shared fixture, only query pod state
//! - Mutating tests: Create/delete their own pods

use color_eyre::eyre::bail;
use color_eyre::Result;
use std::time::{Duration, Instant};
use xshell::{cmd, Shell};

use crate::{
    podman_integration_test, readonly_test, run_devaipod, run_devaipod_in, shell, short_name,
    unique_test_name, PodGuard, SharedFixture, TestRepo,
};

/// Poll a condition until it succeeds or times out.
/// Returns Ok(output) on success, Err on timeout.
fn poll_until<F>(timeout: Duration, interval: Duration, mut check: F) -> Result<String>
where
    F: FnMut() -> Result<Option<String>>,
{
    let start = Instant::now();
    loop {
        match check() {
            Ok(Some(output)) => return Ok(output),
            Ok(None) => {}
            Err(e) => {
                if start.elapsed() >= timeout {
                    return Err(e);
                }
            }
        }
        if start.elapsed() >= timeout {
            bail!("Timed out after {:?}", timeout);
        }
        std::thread::sleep(interval);
    }
}

/// Wait for a file to exist in a container with expected content
fn wait_for_file_content(
    sh: &Shell,
    container: &str,
    path: &str,
    expected: &str,
    timeout: Duration,
) -> Result<String> {
    let container = container.to_string();
    let path = path.to_string();
    let expected = expected.to_string();

    poll_until(timeout, Duration::from_millis(500), || {
        let output = cmd!(sh, "podman exec {container} cat {path}")
            .ignore_status()
            .read()?;
        if output.contains(&expected) {
            Ok(Some(output))
        } else {
            Ok(None)
        }
    })
}

// =============================================================================
// Readonly tests - use shared fixture
// =============================================================================

/// Verify the shared instance exists and is running
fn test_readonly_pod_exists(fixture: &SharedFixture) -> Result<()> {
    let sh = shell()?;
    let pod_name = fixture.pod_name();

    let exists = cmd!(sh, "podman pod exists {pod_name}")
        .ignore_status()
        .output()?;
    assert!(
        exists.status.success(),
        "Shared instance {} should exist",
        pod_name
    );

    // Verify instance is running (podman uses .State, not .Status)
    let format_state = "{{.State}}";
    let state = cmd!(sh, "podman pod inspect {pod_name} --format {format_state}").read()?;
    assert!(
        state.contains("Running"),
        "Shared instance should be running, got: {}",
        state
    );

    Ok(())
}
readonly_test!(test_readonly_pod_exists);

/// Verify we can SSH into the shared pod and run commands
fn test_readonly_can_ssh(fixture: &SharedFixture) -> Result<()> {
    // Use short_name() for devaipod CLI commands
    let short_name = fixture.short_name();

    // Run a simple command via ssh
    let output = run_devaipod(&["ssh", short_name, "--", "echo", "hello-from-shared"])?;
    output.assert_success("devaipod ssh echo");
    assert!(
        output.stdout.contains("hello-from-shared"),
        "SSH should return command output: {}",
        output.combined()
    );

    // Verify we can see the workspace
    let ls_output = run_devaipod(&["ssh", short_name, "--", "ls", "/workspaces"])?;
    ls_output.assert_success("devaipod ssh ls");
    assert!(
        ls_output.stdout.contains("shared-test-repo"),
        "Should see shared workspace directory: {}",
        ls_output.stdout
    );

    Ok(())
}
readonly_test!(test_readonly_can_ssh);

/// Verify the agent API endpoint responds to authenticated requests
fn test_readonly_api_responds(fixture: &SharedFixture) -> Result<()> {
    let sh = shell()?;
    let pod_name = fixture.pod_name();

    // Get API credentials from pod labels
    let format_label = "{{index .Labels \"io.devaipod.api-password\"}}";
    let password = cmd!(sh, "podman pod inspect {pod_name} --format {format_label}").read()?;
    let password = password.trim();
    assert!(!password.is_empty(), "Pod should have API password label");

    // Get the published port (4097 is the auth proxy port)
    let agent_container = fixture.agent_container();
    let port_output = cmd!(sh, "podman port {agent_container} 4097")
        .ignore_status()
        .read()?;
    assert!(
        port_output.contains("127.0.0.1:"),
        "Port 4097 (auth proxy) should be published: {}",
        port_output
    );

    let port: u16 = port_output
        .trim()
        .split(':')
        .last()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    assert!(port > 0, "Should have a valid port number");

    // Test authenticated request to /session endpoint
    let url = format!("http://127.0.0.1:{}/session", port);
    let response = cmd!(sh, "curl -sf -u opencode:{password} {url}")
        .ignore_status()
        .output()?;
    assert!(
        response.status.success(),
        "Authenticated API request should succeed"
    );

    Ok(())
}
readonly_test!(test_readonly_api_responds);

/// Verify the shared pod has the expected containers
fn test_readonly_containers_exist(fixture: &SharedFixture) -> Result<()> {
    let sh = shell()?;
    let pod_name = fixture.pod_name();

    let format_names = "{{.Names}}";
    let ps_output = cmd!(
        sh,
        "podman ps --filter pod={pod_name} --format {format_names}"
    )
    .read()?;

    assert!(
        ps_output.contains("workspace"),
        "Pod should have workspace container: {}",
        ps_output
    );
    assert!(
        ps_output.contains("agent"),
        "Pod should have agent container: {}",
        ps_output
    );

    Ok(())
}
readonly_test!(test_readonly_containers_exist);

/// Verify we can query pod status via devaipod
fn test_readonly_status_command(fixture: &SharedFixture) -> Result<()> {
    // Use short_name() for devaipod CLI commands
    let short_name = fixture.short_name();

    let status_output = run_devaipod(&["status", short_name])?;
    status_output.assert_success("devaipod status");

    Ok(())
}
readonly_test!(test_readonly_status_command);

/// Verify the pod appears in devaipod list
fn test_readonly_list_shows_pod(fixture: &SharedFixture) -> Result<()> {
    // devaipod list shows the short name (without prefix)
    let short_name = fixture.short_name();

    let list_output = run_devaipod(&["list"])?;
    list_output.assert_success("devaipod list");
    assert!(
        list_output.stdout.contains(short_name) || list_output.stderr.contains(short_name),
        "List should show the shared pod {}: {}",
        short_name,
        list_output.combined()
    );

    Ok(())
}
readonly_test!(test_readonly_list_shows_pod);

// =============================================================================
// Mutating tests - create/delete their own pods
// =============================================================================

fn test_pod_creation_and_deletion() -> Result<()> {
    let repo = TestRepo::new()?;
    let pod_name = unique_test_name("test-create");

    let mut pods = PodGuard::new();
    pods.add(&pod_name);

    // Create pod with explicit name (pass short name, devaipod adds prefix)
    let output = run_devaipod_in(
        &repo.repo_path,
        &["up", ".", "--name", short_name(&pod_name)],
    )?;
    if !output.success() {
        bail!("devaipod up failed: {}", output.combined());
    }

    let sh = shell()?;

    // Verify pod was created
    let pod_exists = cmd!(sh, "podman pod exists {pod_name}")
        .ignore_status()
        .output()?;
    assert!(
        pod_exists.status.success(),
        "Pod {} should exist after 'devaipod up'",
        pod_name
    );

    // Verify containers are running
    let format_names = "{{.Names}}";
    let ps_output = cmd!(
        sh,
        "podman ps --filter pod={pod_name} --format {format_names}"
    )
    .read()?;
    assert!(
        ps_output.contains("workspace"),
        "Pod should have workspace container: {}",
        ps_output
    );
    assert!(
        ps_output.contains("agent"),
        "Pod should have agent container: {}",
        ps_output
    );

    // Test devaipod list shows the instance
    let list_output = run_devaipod(&["list"])?;
    list_output.assert_success("devaipod list");

    // Test devaipod status (use short name for CLI commands)
    let status_output = run_devaipod(&["status", short_name(&pod_name)])?;
    status_output.assert_success("devaipod status");

    // Delete instance (use short name for CLI commands)
    let delete_output = run_devaipod(&["delete", short_name(&pod_name), "--force"])?;
    delete_output.assert_success("devaipod delete");

    // Verify pod is gone
    let pod_exists_after = cmd!(sh, "podman pod exists {pod_name}")
        .ignore_status()
        .output()?;
    assert!(
        !pod_exists_after.status.success(),
        "Pod {} should not exist after 'devaipod delete'",
        pod_name
    );

    Ok(())
}
podman_integration_test!(test_pod_creation_and_deletion);

fn test_workspace_container_has_repo() -> Result<()> {
    let repo = TestRepo::new()?;
    let pod_name = unique_test_name("test-repo");

    let mut pods = PodGuard::new();
    pods.add(&pod_name);

    // Create pod (pass short name, devaipod adds prefix)
    let output = run_devaipod_in(
        &repo.repo_path,
        &["up", ".", "--name", short_name(&pod_name)],
    )?;
    if !output.success() {
        bail!("devaipod up failed: {}", output.combined());
    }

    let workspace_container = format!("{}-workspace", pod_name);

    // Give containers a moment to start
    std::thread::sleep(std::time::Duration::from_secs(2));

    let sh = shell()?;

    // Verify workspace container has the repository cloned
    let ls_output = cmd!(
        sh,
        "podman exec {workspace_container} ls /workspaces/test-repo"
    )
    .read()?;
    assert!(
        ls_output.contains("README.md"),
        "Workspace should have README.md: {}",
        ls_output
    );

    Ok(())
}
podman_integration_test!(test_workspace_container_has_repo);

fn test_stop_and_start_pod() -> Result<()> {
    let repo = TestRepo::new()?;
    let pod_name = unique_test_name("test-stop");

    let mut pods = PodGuard::new();
    pods.add(&pod_name);

    // Create pod (pass short name, devaipod adds prefix)
    let output = run_devaipod_in(
        &repo.repo_path,
        &["up", ".", "--name", short_name(&pod_name)],
    )?;
    if !output.success() {
        bail!("devaipod up failed: {}", output.combined());
    }

    // Stop instance (use short name for CLI commands)
    let stop_output = run_devaipod(&["stop", short_name(&pod_name)])?;
    stop_output.assert_success("devaipod stop");

    let sh = shell()?;

    // Verify pod is stopped (containers should not be running)
    let ps_output = cmd!(sh, "podman ps -q --filter pod={pod_name}").read()?;
    assert!(
        ps_output.trim().is_empty(),
        "No containers should be running after stop: {}",
        ps_output
    );

    // Start pod again via podman (devaipod up would create a new pod now)
    cmd!(sh, "podman pod start {pod_name}").run()?;

    // Verify pod is running again
    let ps_output2 = cmd!(sh, "podman ps -q --filter pod={pod_name}").read()?;
    assert!(
        !ps_output2.trim().is_empty(),
        "Containers should be running after restart"
    );

    Ok(())
}
podman_integration_test!(test_stop_and_start_pod);

fn test_image_override_creates_pod() -> Result<()> {
    // Create a repo without devcontainer.json
    let repo = TestRepo::new_minimal()?;
    let pod_name = unique_test_name("test-image");

    let mut pods = PodGuard::new();
    pods.add(&pod_name);

    // Create pod with image override - use an image that has git
    // (pass short name, devaipod adds prefix)
    let test_image = std::env::var("DEVAIPOD_TEST_IMAGE")
        .unwrap_or_else(|_| "ghcr.io/bootc-dev/devenv-debian:latest".to_string());
    let output = run_devaipod_in(
        &repo.repo_path,
        &[
            "up",
            ".",
            "--name",
            short_name(&pod_name),
            "--image",
            &test_image,
        ],
    )?;
    if !output.success() {
        bail!("devaipod up --image failed: {}", output.combined());
    }

    let sh = shell()?;

    // Verify pod was created
    let pod_exists = cmd!(sh, "podman pod exists {pod_name}")
        .ignore_status()
        .output()?;
    assert!(
        pod_exists.status.success(),
        "Pod {} should exist after 'devaipod up --image'",
        pod_name
    );

    // Verify workspace container is running
    let format_names = "{{.Names}}";
    let ps_output = cmd!(
        sh,
        "podman ps --filter pod={pod_name} --format {format_names}"
    )
    .read()?;
    assert!(
        ps_output.contains("workspace"),
        "Pod should have workspace container: {}",
        ps_output
    );

    Ok(())
}
podman_integration_test!(test_image_override_creates_pod);

fn test_logs_command() -> Result<()> {
    let repo = TestRepo::new()?;
    let pod_name = unique_test_name("test-logs");

    let mut pods = PodGuard::new();
    pods.add(&pod_name);

    // Create pod (pass short name, devaipod adds prefix)
    let output = run_devaipod_in(
        &repo.repo_path,
        &["up", ".", "--name", short_name(&pod_name)],
    )?;
    if !output.success() {
        bail!("devaipod up failed: {}", output.combined());
    }

    // Give containers a moment to produce logs
    std::thread::sleep(std::time::Duration::from_secs(2));

    // Get logs (should not error even if empty) - use short name for CLI
    let logs_output = run_devaipod(&["logs", short_name(&pod_name)])?;
    // Logs command should succeed even if there are no logs yet
    logs_output.assert_success("devaipod logs");

    Ok(())
}
podman_integration_test!(test_logs_command);

fn test_ssh_runs_command() -> Result<()> {
    let repo = TestRepo::new()?;
    let pod_name = unique_test_name("test-ssh");

    let mut pods = PodGuard::new();
    pods.add(&pod_name);

    // Create pod (pass short name, devaipod adds prefix)
    let output = run_devaipod_in(
        &repo.repo_path,
        &["up", ".", "--name", short_name(&pod_name)],
    )?;
    if !output.success() {
        bail!("devaipod up failed: {}", output.combined());
    }

    // Give containers a moment to start
    std::thread::sleep(std::time::Duration::from_secs(2));

    // Run a command via ssh (use short name for CLI)
    let ssh_output = run_devaipod(&["ssh", short_name(&pod_name), "--", "echo", "hello"])?;
    ssh_output.assert_success("devaipod ssh echo");
    assert!(
        ssh_output.stdout.contains("hello"),
        "ssh should run command and return output: {}",
        ssh_output.combined()
    );

    // Verify we can see the workspace
    let ls_output = run_devaipod(&["ssh", short_name(&pod_name), "--", "ls", "/workspaces"])?;
    ls_output.assert_success("devaipod ssh ls");
    assert!(
        ls_output.stdout.contains("test-repo"),
        "Should see workspace directory: {}",
        ls_output.stdout
    );

    Ok(())
}
podman_integration_test!(test_ssh_runs_command);

fn test_ssh_nonexistent_pod_fails() -> Result<()> {
    // SSH to an instance that doesn't exist should fail gracefully
    // Use a short name since that's what devaipod CLI expects
    let output = run_devaipod(&["ssh", "nonexistent-instance-12345", "--", "echo", "hi"])?;
    assert!(!output.success(), "ssh to nonexistent instance should fail");

    Ok(())
}
podman_integration_test!(test_ssh_nonexistent_pod_fails);

fn test_pod_has_api_credentials() -> Result<()> {
    let repo = TestRepo::new()?;
    let pod_name = unique_test_name("test-api-creds");

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

    let sh = shell()?;

    // Verify pod has API password label
    let format_label = "{{index .Labels \"io.devaipod.api-password\"}}";
    let password = cmd!(sh, "podman pod inspect {pod_name} --format {format_label}").read()?;
    assert!(
        !password.trim().is_empty(),
        "Pod should have io.devaipod.api-password label"
    );
    assert!(
        password.trim().len() >= 32,
        "API password should be at least 32 chars, got: {}",
        password.len()
    );

    // Verify auth proxy port is published (4097 is the auth proxy, 4096 is internal)
    let agent_container = format!("{}-agent", pod_name);
    let port_output = cmd!(sh, "podman port {agent_container} 4097")
        .ignore_status()
        .read()?;
    assert!(
        port_output.contains("127.0.0.1:"),
        "Port 4097 (auth proxy) should be published to localhost: {}",
        port_output
    );

    // Extract the port number
    let port: u16 = port_output
        .trim()
        .split(':')
        .last()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    assert!(port > 0, "Should have a valid port number");

    Ok(())
}
podman_integration_test!(test_pod_has_api_credentials);

fn test_api_authentication_works() -> Result<()> {
    let repo = TestRepo::new()?;
    let pod_name = unique_test_name("test-api-auth");

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

    // Give agent time to start
    std::thread::sleep(std::time::Duration::from_secs(5));

    let sh = shell()?;

    // Get API credentials
    let format_label = "{{index .Labels \"io.devaipod.api-password\"}}";
    let password = cmd!(sh, "podman pod inspect {pod_name} --format {format_label}").read()?;
    let password = password.trim();

    // Get the published auth proxy port (4097)
    let agent_container = format!("{}-agent", pod_name);
    let port_output = cmd!(sh, "podman port {agent_container} 4097").read()?;
    let port: u16 = port_output
        .trim()
        .split(':')
        .last()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);

    // Test that authenticated request works (via auth proxy on published port)
    let url = format!("http://127.0.0.1:{}/session", port);
    let auth_response = cmd!(sh, "curl -sf -u opencode:{password} {url}")
        .ignore_status()
        .output()?;
    assert!(
        auth_response.status.success(),
        "Authenticated API request to proxy should succeed"
    );

    // Test that unauthenticated request to proxy fails (401)
    let unauth_response = cmd!(sh, "curl -sf {url}").ignore_status().output()?;
    assert!(
        !unauth_response.status.success(),
        "Unauthenticated API request to proxy should fail"
    );

    // Test that internal access (inside container) works without auth
    // This verifies opencode serve is running without OPENCODE_SERVER_PASSWORD
    let internal_response = cmd!(
        sh,
        "podman exec {agent_container} curl -sf http://localhost:4096/session"
    )
    .ignore_status()
    .output()?;
    assert!(
        internal_response.status.success(),
        "Internal API request (no auth) should succeed"
    );

    Ok(())
}
podman_integration_test!(test_api_authentication_works);

/// Verify agent container has matching security settings to workspace.
///
/// In rootless podman, capabilities are relative to the user namespace, so both
/// containers should have the same security settings to enable nested containers.
fn test_agent_matches_workspace_security(fixture: &SharedFixture) -> Result<()> {
    let sh = shell()?;
    let workspace = fixture.workspace_container();
    let agent = fixture.agent_container();

    // Check that both containers have SELinux disabled (label=disable) if workspace does
    // We check this by looking at the security options in the container inspect output
    let format_security = "{{json .HostConfig.SecurityOpt}}";
    let workspace_security =
        cmd!(sh, "podman inspect {workspace} --format {format_security}").read()?;
    let agent_security = cmd!(sh, "podman inspect {agent} --format {format_security}").read()?;

    // If workspace has label:disable, agent should too
    if workspace_security.contains("label") {
        assert!(
            agent_security.contains("label"),
            "Agent should have same SELinux settings as workspace.\nWorkspace: {}\nAgent: {}",
            workspace_security,
            agent_security
        );
    }

    // Check that agent doesn't have no-new-privileges (which would block nested containers)
    let format_nnp = "{{.HostConfig.SecurityOpt}}";
    let agent_nnp = cmd!(sh, "podman inspect {agent} --format {format_nnp}").read()?;
    assert!(
        !agent_nnp.contains("no-new-privileges"),
        "Agent should not have no-new-privileges: {}",
        agent_nnp
    );

    Ok(())
}
readonly_test!(test_agent_matches_workspace_security);

/// Verify both workspace and agent containers can run commands that require user namespaces.
///
/// This tests that newuidmap/newgidmap work, which is required for nested containers.
/// We test by checking if unshare --user works (creates a user namespace).
fn test_containers_support_user_namespaces(fixture: &SharedFixture) -> Result<()> {
    let sh = shell()?;
    let workspace = fixture.workspace_container();
    let agent = fixture.agent_container();

    // Test workspace can create user namespace
    let workspace_unshare = cmd!(
        sh,
        "podman exec {workspace} unshare --user --map-root-user id"
    )
    .ignore_status()
    .output()?;

    // Test agent can create user namespace
    let agent_unshare = cmd!(sh, "podman exec {agent} unshare --user --map-root-user id")
        .ignore_status()
        .output()?;

    // If workspace supports user namespaces, agent should too
    if workspace_unshare.status.success() {
        assert!(
            agent_unshare.status.success(),
            "Agent should support user namespaces like workspace.\nWorkspace: success\nAgent stderr: {}",
            String::from_utf8_lossy(&agent_unshare.stderr)
        );
    }

    Ok(())
}
readonly_test!(test_containers_support_user_namespaces);

/// Verify agent container has access to devices when devcontainer.json specifies them.
///
/// This creates a pod with a devcontainer.json that requests /dev/kvm (if available),
/// and verifies the agent container can see it.
fn test_agent_device_passthrough() -> Result<()> {
    use std::path::Path;

    // Skip if /dev/kvm doesn't exist on host
    if !Path::new("/dev/kvm").exists() {
        tracing::info!("Skipping test_agent_device_passthrough: /dev/kvm not available");
        return Ok(());
    }

    let repo = TestRepo::new_with_devcontainer(
        r#"{
    "name": "device-test",
    "image": "ghcr.io/bootc-dev/devenv-debian:latest",
    "runArgs": ["--device", "/dev/kvm"]
}"#,
    )?;
    let pod_name = unique_test_name("test-device");

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

    let sh = shell()?;
    let agent_container = format!("{}-agent", pod_name);
    let workspace_container = format!("{}-workspace", pod_name);

    // Give containers time to start
    std::thread::sleep(std::time::Duration::from_secs(2));

    // Verify workspace has /dev/kvm
    let workspace_kvm = cmd!(sh, "podman exec {workspace_container} test -e /dev/kvm")
        .ignore_status()
        .output()?;
    assert!(
        workspace_kvm.status.success(),
        "Workspace should have /dev/kvm"
    );

    // Verify agent also has /dev/kvm
    let agent_kvm = cmd!(sh, "podman exec {agent_container} test -e /dev/kvm")
        .ignore_status()
        .output()?;
    assert!(
        agent_kvm.status.success(),
        "Agent should have /dev/kvm like workspace"
    );

    Ok(())
}
podman_integration_test!(test_agent_device_passthrough);

/// Verify lifecycle commands (postCreateCommand) run in BOTH workspace and agent containers.
///
/// This is critical for init scripts that configure nested podman, subuid mappings, etc.
/// Both containers need these configurations for nested containers to work.
fn test_lifecycle_commands_run_in_both_containers() -> Result<()> {
    // Create a devcontainer with a postCreateCommand that creates a marker file
    let marker_path = "/tmp/lifecycle-test-marker";
    let devcontainer_json = format!(
        r#"{{
    "name": "lifecycle-test",
    "image": "ghcr.io/bootc-dev/devenv-debian:latest",
    "postCreateCommand": "echo 'lifecycle-ran' > {}"
}}"#,
        marker_path
    );

    let repo = TestRepo::new_with_devcontainer(&devcontainer_json)?;
    let pod_name = unique_test_name("test-lifecycle");

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

    let sh = shell()?;
    let agent_container = format!("{}-agent", pod_name);
    let workspace_container = format!("{}-workspace", pod_name);

    // Give containers time to finish lifecycle commands
    std::thread::sleep(std::time::Duration::from_secs(3));

    // Verify marker file exists in workspace container
    let workspace_marker = cmd!(sh, "podman exec {workspace_container} cat {marker_path}")
        .ignore_status()
        .read()?;
    assert!(
        workspace_marker.contains("lifecycle-ran"),
        "Workspace should have marker file from postCreateCommand: {}",
        workspace_marker
    );

    // Verify marker file exists in agent container
    let agent_marker = cmd!(sh, "podman exec {agent_container} cat {marker_path}")
        .ignore_status()
        .read()?;
    assert!(
        agent_marker.contains("lifecycle-ran"),
        "Agent should have marker file from postCreateCommand: {}",
        agent_marker
    );

    Ok(())
}
podman_integration_test!(test_lifecycle_commands_run_in_both_containers);

/// Verify that a more complex init script runs in both containers.
///
/// This simulates what devenv-init.sh does: creates config files that are needed
/// for nested container operations.
fn test_init_script_configures_both_containers() -> Result<()> {
    // Create a devcontainer with an init script that creates a config file
    let config_path = "/tmp/nested-container-config";
    let devcontainer_json = format!(
        r#"{{
    "name": "init-script-test",
    "image": "ghcr.io/bootc-dev/devenv-debian:latest",
    "postCreateCommand": "echo 'subuid_configured=true' > {} && echo 'Init script completed for user:' $(whoami)"
}}"#,
        config_path
    );

    let repo = TestRepo::new_with_devcontainer(&devcontainer_json)?;
    let pod_name = unique_test_name("test-init");

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

    let sh = shell()?;
    let agent_container = format!("{}-agent", pod_name);
    let workspace_container = format!("{}-workspace", pod_name);

    // Give containers time to finish lifecycle commands
    std::thread::sleep(std::time::Duration::from_secs(3));

    // Verify config file exists and has correct content in workspace
    let workspace_config = cmd!(sh, "podman exec {workspace_container} cat {config_path}")
        .ignore_status()
        .read()?;
    assert!(
        workspace_config.contains("subuid_configured=true"),
        "Workspace should have config from init script: {}",
        workspace_config
    );

    // Verify config file exists and has correct content in agent
    let agent_config = cmd!(sh, "podman exec {agent_container} cat {config_path}")
        .ignore_status()
        .read()?;
    assert!(
        agent_config.contains("subuid_configured=true"),
        "Agent should have config from init script: {}",
        agent_config
    );

    Ok(())
}
podman_integration_test!(test_init_script_configures_both_containers);
