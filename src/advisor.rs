//! Advisor MCP server - pod introspection and draft agent proposal management
//!
//! The advisor runs as a sidecar inside the advisor pod and provides MCP tools
//! that an opencode agent can call to:
//!
//! - Inspect other devaipod pods (list, status, logs)
//! - Create and manage draft proposals for launching new agent pods
//!
//! Draft proposals go through a human review cycle: the advisor agent creates
//! them based on its analysis (e.g. of GitHub issues), and a human approves,
//! dismisses, or lets them expire. This ensures human oversight over agent
//! spawning decisions.
//!
//! The MCP HTTP server (SSE transport) will be added in a follow-up; this
//! module provides the data types, storage layer, and pod introspection
//! functions that the server will expose as tools.

use std::path::Path;

use color_eyre::eyre::{Context, Result};
use serde::{Deserialize, Serialize};

/// Default path for draft proposals storage
pub const DRAFTS_PATH: &str = "/var/lib/devaipod-drafts.json";

/// Port for the advisor MCP server
pub const ADVISOR_MCP_PORT: u16 = 8766;

/// Priority level for a draft proposal
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    High,
    Medium,
    Low,
}

/// Status of a draft proposal
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProposalStatus {
    /// Waiting for human review
    #[default]
    Pending,
    /// Approved by human, pod launch pending or in progress
    Approved,
    /// Dismissed by human
    Dismissed,
    /// Expired (source issue closed, etc.)
    Expired,
}

/// A draft proposal for launching an agent pod
///
/// Created by the advisor agent when it identifies work that could be
/// delegated to a new agent pod. Each proposal captures enough context
/// for a human to make an informed approve/dismiss decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftProposal {
    /// Unique identifier (hex-encoded timestamp)
    pub id: String,
    /// Human-readable summary
    pub title: String,
    /// Target repository (e.g. "myorg/backend")
    pub repo: String,
    /// Task description for the agent
    pub task: String,
    /// Why the advisor thinks this is worth doing
    pub rationale: String,
    /// Priority level
    pub priority: Priority,
    /// What triggered this proposal (e.g. "github:myorg/backend#142")
    pub source: Option<String>,
    /// Rough sizing estimate (e.g. "small", "medium", "large")
    pub estimated_scope: Option<String>,
    /// Current status
    #[serde(default)]
    pub status: ProposalStatus,
    /// When the proposal was created (RFC 3339)
    pub created_at: String,
}

/// Collection of draft proposals, persisted as a JSON file
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DraftStore {
    pub proposals: Vec<DraftProposal>,
}

impl DraftStore {
    /// Load from a JSON file, returning an empty store if the file doesn't exist
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = std::fs::read_to_string(path)
            .with_context(|| format!("Reading draft store from {}", path.display()))?;
        serde_json::from_str(&data)
            .with_context(|| format!("Parsing draft store from {}", path.display()))
    }

    /// Save to a JSON file (creates parent directories if needed)
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Creating directory {}", parent.display()))?;
        }
        let data = serde_json::to_string_pretty(self).context("Serializing draft store")?;
        std::fs::write(path, data)
            .with_context(|| format!("Writing draft store to {}", path.display()))
    }

    /// Add a new proposal, returning its generated ID
    pub fn add(&mut self, mut proposal: DraftProposal) -> String {
        let id = generate_proposal_id();
        proposal.id = id.clone();
        self.proposals.push(proposal);
        id
    }

    /// List proposals, optionally filtered by status
    pub fn list(&self, status: Option<&ProposalStatus>) -> Vec<&DraftProposal> {
        match status {
            Some(s) => self.proposals.iter().filter(|p| &p.status == s).collect(),
            None => self.proposals.iter().collect(),
        }
    }

    /// Update a proposal's status by ID, returning the updated proposal if found
    pub fn update_status(&mut self, id: &str, status: ProposalStatus) -> Option<&DraftProposal> {
        if let Some(proposal) = self.proposals.iter_mut().find(|p| p.id == id) {
            proposal.status = status;
            // Re-borrow as immutable
            self.proposals.iter().find(|p| p.id == id)
        } else {
            None
        }
    }
}

/// Generate a unique proposal ID from the current timestamp.
///
/// Uses the same approach as `unique_suffix()` in main.rs: lower bits of
/// the unix timestamp XOR'd with nanoseconds for short, reasonably unique IDs.
fn generate_proposal_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let val = (now.as_secs() & 0xFFFFFF) ^ ((now.subsec_nanos() as u64) & 0xFFFF);
    format!("{val:x}")
}

// ---------------------------------------------------------------------------
// Pod introspection — shell out to podman CLI
// ---------------------------------------------------------------------------

use crate::{get_instance_id, INSTANCE_LABEL_KEY};

/// List all devaipod pods with basic status info.
///
/// Runs `podman pod ps --filter name=devaipod-* --format json` and returns
/// the parsed JSON array. Each element contains fields like `Name`, `Status`,
/// `Created`, etc. as defined by podman's JSON output.
///
/// When `DEVAIPOD_INSTANCE` is set, results are narrowed to pods carrying
/// the matching label. When unset, no instance filtering is performed here
/// because the advisor runs inside an isolated container environment where
/// cross-instance contamination doesn't occur (unlike the host-side CLI/TUI).
pub fn list_pods() -> Result<Vec<serde_json::Value>> {
    let instance_id = get_instance_id();

    let mut args = vec![
        "pod".to_string(),
        "ps".to_string(),
        "--filter".to_string(),
        "name=devaipod-*".to_string(),
    ];
    if let Some(ref id) = instance_id {
        args.push("--filter".to_string());
        args.push(format!("label={INSTANCE_LABEL_KEY}={id}"));
    }
    args.push("--format".to_string());
    args.push("json".to_string());

    let output = std::process::Command::new("podman")
        .args(&args)
        .output()
        .context("Failed to run podman pod ps")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        color_eyre::eyre::bail!("podman pod ps failed: {}", stderr.trim());
    }

    let pods: Vec<serde_json::Value> =
        serde_json::from_slice(&output.stdout).context("Parsing podman pod ps output")?;

    Ok(pods)
}

