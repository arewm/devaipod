//! Control plane integration tests.
//!
//! Tests for the unified pod list endpoint (`GET /api/devaipod/pods`) which
//! returns pod metadata, agent status, and enrichment in a single response.

use color_eyre::Result;

use crate::container_integration_test;

use super::WebFixture;

/// Verify `GET /api/devaipod/pods` returns a JSON array with valid pod entries.
///
/// This is the basic smoke test for the unified endpoint: it must return 200,
/// a valid JSON array, and each entry must have the expected fields. Since the
/// web fixture is running a devaipod pod, we expect at least one entry.
fn test_unified_pod_list() -> Result<()> {
    let fixture = WebFixture::get()?;
    let token = fixture.token().to_string();

    let (status, body) = fixture.curl_in_container("/api/devaipod/pods", Some(&token))?;

    assert_eq!(
        status,
        200,
        "GET /api/devaipod/pods should return 200, got {}: {}",
        status,
        &body[..body.len().min(300)]
    );

    let pods: Vec<serde_json::Value> = serde_json::from_str(&body).map_err(|e| {
        color_eyre::eyre::eyre!(
            "Failed to parse response: {} - body: {}",
            e,
            &body[..body.len().min(500)]
        )
    })?;

    // The web fixture is a running devaipod pod, so we expect at least one entry.
    assert!(
        !pods.is_empty(),
        "Expected at least one pod in the response"
    );

    for pod in &pods {
        let name = pod
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("Pod entry missing 'name': {pod}"));
        assert!(
            name.starts_with("devaipod-"),
            "Pod name should start with 'devaipod-', got: {name}"
        );

        assert!(
            pod.get("status").and_then(|v| v.as_str()).is_some(),
            "Pod '{name}' missing 'status'"
        );

        assert!(
            pod.get("needs_update").and_then(|v| v.as_bool()).is_some(),
            "Pod '{name}' missing boolean 'needs_update'"
        );
    }

    tracing::info!("Unified pod list returned {} entries", pods.len());
    Ok(())
}
container_integration_test!(test_unified_pod_list);
