# Forgejo Integration Design

This document specifies integrating a local Forgejo instance as an optional but default component of devaipod, providing a full code review and CI/CD workflow.

## Vision

```
devaipod run https://github.com/org/repo 'fix the bug'
```

What happens:

1. **Mirror**: Clone/mirror the GitHub repo into local Forgejo
2. **Sandbox**: Spawn agent pod with workspace, agent, gator containers
3. **Work**: Agent works autonomously, can push branches and create draft PRs in Forgejo
4. **CI/CD**: Forgejo Actions run locally to validate changes
5. **Review**: When agent marks PR as ready, human reviews in Forgejo web UI
6. **Sync**: Approved changes push back to GitHub (via push mirror or `gh pr create`)

```
┌─────────────────────────────────────────────────────────────────────────┐
│                           devaipod                                       │
│                                                                          │
│  ┌────────────────────────────────────────────────────────────────────┐ │
│  │  Local Forgejo (http://localhost:3000)                              │ │
│  │  • Mirrored repos from GitHub                                       │ │
│  │  • Agent-created PRs for review                                     │ │
│  │  • Forgejo Actions for local CI/CD                                  │ │
│  │  • Web UI for human code review                                     │ │
│  └────────────────────────────────────────────────────────────────────┘ │
│           ↑                    ↑                    ↑                    │
│           │                    │                    │                    │
│  ┌────────┴───────┐  ┌────────┴───────┐  ┌────────┴───────┐            │
│  │ devaipod-proj1 │  │ devaipod-proj2 │  │ devaipod-proj3 │            │
│  │ • workspace    │  │ • workspace    │  │ • workspace    │            │
│  │ • agent        │  │ • agent        │  │ • agent        │            │
│  │ • gator        │  │ • gator        │  │ • gator        │            │
│  └────────────────┘  └────────────────┘  └────────────────┘            │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    │ Push approved changes
                                    ↓
                          ┌─────────────────┐
                          │     GitHub      │
                          └─────────────────┘
```

## Why Local Forgejo?

### Benefits

1. **Real forge UI for review**: No need to build a custom diff viewer - Forgejo has excellent PR review UI with inline comments, suggestions, approvals
2. **Local CI/CD**: Forgejo Actions run in containers on your machine - fast, free, no secrets leaving your machine
3. **Agent autonomy**: Agent can iterate (push, run CI, fix, retry) without burning GitHub Actions minutes or creating noise
4. **Privacy**: All work happens locally until explicitly synced
5. **Existing tooling**: git push/pull, PR workflows work as expected

### Tradeoffs

1. **Resource overhead**: Forgejo adds ~200-400MB RAM, ~100MB disk
2. **Complexity**: Another service to manage (mitigated by devaipod managing it)
3. **Sync friction**: Need to explicitly sync approved changes to GitHub

## Architecture

### New Components

```
devaipod infrastructure:
├── forgejo container (optional, default enabled)
│   ├── Forgejo server (port 3000)
│   ├── SQLite database (minimal footprint)
│   └── Git repositories (mirrored from GitHub)
├── forgejo-runner container (optional, for CI/CD)
│   └── Executes Forgejo Actions in containers
└── existing pod infrastructure
    └── workspace, agent, gator containers per project
```

### Configuration

```toml
# ~/.config/devaipod.toml

[forgejo]
# Enable local Forgejo instance (default: true)
enabled = true

# Port for web UI (default: 3000)
port = 3000

# Enable Forgejo Actions for local CI/CD (default: true)
actions = true

# Auto-mirror GitHub repos when using `devaipod run` with GitHub URLs
auto-mirror = true

# Push approved PRs back to GitHub automatically (default: false, prompt)
auto-sync = false
```

### Workflow Details

#### 1. Initial Setup

```bash
devaipod init
# ... existing setup ...
# New: "Enable local Forgejo for code review? [Y/n]"
# Creates forgejo container, generates admin credentials
# Stores credentials in podman secret
```

#### 2. Starting a Task

```bash
devaipod run https://github.com/org/repo 'fix the bug'
```

Behind the scenes:
1. Check if repo already mirrored in Forgejo
2. If not, create mirror via Forgejo API: `POST /api/v1/repos/migrate`
3. Create devaipod pod as usual
4. Agent gets Forgejo URL + token instead of GitHub token
5. Agent clones from local Forgejo (fast, on localhost)

#### 3. Agent Work Cycle

Agent has access to:
- `git push` to Forgejo (via token)
- Forgejo MCP tools: `create_pr`, `post_comment`, `get_issue`
- Local CI feedback via Forgejo Actions

