//! CLI integration tests
//!
//! These tests verify CLI behavior that doesn't require podman but still
//! exercises real functionality (not just --help output).

use color_eyre::Result;

use crate::{integration_test, run_devaipod, run_devaipod_in, shell, TestRepo};

fn test_dry_run_shows_config() -> Result<()> {
    let repo = TestRepo::new()?;

    let output = run_devaipod_in(&repo.repo_path, &["up", ".", "--dry-run"])?;
    output.assert_success("devaipod up --dry-run");

    // Dry run should show what would be created.
    // The message comes from tracing (stderr), so check combined output.
    let combined = output.combined();
    assert!(
        combined.contains("Dry run") || combined.contains("dry run"),
        "Expected dry-run message. output:\n{}",
        combined
    );

    Ok(())
}
integration_test!(test_dry_run_shows_config);

fn test_up_requires_git_remote() -> Result<()> {
    // Create a repo without a remote
    let temp_dir = tempfile::TempDir::new()?;
    let repo_path = temp_dir.path().join("no-remote-repo");
    std::fs::create_dir_all(&repo_path)?;

    let sh = shell()?;
    let repo = repo_path.to_str().unwrap();

    // Initialize git repo without remote
    xshell::cmd!(sh, "git -C {repo} init").run()?;
    xshell::cmd!(sh, "git -C {repo} config user.email test@example.com").run()?;
    xshell::cmd!(sh, "git -C {repo} config user.name 'Test User'").run()?;

    // Create devcontainer.json
    let devcontainer_dir = repo_path.join(".devcontainer");
    std::fs::create_dir_all(&devcontainer_dir)?;
    std::fs::write(
        devcontainer_dir.join("devcontainer.json"),
        r#"{"image": "alpine:latest"}"#,
    )?;
    std::fs::write(repo_path.join("README.md"), "# Test\n")?;

    xshell::cmd!(sh, "git -C {repo} add .").run()?;
    xshell::cmd!(sh, "git -C {repo} commit -m 'Initial'").run()?;

    // Should fail without a remote (use --config /dev/null to ignore user config)
    let output = run_devaipod_in(&repo_path, &["up", ".", "--config", "/dev/null"])?;
    assert!(
        !output.success(),
        "Should fail without git remote configured"
    );
    assert!(
        output.combined().contains("remote")
            || output.combined().contains("Remote")
            || output.combined().contains("clone"),
        "Error should mention remote/clone issue: {}",
        output.combined()
    );

    Ok(())
}
integration_test!(test_up_requires_git_remote);

fn test_up_requires_devcontainer_or_image() -> Result<()> {
    // Create a repo without devcontainer.json
    let repo = TestRepo::new_minimal()?;

    // Should fail without --image (use --config /dev/null to ignore user config
    // which may have a default-image set)
    let output = run_devaipod_in(&repo.repo_path, &["up", ".", "--config", "/dev/null"])?;
    assert!(
        !output.success(),
        "Should fail without devcontainer.json or --image"
    );
    assert!(
        output.combined().contains("devcontainer.json")
            || output.combined().contains("devcontainer"),
        "Error should mention devcontainer: {}",
        output.combined()
    );

    Ok(())
}
integration_test!(test_up_requires_devcontainer_or_image);

fn test_image_override_bypasses_devcontainer() -> Result<()> {
    // Create a repo without devcontainer.json
    let repo = TestRepo::new_minimal()?;

    // Should succeed with --image (dry-run to avoid actually creating pod)
    let output = run_devaipod_in(
        &repo.repo_path,
        &["up", ".", "--dry-run", "--image", "alpine:latest"],
    )?;
    output.assert_success("devaipod up --image --dry-run");

    Ok(())
}
integration_test!(test_image_override_bypasses_devcontainer);

fn test_list_works() -> Result<()> {
    // List should work even when there are no pods
    let output = run_devaipod(&["list"])?;
    output.assert_success("devaipod list");
    Ok(())
}
integration_test!(test_list_works);

fn test_internals_output_devcontainer_state() -> Result<()> {
    let repo = TestRepo::new()?;

    let output = run_devaipod(&[
        "internals",
        "output-devcontainer-state",
        repo.repo_path.to_str().unwrap(),
    ])?;
    output.assert_success("devaipod internals output-devcontainer-state");

    // Should output valid JSON
    let info: serde_json::Value =
        serde_json::from_str(&output.stdout).expect("output should be valid JSON");

    // Should have devcontainer_json field with content
    let dc_json = info["devcontainer_json"]
        .as_str()
        .expect("devcontainer_json should be a string");
    assert!(
        dc_json.contains("image"),
        "devcontainer_json should contain image field: {}",
        dc_json
    );

    // Should have a default_branch field
    let branch = info["default_branch"]
        .as_str()
        .expect("default_branch should be a string");
    assert!(!branch.is_empty(), "default_branch should not be empty");

    Ok(())
}
integration_test!(test_internals_output_devcontainer_state);

fn test_internals_output_devcontainer_state_no_devcontainer() -> Result<()> {
    let repo = TestRepo::new_minimal()?;

    let output = run_devaipod(&[
        "internals",
        "output-devcontainer-state",
        repo.repo_path.to_str().unwrap(),
    ])?;
    output.assert_success("devaipod internals output-devcontainer-state (no devcontainer)");

    let info: serde_json::Value =
        serde_json::from_str(&output.stdout).expect("output should be valid JSON");

    // devcontainer_json should be null when no devcontainer.json exists
    assert!(
        info["devcontainer_json"].is_null(),
        "devcontainer_json should be null for repo without devcontainer.json"
    );

    // default_branch should still be present
    assert!(
        info["default_branch"].is_string(),
        "default_branch should still be a string"
    );

    Ok(())
}
integration_test!(test_internals_output_devcontainer_state_no_devcontainer);

fn test_internals_output_devcontainer_state_with_forward_ports() -> Result<()> {
    let image = std::env::var("DEVAIPOD_TEST_IMAGE")
        .unwrap_or_else(|_| "ghcr.io/bootc-dev/devenv-debian:latest".to_string());
    let devcontainer_json = format!(
        r#"{{
    "image": "{}",
    "forwardPorts": [8080, "3000"]
}}"#,
        image
    );
    let repo = TestRepo::new_with_devcontainer(&devcontainer_json)?;

    let output = run_devaipod(&[
        "internals",
        "output-devcontainer-state",
        repo.repo_path.to_str().unwrap(),
    ])?;
    output.assert_success("devaipod internals output-devcontainer-state (forwardPorts)");

    let info: serde_json::Value =
        serde_json::from_str(&output.stdout).expect("output should be valid JSON");

    let dc_content = info["devcontainer_json"]
        .as_str()
        .expect("devcontainer_json should be a string");
    assert!(
        dc_content.contains("forwardPorts"),
        "devcontainer_json should preserve forwardPorts: {}",
        dc_content
    );
    assert!(
        dc_content.contains("8080"),
        "devcontainer_json should contain port 8080: {}",
        dc_content
    );

    Ok(())
}
integration_test!(test_internals_output_devcontainer_state_with_forward_ports);
