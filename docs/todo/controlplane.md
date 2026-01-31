# Control Plane Design

This document specifies the design for a devaipod control plane - a component that provides visibility and control over running agent pods, including code review workflows.

> **See also**: [forgejo-integration.md](./forgejo-integration.md) for the recommended approach using a local Forgejo instance as the review UI.

## Motivation

Currently, devaipod is purely CLI-driven. Users interact with individual pods via `devaipod attach`, `ssh`, `logs`, etc. As usage scales (multiple pods, longer-running tasks), there's a need for:

1. **Centralized visibility**: See all pods, their status, and what they're working on
2. **Code review**: Review git commits/changes the agent has made before they're pushed
3. **Approval workflows**: Accept/reject agent work at the commit or hunk level
4. **Multi-pod coordination**: Manage multiple concurrent agent sessions

## Recommended Approach: Local Forgejo

The recommended solution is to integrate a local Forgejo instance as the review UI:

```
devaipod run https://github.com/org/repo 'fix the bug'
→ Mirrors repo to local Forgejo
→ Agent creates PRs in Forgejo
→ Human reviews in Forgejo web UI
→ Approved changes sync to GitHub
```

**Benefits:**
- Real forge UI with proper diff viewer, inline comments, approvals
- Local CI/CD via Forgejo Actions
- Agent can iterate autonomously until satisfied
- No custom UI to build and maintain

See [forgejo-integration.md](./forgejo-integration.md) for full specification.

## Alternative: Custom TUI/Web UI

If Forgejo is too heavyweight or a lighter-weight option is preferred, a custom control plane could be built. The rest of this document describes that alternative approach.

## Prior Art Comparison

### OpenHands