Typical cycle:
```
Agent: Makes changes, commits, pushes branch
Agent: Creates draft PR in Forgejo
Agent: Waits for CI (Forgejo Actions)
Agent: CI fails → reads logs, fixes, pushes again
Agent: CI passes → marks PR as ready for review
Agent: Posts comment: "Ready for human review"
```

#### 4. Human Review

Human opens Forgejo web UI:
- See all agent PRs across repos
- Review diffs with syntax highlighting
- Leave inline comments
- Request changes or approve
- Merge to local main branch

#### 5. Sync to GitHub

After approval, sync to GitHub:

```bash
# Option A: devaipod command
devaipod sync proj-1  # Pushes approved changes to GitHub, creates PR

# Option B: Forgejo push mirror (automatic)
# Configure in Forgejo: Settings → Repository → Push Mirror
# Pushes on every merge to main

# Option C: Agent-initiated (via service-gator)
# Agent uses gh CLI through gator to create GitHub PR
```

## CLI Changes

### New Commands

```bash
# Forgejo lifecycle
devaipod forgejo start      # Start Forgejo container
devaipod forgejo stop       # Stop Forgejo container
devaipod forgejo status     # Show Forgejo status and URL
devaipod forgejo open       # Open Forgejo in browser

# Mirror management
devaipod mirror https://github.com/org/repo  # Mirror repo to Forgejo
devaipod mirror --list                        # List mirrored repos

# Sync workflow
devaipod sync <workspace>   # Sync approved changes to GitHub
devaipod sync --all         # Sync all approved PRs
```

### Modified Commands

```bash
devaipod run https://github.com/org/repo 'task'
# New behavior: auto-mirrors to Forgejo, agent works locally

devaipod status <workspace>
# New output: includes Forgejo PR status if exists
```

## MCP Integration

### Forgejo MCP Server

Extend or reuse aipproval-forge's MCP server:

```rust
// Tools available to agent
struct ForgejoMcp {
    // Repository operations
    fn clone_repo(repo: &str) -> Result<()>;
    fn list_repos() -> Result<Vec<Repo>>;
    
    // PR operations  
    fn create_pr(title: &str, head: &str, base: &str, draft: bool) -> Result<PR>;
    fn update_pr(pr_id: u64, title: Option<&str>, draft: Option<bool>) -> Result<PR>;
    fn get_pr(pr_id: u64) -> Result<PR>;
    fn list_prs(state: PRState) -> Result<Vec<PR>>;
    
    // General comment operations
    fn post_comment(pr_id: u64, body: &str) -> Result<Comment>;
    fn get_comments(pr_id: u64) -> Result<Vec<Comment>>;
    
    // Code review operations (line-level feedback)
    fn get_reviews(pr_id: u64) -> Result<Vec<Review>>;
    fn get_review_comments(pr_id: u64, review_id: u64) -> Result<Vec<LineComment>>;
    fn post_review(pr_id: u64, body: &str, event: ReviewEvent, comments: Vec<LineComment>) -> Result<Review>;
    
    // CI status
    fn get_ci_status(commit: &str) -> Result<CIStatus>;
    fn get_ci_logs(run_id: u64) -> Result<String>;
}

// Line-level review comment structure
struct LineComment {
    path: String,              // "src/lib.rs"
    body: String,              // The feedback text
    new_position: u32,         // Line in new code (0 if commenting on deletion)
    old_position: u32,         // Line in old code (0 if commenting on addition)
    diff_hunk: Option<String>, // Context from the diff (in responses)
    resolved: bool,            // Has human marked this resolved?
}

enum ReviewEvent {
    Approve,
    RequestChanges,
    Comment,
}
```

### Review Feedback Loop

Forgejo's review API enables a powerful human-agent feedback loop:

```
Human reviews PR in Forgejo web UI
    │
    ├── Leaves line-level comment: "Consider error handling here"
    │   └── API: POST /repos/.../pulls/1/reviews with comments[]
    │
Agent polls for feedback
    │
    ├── GET /repos/.../pulls/1/reviews → finds review with comments_count > 0
    ├── GET /repos/.../pulls/1/reviews/{id}/comments → gets line comments
    │
    └── For each unresolved comment:
        ├── Read: path="src/lib.rs", position=42, body="Consider error handling"
        ├── Read: diff_hunk shows the surrounding code context
        ├── Agent makes fix, pushes new commit
        └── Agent posts response comment at same location
```

**Key API details:**
- `position` = line number in new code (additions/unchanged)
- `original_position` = line number in old code (deletions)
- `resolver` field is non-null when human marks comment as resolved
- `diff_hunk` contains `@@` header and surrounding lines with `+`/`-` prefixes