/// Get detailed status for a specific pod.
///
/// Runs `podman pod inspect <pod_name>` and returns the full JSON object
/// with container details, network config, etc.
pub fn pod_status(pod_name: &str) -> Result<serde_json::Value> {
    let output = std::process::Command::new("podman")
        .args(["pod", "inspect", pod_name])
        .output()
        .with_context(|| format!("Failed to run podman pod inspect {pod_name}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        color_eyre::eyre::bail!("podman pod inspect {pod_name} failed: {}", stderr.trim());
    }

    serde_json::from_slice(&output.stdout)
        .with_context(|| format!("Parsing podman pod inspect output for {pod_name}"))
}

/// Get recent logs from a pod's agent container.
///
/// Runs `podman logs --tail <lines> <pod_name>-agent` and returns the
/// combined stdout/stderr output as a string.
pub fn pod_logs(pod_name: &str, lines: Option<u32>) -> Result<String> {
    let container_name = format!("{pod_name}-agent");
    let tail = lines.unwrap_or(100).to_string();

    let output = std::process::Command::new("podman")
        .args(["logs", "--tail", &tail, &container_name])
        .output()
        .with_context(|| format!("Failed to run podman logs for {container_name}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        color_eyre::eyre::bail!("podman logs {container_name} failed: {}", stderr.trim());
    }

    // podman logs writes to both stdout and stderr; combine them
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&stderr);
    }
    Ok(combined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_draft_store_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drafts.json");

        let mut store = DraftStore::default();
        let id = store.add(DraftProposal {
            id: String::new(), // will be replaced by add()
            title: "Fix CI flake".into(),
            repo: "myorg/backend".into(),
            task: "Investigate and fix the flaky test in ci.yml".into(),
            rationale: "CI has been red for 3 days".into(),
            priority: Priority::High,
            source: Some("github:myorg/backend#142".into()),
            estimated_scope: Some("small".into()),
            status: ProposalStatus::default(),
            created_at: "2025-01-15T10:00:00Z".into(),
        });

        assert!(!id.is_empty());
        assert_eq!(store.proposals.len(), 1);
        assert_eq!(store.proposals[0].id, id);

        store.save(&path).unwrap();

        let loaded = DraftStore::load(&path).unwrap();
        assert_eq!(loaded.proposals.len(), 1);
        assert_eq!(loaded.proposals[0].title, "Fix CI flake");
        assert_eq!(loaded.proposals[0].priority, Priority::High);
    }

    #[test]
    fn test_draft_store_load_missing_file() {
        let store = DraftStore::load(Path::new("/nonexistent/drafts.json")).unwrap();
        assert!(store.proposals.is_empty());
    }

    #[test]
    fn test_draft_store_filter_by_status() {
        let mut store = DraftStore::default();
        store.add(DraftProposal {
            id: String::new(),
            title: "Task A".into(),
            repo: "org/a".into(),
            task: "Do A".into(),
            rationale: "Because".into(),
            priority: Priority::Low,
            source: None,
            estimated_scope: None,
            status: ProposalStatus::Pending,
            created_at: "2025-01-01T00:00:00Z".into(),
        });
        store.add(DraftProposal {
            id: String::new(),
            title: "Task B".into(),
            repo: "org/b".into(),
            task: "Do B".into(),
            rationale: "Because".into(),
            priority: Priority::Medium,
            source: None,
            estimated_scope: None,
            status: ProposalStatus::Approved,
            created_at: "2025-01-02T00:00:00Z".into(),
        });

        // Note: add() overwrites the status field only if already set,
        // but the status we set in the struct is preserved since add()
        // only replaces the id field.
        // However, add() sets the id, not the status. Let's fix status
        // on the second proposal manually for this test.
        store.proposals[1].status = ProposalStatus::Approved;

        let pending = store.list(Some(&ProposalStatus::Pending));
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].title, "Task A");

        let all = store.list(None);
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_update_status() {
        let mut store = DraftStore::default();
        let id = store.add(DraftProposal {
            id: String::new(),
            title: "Test".into(),
            repo: "org/repo".into(),
            task: "Do it".into(),
            rationale: "Why not".into(),
            priority: Priority::Medium,
            source: None,
            estimated_scope: None,
            status: ProposalStatus::Pending,
            created_at: "2025-01-01T00:00:00Z".into(),
        });

        let updated = store.update_status(&id, ProposalStatus::Approved);
        assert!(updated.is_some());
        assert_eq!(updated.unwrap().status, ProposalStatus::Approved);

        let missing = store.update_status("nonexistent", ProposalStatus::Dismissed);
        assert!(missing.is_none());
    }

    #[test]
    fn test_draft_store_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"not json").unwrap();

        let result = DraftStore::load(&path);
        assert!(result.is_err());
    }

    #[test]
    fn test_proposal_id_is_nonempty() {
        let id = generate_proposal_id();
        assert!(!id.is_empty());
        // Should be valid hex
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_serde_priority_roundtrip() {
        let json = serde_json::to_string(&Priority::High).unwrap();
        assert_eq!(json, "\"high\"");
        let parsed: Priority = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, Priority::High);
    }

    #[test]
    fn test_serde_status_default() {
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(default)]
            status: ProposalStatus,
        }
        let w: Wrapper = serde_json::from_str("{}").unwrap();
        assert_eq!(w.status, ProposalStatus::Pending);
    }
}
