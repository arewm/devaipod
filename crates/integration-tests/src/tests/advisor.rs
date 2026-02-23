//! Advisor integration tests
//!
//! Tests for the `devaipod advisor` command and the advisor pod launch flow.
//!
//! CLI-level tests verify that advisor subcommands handle the "no pod" case
//! gracefully. Pod creation tests exercise the actual launch path that
//! uses `--image` to override the devcontainer, which is the flow that
//! was failing with "Failed to clone into workspace volume (exit code 125)".

use color_eyre::eyre::bail;
use color_eyre::Result;

use crate::{
    integration_test, podman_integration_test, run_devaipod, run_devaipod_in, short_name,
    unique_test_name, PodGuard, TestRepo,
};

// =============================================================================
// CLI-level tests (no podman needed)
// =============================================================================

/// `devaipod advisor --status` should succeed even when no advisor pod exists.
fn test_advisor_status_no_pod() -> Result<()> {
    let output = run_devaipod(&["advisor", "--status"])?;
    output.assert_success("advisor --status with no pod");
    assert!(
        output.combined().contains("not found"),
        "Should report advisor not found: {}",
        output.combined()
    );
    Ok(())
}
integration_test!(test_advisor_status_no_pod);

/// `devaipod advisor --proposals` should succeed even when no advisor pod exists.
fn test_advisor_proposals_no_pod() -> Result<()> {
    let output = run_devaipod(&["advisor", "--proposals"])?;
    output.assert_success("advisor --proposals with no pod");
    assert!(
        output.combined().contains("No proposals")
            || output.combined().contains("not running")
            || output.combined().contains("not be running"),
        "Should handle no proposals gracefully: {}",
        output.combined()
    );
    Ok(())
}
integration_test!(test_advisor_proposals_no_pod);

/// The `--mcp` flag should be accepted by `devaipod up --dry-run`.
fn test_mcp_flag_dry_run() -> Result<()> {
    let repo = TestRepo::new()?;
    let output = run_devaipod_in(
        &repo.repo_path,
        &[
            "up",
            ".",
            "--dry-run",
            "--mcp",
            "test=http://localhost:9999/mcp",
        ],
    )?;
    output.assert_success("devaipod up --mcp --dry-run");
    Ok(())
}
integration_test!(test_mcp_flag_dry_run);

// =============================================================================
// Pod creation tests (need podman)
// =============================================================================

/// Test creating a pod from a local repo with `--image` override, mirroring
/// part of the advisor launch flow.
///
/// NOTE: This test uses a local path as the source, which requires the
/// podman VM to have access to the host temp directory. It works when run
/// inside the devaipod container (just test-integration-container) but may
/// fail on macOS with a podman machine due to path visibility. The remote
/// variant (test_advisor_launch_remote_with_image) tests the actual advisor
/// flow and works everywhere.
fn test_advisor_launch_with_image() -> Result<()> {
    let repo = TestRepo::new()?;
    let pod_name = unique_test_name("advisor-img");
    let test_image = std::env::var("DEVAIPOD_TEST_IMAGE")
        .unwrap_or_else(|_| "ghcr.io/bootc-dev/devenv-debian:latest".to_string());

    let mut guard = PodGuard::new();
    guard.add(&pod_name);

    let output = run_devaipod_in(
        &repo.repo_path,
        &[
            "run",
            ".",
            "--name",
            short_name(&pod_name),
            "--image",
            &test_image,
            "--mcp",
            "advisor=http://localhost:8766/mcp",
            "--service-gator-ro",
            "Test advisor task",
        ],
    )?;

    if !output.success() {
        eprintln!("Advisor-like pod creation failed:");
        eprintln!("stdout: {}", output.stdout);
        eprintln!("stderr: {}", output.stderr);
        bail!("advisor-like pod creation failed: {}", output.combined());
    }

    // Verify pod appears in list
    let list_output = run_devaipod(&["list"])?;
    list_output.assert_success("list after advisor-like create");
    let sn = short_name(&pod_name);
    assert!(
        list_output.combined().contains(sn),
        "Pod '{}' should appear in list: {}",
        sn,
        list_output.combined()
    );

    drop(guard);
    Ok(())
}
podman_integration_test!(test_advisor_launch_with_image);

/// Test creating a pod from a remote URL with `--image` override.
///
/// This is the actual advisor flow: a remote URL (dotfiles) is used as the
/// workspace source and the image is overridden to the devaipod container.
/// The combination of remote clone + image override was triggering the
/// "exit code 125" failure in the workspace volume setup.
fn test_advisor_launch_remote_with_image() -> Result<()> {
    let test_image = std::env::var("DEVAIPOD_TEST_IMAGE")
        .unwrap_or_else(|_| "ghcr.io/bootc-dev/devenv-debian:latest".to_string());

    let pod_name = unique_test_name("advisor-remote");

    let mut guard = PodGuard::new();
    guard.add(&pod_name);

    // Use a small public repo as the source (simulating dotfiles)
    let output = run_devaipod(&[
        "run",
        "https://github.com/cgwalters/playground",
        "--name",
        short_name(&pod_name),
        "--image",
        &test_image,
        "--service-gator-ro",
        "Test advisor from remote",
    ])?;

    if !output.success() {
        eprintln!("Remote advisor-like pod creation failed:");
        eprintln!("stdout: {}", output.stdout);
        eprintln!("stderr: {}", output.stderr);
        bail!(
            "remote advisor-like pod creation failed: {}",
            output.combined()
        );
    }

    drop(guard);
    Ok(())
}
podman_integration_test!(test_advisor_launch_remote_with_image);