### Agent Instructions Update

Agent instructions should include:
```
You have access to a local Forgejo instance for code review.

## Creating a PR

1. Make your changes and commit them
2. Push to a branch: git push origin feature-branch
3. Create a draft PR: use create_pr tool with draft=true
4. Wait for CI to pass (check with get_ci_status)
5. If CI fails, read logs with get_ci_logs, fix issues, push again
6. When CI passes and you're satisfied, mark PR as ready: use update_pr with draft=false
7. Post a comment summarizing what you did

## Responding to Human Feedback

The human will review your PR in the Forgejo web UI. After marking ready, poll for reviews:

1. Use get_reviews to check for new reviews
2. For reviews with state="REQUEST_CHANGES", use get_review_comments to read line-level feedback
3. Each comment includes:
   - path: the file being commented on
   - position: line number in the new code
   - body: the human's feedback
   - diff_hunk: surrounding code context
   - resolved: whether human marked it done
4. Address each unresolved comment:
   - Make the fix in your code
   - Commit and push
   - Optionally respond with post_review explaining your fix
5. When all comments are resolved, the human will approve and merge

## Review States

- APPROVE: Human approved, ready to merge
- REQUEST_CHANGES: Human wants changes, check comments
- COMMENT: General feedback, no approval status
```

## Forgejo Configuration

### Minimal Setup for devaipod

```ini
# app.ini template

[server]
DOMAIN = localhost
HTTP_PORT = 3000
ROOT_URL = http://localhost:3000/

[database]
DB_TYPE = sqlite3
PATH = /data/gitea.db

[service]
DISABLE_REGISTRATION = true    # Single user
REQUIRE_SIGNIN_VIEW = false    # Allow anonymous browsing

[repository]
ENABLE_PUSH_CREATE_USER = true # Push-to-create repos

[actions]
ENABLED = true
DEFAULT_ACTIONS_URL = https://data.forgejo.org

[security]
INSTALL_LOCK = true            # Skip install wizard
```

### Container Images

- **Forgejo**: `codeberg.org/forgejo/forgejo:14-rootless` (~74MB)
- **Runner**: `data.forgejo.org/forgejo/runner:11` (~50MB)

Total: ~125MB additional images

### Resource Footprint

| Resource | Forgejo | Runner | Total |
|----------|---------|--------|-------|
| RAM (idle) | ~150MB | ~50MB | ~200MB |
| RAM (active) | ~300MB | ~100MB+ | ~400MB+ |
| Disk (base) | ~100MB | ~50MB | ~150MB |
| Startup | ~3s | ~1s | ~4s |

## Implementation Phases

### Phase 1: Basic Forgejo Integration (2 weeks)

- [ ] Add Forgejo container to `devaipod init`
- [ ] `devaipod forgejo start/stop/status` commands
- [ ] Auto-start Forgejo when running `devaipod run` with GitHub URL
- [ ] Mirror GitHub repos to Forgejo via API
- [ ] Store Forgejo credentials in podman secrets
- [ ] Update agent to push to Forgejo instead of GitHub

### Phase 2: Agent PR Workflow (2 weeks)

- [ ] Forgejo MCP server (adapt from aipproval-forge or new)
- [ ] Agent instructions for Forgejo workflow
- [ ] Draft PR creation and update
- [ ] Comment posting for status updates
- [ ] Status display in `devaipod status`

### Phase 3: CI/CD Integration (2 weeks)

- [ ] Forgejo Runner container setup
- [ ] Runner registration automation
- [ ] Agent can read CI status and logs
- [ ] Agent retries on CI failure
- [ ] `.github/workflows/` compatibility

### Phase 4: Sync Workflow (1 week)

- [ ] `devaipod sync` command
- [ ] Push mirror configuration
- [ ] GitHub PR creation after local approval
- [ ] Status tracking across GitHub and Forgejo

## Relationship to aipproval-forge

aipproval-forge already implements much of this. Options:

### Option A: Merge Projects

Combine aipproval-forge into devaipod:
- Use aipproval-forge's orchestrator and MCP server
- Keep devaipod's pod architecture and sandboxing
- Single project with full workflow

### Option B: Share Components

Extract shared crates:
- `forgejo-client` - Forgejo API client
- `forgejo-mcp` - MCP server for Forgejo
- `github-mirror` - GitHub ↔ Forgejo sync

Both projects depend on shared crates.

### Option C: Layered Architecture

- devaipod provides sandboxed pods
- aipproval-forge orchestrates issue-driven workflows
- aipproval-forge uses devaipod for execution