OpenHands (https://openhands.dev) is the most directly comparable project:

| Aspect | OpenHands | devaipod (proposed) |
|--------|-----------|---------------------|
| **Language** | Python + TypeScript | Rust |
| **Container** | Docker (primary) | Podman pods (native) |
| **Credential model** | In-container with config | Strict partitioning (agent never sees tokens) |
| **Control plane** | Full React SPA + FastAPI | TUI-first, optional web UI |
| **Architecture** | Agent Server + SDK | Single binary + pod primitives |
| **Human-in-loop** | LLM-based security analyzer, confirmation policies | Trust boundary + explicit review |

**Key OpenHands patterns to consider:**
- Session management with persistence and resume
- WebSocket event streaming for real-time updates
- Confirmation policies (AlwaysConfirm, NeverConfirm, ConfirmRisky)
- SDK-first architecture separating core from interfaces

**devaipod differentiators to preserve:**
- Agent container NEVER receives credentials
- Podman-native (rootless, no daemon, systemd integration path)
- Single Rust binary, minimal dependencies
- service-gator MCP for scoped forge access

### aipproval-forge

[aipproval-forge](https://github.com/cgwalters/aipproval-forge) is a sibling project with significant overlap:

| Aspect | aipproval-forge | devaipod controlplane |
|--------|-----------------|----------------------|
| **Purpose** | Issue-driven AI workflow with Forgejo | Pod management + code review |
| **Language** | Rust | Rust |
| **Container** | Podman + podman-compose | Podman pods |
| **Forge** | Self-hosted Forgejo (local) | GitHub/GitLab via service-gator |
| **Approval model** | `/ok` command syncs to GitHub | Accept/reject in TUI/web |
| **Agent runtime** | Goose + Gemini | opencode (any LLM) |

**Key aipproval-forge patterns to adopt:**

1. **Structured agent output markers**:
   ```
   === AI_ANALYSIS_START ===
   [content]
   === AI_ANALYSIS_END ===
   ```
   Makes parsing agent output reliable for automation.

2. **MCP server for tools**: aipproval-forge provides Forgejo tools via MCP (`get_issue`, `post_comment`, `create_pull_request`). The controlplane could expose similar tools.

3. **Slash commands for approval**: Simple `/ok`, `/cancel` commands in comments. Could adapt for TUI keybindings or API.

4. **Container host socket pattern**: When running in a container, use `CONTAINER_HOST=unix:///run/podman/podman.sock` to spawn sibling containers.

5. **Quadlet deployment**: Native systemd integration via Podman Quadlets for reliable service management.

6. **Agent configuration model**:
   ```rust
   struct AgentSpawnConfig {
       repo_name: String,
       issue_number: u64,
       timeout_minutes: Option<u64>,
       // ...
   }
   ```

**Potential integration paths:**

1. **Unified orchestrator**: aipproval-forge's orchestrator could manage devaipod pods
2. **Shared MCP servers**: Both could use the same Forgejo/GitHub MCP tooling
3. **Local Forgejo as cache**: Use aipproval-forge's Forgejo for local git operations, sync approved changes to GitHub
4. **Converge on one approach**: Consider whether these should merge into a single project

## Proposed Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                      Control Plane Service                          │
│  ┌───────────────────────────────────────────────────────────────┐  │
│  │  Pod Registry                                                  │  │
│  │  • Watch podman for devaipod-* pods                           │  │
│  │  • Track status, task labels, git state                       │  │
│  │  • Aggregate service-gator scopes                             │  │
│  └───────────────────────────────────────────────────────────────┘  │
│                              │                                       │
│  ┌───────────────┐  ┌────────┴────────┐  ┌───────────────────────┐  │
│  │  TUI Client   │  │  HTTP/WS API    │  │  Web UI (future)      │  │
│  │  (ratatui)    │  │  (axum)         │  │  (React/Leptos)       │  │
│  └───────────────┘  └─────────────────┘  └───────────────────────┘  │
│                                                                      │
└─────────────────────────────────────────────────────────────────────┘
         │                    │                      │
         ▼                    ▼                      ▼
┌─────────────────┐  ┌─────────────────┐  ┌─────────────────┐
│ devaipod-proj-1 │  │ devaipod-proj-2 │  │ devaipod-proj-3 │
│ • workspace     │  │ • workspace     │  │ • workspace     │
│ • agent         │  │ • agent         │  │ • agent         │
│ • gator         │  │ • gator         │  │ • gator         │
└─────────────────┘  └─────────────────┘  └─────────────────┘
```

### Component Breakdown

#### 1. Pod Registry

Watches for devaipod pods and maintains state:

```rust
struct PodState {
    name: String,
    status: PodStatus,
    task: Option<String>,       // From io.devaipod.task label
    mode: WorkspaceMode,        // up vs run
    git_state: Option<GitState>,
    agent_health: AgentHealth,
    created_at: DateTime<Utc>,
}

struct GitState {
    branch: String,
    ahead_of_origin: u32,       // Unpushed commits
    dirty_files: u32,
    recent_commits: Vec<CommitSummary>,
}
```

#### 2. TUI Client (Phase 1 - MVP)

Built with ratatui, focusing on review workflow:

```
┌─ devaipod control plane ────────────────────────────────────────────┐
│                                                                      │
│  Pods                        │  Review: proj-1                       │
│  ─────                       │  ──────────────────────────────────   │
│  ● proj-1-abc123    Running  │  Commits (2 unpushed):                │
│    Task: Fix bug #42         │                                       │
│    2 commits unpushed        │  • a1b2c3d fix: Handle edge case      │
│                              │  • e4f5g6h refactor: Extract helper   │
│  ○ proj-2-def456    Idle     │                                       │
│    No task                   │  ┌─ a1b2c3d ──────────────────────┐   │
│                              │  │ src/main.rs                     │   │
│  ○ proj-3-ghi789    Stopped  │  │ @@ -42,6 +42,10 @@              │   │
│                              │  │ -    let x = foo();             │   │
│                              │  │ +    let x = match foo() {      │   │
│                              │  │ +        Ok(v) => v,            │   │
│                              │  │ +        Err(e) => return Err(e)│   │
│                              │  │ +    };                         │   │
│                              │  └────────────────────────────────┘   │
│                              │                                       │
├──────────────────────────────┴───────────────────────────────────────┤
│ [a]ccept commit  [r]eject  [c]omment  [d]iff full  [q]uit  [?]help   │
└──────────────────────────────────────────────────────────────────────┘
```

Key features:
- Left panel: Pod list with status indicators
- Right panel: Git diff viewer for selected pod
- Accept/reject at commit or hunk level
- Comment to request changes (fed back to agent)

#### 3. HTTP/WebSocket API (Phase 2)

REST + WebSocket for programmatic access and web UI:

```
GET  /api/pods                    # List all pods
GET  /api/pods/:name              # Pod details
GET  /api/pods/:name/commits      # Unpushed commits
GET  /api/pods/:name/diff/:sha    # Diff for specific commit
POST /api/pods/:name/review       # Submit review (accept/reject/comment)
WS   /api/pods/:name/events       # Real-time status updates
```

#### 4. Web UI (Phase 3)

React or Leptos SPA providing:
- Same functionality as TUI in a browser
- Rich diff viewer with syntax highlighting
- Side-by-side or unified diff views
- Inline commenting

## CLI Integration

New command structure:

```bash
# Start control plane (TUI by default)
devaipod controlplane

# Start as HTTP server (for web UI or API access)
devaipod controlplane --serve --port 8080

# One-shot review (no persistent TUI)
devaipod review myworkspace        # Review unpushed commits
devaipod review myworkspace --accept-all
```

## Review Workflow

### Agent → Human Review Flow

1. Agent makes commits in workspace
2. Control plane detects unpushed commits via git polling
3. Human reviews in TUI/web:
   - **Accept**: Mark commit(s) as reviewed, optionally auto-push
   - **Reject**: Discard commit, optionally send feedback to agent
   - **Comment**: Send feedback, agent continues working
4. Accepted commits can be pushed (manually or auto)

### Storage for Review State

Options considered:

| Approach | Pros | Cons |
|----------|------|------|
| **git-notes** | Distributed, no extra storage | Complex to query, sync issues |
| **git-appraise** | Purpose-built, CLI-native | Limited maintenance, extra dep |
| **SQLite in pod** | Fast, queryable | Pod-local, lost on delete |
| **SQLite in control plane** | Centralized, survives pod lifecycle | Extra state to manage |
| **Label/annotation on pod** | Simple, podman-native | Limited size, no history |

**Recommendation**: Start with SQLite in control plane data directory (`~/.local/share/devaipod/reviews.db`). Simple, fast, and survives pod lifecycle.

## Testing Strategy

### TUI Testing (ratatui)

Based on research of gitui, lazygit, and ratatui ecosystem:

1. **Snapshot testing with insta**:
```rust
#[test]
fn test_pod_list_render() {
    let app = App::with_mock_pods(vec![mock_pod("proj-1"), mock_pod("proj-2")]);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| app.render(f)).unwrap();
    insta::assert_snapshot!(terminal.backend());
}
```

2. **State transition tests**:
```rust
#[test]
fn test_navigation() {
    let mut app = App::default();
    app.handle_key(KeyCode::Down);
    assert_eq!(app.selected_pod, 1);
}
```

3. **E2E with rexpect** (for actual binary):
```rust
#[test]
fn test_interactive_review() {
    let mut p = spawn("devaipod controlplane", Some(5000)).unwrap();
    p.exp_string("Pods").unwrap();
    p.send("j").unwrap();  // Navigate down
    p.send("a").unwrap();  // Accept
    p.exp_string("Accepted").unwrap();
}
```

### Integration Testing

- Mock podman responses for pod listing
- Test against real pods in CI (using testcontainers or similar)
- Git fixture repos for diff testing

## Implementation Phases

### Phase 1: TUI MVP (Target: 2 weeks)

- [ ] Add `devaipod controlplane` command
- [ ] Pod registry watching via `podman pod ps --format json`
- [ ] Basic ratatui TUI with pod list
- [ ] Git diff viewer for selected pod
- [ ] Accept/reject at commit level
- [ ] Snapshot tests with insta

### Phase 2: Rich Review (Target: 4 weeks)

- [ ] Hunk-level accept/reject
- [ ] Comment workflow (write feedback, restart agent)
- [ ] SQLite storage for review history
- [ ] HTTP API via axum
- [ ] WebSocket for real-time updates

### Phase 3: Web UI (Target: 8 weeks)

- [ ] React or Leptos web frontend
- [ ] Syntax-highlighted diff viewer
- [ ] Authentication for multi-user
- [ ] Integration with GitHub/GitLab for pushing reviewed commits

## Open Questions

1. **Push automation**: Should accepted commits auto-push, or require explicit action?

2. **Agent feedback loop**: How do we communicate rejection/comments back to the running agent? Options:
   - Write to a file the agent watches
   - Send message via opencode API
   - Signal/restart agent with updated task

3. **Multi-user**: For shared control planes, how do we handle concurrent review of the same pod?

4. **Persistent agent**: Should the control plane be able to restart agents that crashed?

## Dependencies

```toml
[dependencies]
ratatui = "0.30"
crossterm = "0.28"
tokio = { version = "1", features = ["full"] }
axum = "0.8"               # Phase 2
tower-http = "0.6"         # Phase 2
rusqlite = "0.32"          # Phase 2
similar = "2"              # Diff generation

[dev-dependencies]
insta = { version = "1.46", features = ["yaml"] }
rexpect = "0.5"
```

## References

- [ratatui](https://ratatui.rs) - TUI framework
- [gitui](https://github.com/gitui-org/gitui) - Reference for git+TUI patterns
- [gitu](https://github.com/altsem/gitu) - Magit-inspired git TUI
- [OpenHands](https://github.com/All-Hands-AI/OpenHands) - Full AI agent platform
- [git-appraise](https://github.com/google/git-appraise) - Distributed code review
- [aipproval-forge](https://github.com/cgwalters/aipproval-forge) - Issue-driven AI workflow with Forgejo