### Recommendation

Start with **Option B** (share components), evaluate **Option A** (merge) after both mature.

Immediate action: Extract `forgejo-client` and `forgejo-mcp` from aipproval-forge as workspace crates that devaipod can use.

## Agent UI in Forgejo

Beyond code review, we want the Forgejo UI to show agent status, live thinking, and controls.

### Forgejo Extensibility Reality

Forgejo has **no plugin system**. Options for UI integration:

| Approach | Description | Maintenance |
|----------|-------------|-------------|
| **Template injection** | Use stable injection points (`footer.tmpl`) to load custom JS | Low - survives upgrades |
| **Light fork** | Add Vue components, mount in templates | Medium - merge conflicts |
| **Heavy fork** | Extensive UI redesign | High - diverge from upstream |

### Recommended: Phased Approach

**Phase 1: Overlay panel via template injection**

```html
<!-- $FORGEJO_CUSTOM/templates/custom/footer.tmpl -->
<script src="{{AppSubUrl}}/assets/devaipod-agent-panel.js" defer></script>
```

The injected JS creates a floating panel (like Intercom):
- Shows agent status (running/idle/stopped)
- Live "thinking" view via WebSocket
- Stop/interrupt button
- Links to full agent conversation

```
┌────────────────────────────────────┐
│ PR #42: Fix the bug                │
│                                    │
│ [normal Forgejo PR UI]             │
│                      ┌───────────┐ │
│                      │ 🤖 Agent  │ │
│                      │ ● Running │ │
│                      │           │ │
│                      │ Reading   │ │
│                      │ review... │ │
│                      │           │ │
│                      │ [⏹ Stop] │ │
│                      └───────────┘ │
└────────────────────────────────────┘
```

**Benefits:**
- Zero Forgejo code changes
- `footer.tmpl` is a documented stable injection point
- Can iterate on agent UI independently
- Survives Forgejo upgrades

**Phase 2: Light fork for deeper integration**

If overlay isn't sufficient, fork Forgejo and add:
- `AgentPanel.vue` component in `web_src/js/components/`
- Mount point in issue/PR templates
- New API endpoints: `/api/v1/agent/status`, `/api/v1/agent/stop`
- WebSocket endpoint for live updates

Forgejo uses Vue 3 selectively - adding a component follows existing patterns.

### Sidecar Architecture

The agent panel JS talks to a sidecar service (not directly to the pod):

```
┌──────────────────┐     ┌────────────────────┐     ┌─────────────────┐
│  Forgejo + JS    │────▶│  Agent UI Sidecar  │────▶│  devaipod pods  │
│  (browser)       │ WS  │  (devaipod serve)  │     │  (agent status) │
└──────────────────┘     └────────────────────┘     └─────────────────┘
```

The sidecar (`devaipod controlplane --serve`) provides:
- WebSocket for live agent status/thinking
- REST API for stop/restart
- Auth via Forgejo OAuth2 or shared token

### Agent Panel Features

| Feature | Phase 1 (Overlay) | Phase 2 (Fork) |
|---------|-------------------|----------------|
| Status indicator | ✅ | ✅ |
| Live "thinking" | ✅ | ✅ |
| Stop button | ✅ | ✅ |
| Full log view | Link to separate page | ✅ Inline |
| Chat interface | Link to separate page | ✅ Inline |
| Multiple agents | Dropdown selector | ✅ Sidebar |

## Open Questions

1. **Authentication flow**: How does the agent get a Forgejo token? Options:
   - Pre-created token in podman secret
   - Generate token at pod creation
   - OAuth flow (overkill for single-user)

2. **Multi-repo PRs**: How to handle changes spanning multiple repos?

3. **Branch protection**: Should we enforce approvals in Forgejo, or is it optional?

4. **Forgejo updates**: How to handle Forgejo upgrades? Auto-update? Pin version?

5. **Resource limits**: Should Forgejo/Runner have resource limits? What are sensible defaults?

6. **Fork timing**: When do we graduate from template injection to light fork?
   - Criteria: UX friction, feature needs, maintenance capacity
   - Could maintain both: overlay for "stock Forgejo" users, fork for full experience

7. **Naming**: If we fork, should it have a distinct name? (e.g., "devaipod-forge")

## References

- [Forgejo documentation](https://forgejo.org/docs/)
- [Forgejo Actions](https://forgejo.org/docs/latest/user/actions/)
- [Forgejo Runner](https://code.forgejo.org/forgejo/runner)
- [aipproval-forge](https://github.com/cgwalters/aipproval-forge)
