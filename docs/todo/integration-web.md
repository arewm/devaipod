# Web UI Integration Testing

🤖 Assisted-by: Opus 4.6

The following text is 100% LLM generated (after research).

---

End-to-end testing of the devaipod web UI against real containers.
All tests described here involve actual podman pods with workspace,
agent, and pod-api containers. There are no mocked git endpoints or
fake container orchestration.

## Motivation

The existing integration tests (`crates/integration-tests/`) validate
container structure, volume mounts, pod lifecycle, and HTTP endpoint
responses, but never open a browser. The vendored opencode-ui has 33
Playwright spec files from upstream, but they test upstream opencode
features (sessions, prompts, settings) against a standalone opencode
server, not the devaipod pod-api sidecar.

The devaipod-specific UI components -- `GitReviewTab`, the gator scope
dialog, the pod management sidebar -- have zero browser test coverage.
The git review flow in particular (agent commits → human reviews diffs
→ approve → push) is the critical path that needs E2E validation.

## Architecture

```
┌─ Playwright (Chromium) ─────────────────────────────┐
│                                                      │
│  Navigates to pod-api's web server                   │
│  Interacts with GitReviewTab, push button, etc.      │
│                                                      │
└──────────────┬───────────────────────────────────────┘
               │ HTTP
               ▼
┌─ Pod-api sidecar ───────────────────────────────────┐
│  Serves vendored opencode UI (static SPA)            │
│  Proxies opencode API to mock-opencode in agent      │
│  Serves /git/log, /git/diff-range, /git/events       │
│  Serves /devaipod/context, /gator/scopes             │
└──────────────┬───────────────────────────────────────┘
               │ volume mounts
        ┌──────┴──────┐
        ▼             ▼
  agent workspace   main workspace
  (commits made     (push target,
   via podman exec)  workspace agent)
```

The test harness creates a real pod, execs `git commit` into the agent
container to simulate agent work, then drives a Chromium browser via
Playwright against the pod-api's HTTP server. No LLM is involved; the
mock-opencode keeps the `/summary` endpoint happy while we test git
review mechanics.

## Prerequisites

### Container image with Playwright browsers

The integration test runner image (`Containerfile` target
`integration-runner`) currently has podman, git, openssh, and tmux.
For web UI tests it would also need Chromium and the Playwright
runtime. Two options:

**Option A: Extend the integration-runner image.** Add Chromium and
Playwright dependencies to the existing runner image. This keeps
everything in one container but increases image size.

**Option B: Separate Playwright runner.** Build a second test image
based on `mcr.microsoft.com/playwright:v1.57.0-noble` (or similar)
that also has the devaipod test binary. This keeps the base integration
runner lean and uses a known-good Playwright+Chromium combination.

Option B is probably better -- Playwright's official images handle the
browser dependency matrix and are tested by Microsoft. The devaipod
test binary can be copied in as a multi-stage build step.

### Published pod-api port

The pod-api sidecar listens on port 8090 inside the pod. To reach it
from the Playwright browser (running outside the pod), we need the
port published to the host. The existing `get_published_port()`
utility in the test harness already handles this for other containers.

The pod-api port needs to be published during pod creation. Currently
`PodSpec::create()` publishes the opencode port (4096) and optionally
SSH (2222). Pod-api's port (8090) is **not** published by default; it
is accessed via the control plane proxy. For integration tests, we need
direct access.

Options:
- Add an `--expose-pod-api` flag to pod creation (test-only)
- Always publish pod-api in integration test mode
- Use the control plane proxy (adds a hop but avoids port changes)

Using the control plane proxy is simplest and tests the real production
path. `DevaipodHarness` already provides authenticated HTTP access; the
Playwright browser just needs the control plane URL with the auth token.

### Devaipod auth token

The pod-api requires a bearer token for all requests. The test harness
captures this from the `devaipod web` stdout. For Playwright, we need
to either:
- Pass the token as a query parameter (`?token=...`) on initial
  navigation, which the SPA reads and stores in `sessionStorage`
- Or inject it via `page.addInitScript()` into `sessionStorage`
  before navigation

The existing opencode-ui fixtures use `page.addInitScript()` for
localStorage seeding, so the same pattern works here.

## Test Fixture Design

### `DevaipodWebFixture`

A new Playwright fixture that wraps `DevaipodHarness` (or equivalent
podman setup) and provides:

```typescript
type DevaipodWebFixture = {
  // The control plane base URL (e.g., http://localhost:38291)
  baseUrl: string
  // Auth token for API access
  token: string
  // Pod name (for constructing API paths)
  podName: string
  // Navigate to the pod's web UI with auth
  gotoAgent: (page: Page) => Promise<void>
  // Exec a git command in the agent container
  agentGitExec: (command: string) => Promise<string>
  // Exec a git command in the workspace container
  workspaceGitExec: (command: string) => Promise<string>
  // Wait for pod-api /git/log to reflect a commit
  waitForCommit: (sha: string) => Promise<void>
}
```

The fixture lifecycle:
1. **beforeAll** (worker-scoped): Start `devaipod web`, create a pod
   from a test repo, wait for pod-api health, exec an initial commit
   into the agent container to have git data ready.
2. **Per-test**: Navigate to the pod's web UI, seed auth token.
3. **afterAll**: Remove pod, kill web server.

Worker-scoping the pod creation is important for performance. Creating
a pod takes 5-15 seconds; sharing one pod across all review-tab tests
avoids that overhead per test. Tests that mutate state (push) need
their own pod or need to reset state between runs.

### Bridging Rust and Playwright

The test harness is Rust (`DevaipodHarness`) but Playwright tests are
TypeScript. Two approaches:

**Approach A: Rust sets up, Playwright tests.**

A Rust integration test creates the pod and execs commits, then spawns
`npx playwright test` as a subprocess with the pod details passed via
environment variables:

```rust
container_integration_test!(test_web_git_review);
fn test_web_git_review() -> Result<()> {
    let mut harness = DevaipodHarness::start()?;
    harness.create_pod(&repo_path, "web-review-test")?;

    // Exec commits into agent container
    podman_exec(&agent_container, "git commit ...");

    // Run Playwright with pod details
    Command::new("npx")
        .args(["playwright", "test", "--project=devaipod"])
        .env("DEVAIPOD_BASE_URL", harness.base_url())
        .env("DEVAIPOD_TOKEN", harness.token())
        .env("DEVAIPOD_POD_NAME", "web-review-test")
        .status()?;
    Ok(())
}
```

This keeps the Rust harness as the orchestrator and Playwright as a
subprocess. The Playwright tests read env vars to know where to connect.

**Approach B: Playwright orchestrates everything.**

A Playwright `globalSetup` script shells out to `devaipod` to create
the pod, captures the token, and stores them in Playwright's
`process.env` for test fixtures. Teardown removes the pod.

This is more self-contained but requires the `devaipod` binary and
podman to be available in the Playwright environment, and duplicates
setup logic that already exists in Rust.

**Recommended: Approach A.** The Rust harness already handles pod
lifecycle robustly (cleanup on drop, volume removal, timeout handling).
Playwright focuses purely on browser interactions.

## Test Scenarios

### 1. GitReviewTab renders agent commits

**Precondition**: Pod created, agent container has 2-3 commits with
file changes across multiple files.

```
1. Navigate to pod's web UI (via control plane proxy or pod-api port)
2. Assert: GitReviewTab is visible (devaipod context detected)
3. Assert: commit log shows all agent commits with correct messages
4. Select a base commit from the dropdown
5. Assert: diff view updates to show changes in the selected range
6. Expand a file diff
7. Assert: before/after content is correct
```

### 2. SSE auto-refresh on new agent commit

```
1. Navigate to review tab, note current commit count
2. Exec a new commit into the agent container
3. Assert (within 5s): commit log auto-refreshes, new commit appears
   (no manual page reload)
```

### 3. Viewed-files tracking gates push button

**Precondition**: Agent has made changes to 3+ files. Push button
and viewed-files tracking are implemented.

```
1. Navigate to review tab
2. Assert: "Approve & Push" button is disabled
3. Expand and scroll through file 1
4. Assert: file 1 is marked as viewed, button still disabled
5. View remaining files
6. Assert: button becomes enabled
```

### 4. Full push flow

**Precondition**: Workspace agent is implemented, pod has a test
upstream (bare git repo). This test needs its own pod (mutates state).

```
1. Agent container makes commits on a feature branch
2. Navigate to review tab, view all files
3. Click "Approve & Push"
4. Assert: push succeeds (UI shows success state)
5. Verify via podman exec: bare upstream repo has the commits
6. Optionally: check Signed-off-by if checkbox was checked
```

### 5. Inline comment routes to agent

```
1. Navigate to review tab, expand a file diff
2. Click on a line to add an inline comment
3. Type a comment and submit
4. Assert: comment is sent to the agent session
   (verify via opencode API or agent container state)
```

### 6. Base commit selector

```
1. Agent has 5+ commits
2. Navigate to review tab
3. Select different base commits from the dropdown
4. Assert: diff view updates correctly for each selection
5. Assert: file count and diff content change appropriately
```

## Frontend Prerequisites

Before these tests can run, the `GitReviewTab` component needs
`data-component` and `data-action` attributes for Playwright
selectors. The upstream opencode convention is to use these attributes
exclusively (never CSS classes or IDs). Needed attributes:

- `data-component="git-review-tab"` on the review tab container
- `data-component="git-commit-log"` on the commit list
- `data-component="git-commit-item"` on individual commits
- `data-component="git-base-selector"` on the base commit dropdown
- `data-component="git-file-diff"` on each file diff section
- `data-action="approve-push"` on the push button
- `data-action="signoff-checkbox"` on the Signed-off-by toggle
- `data-slot="diff-before"` / `data-slot="diff-after"` on diff panels

## CI Integration

### Justfile target

```
test-integration-web: container-build build-integration
    # 1. Ensure Playwright browsers are available
    # 2. Run Rust integration test that creates pod + spawns Playwright
    ...
```

### GitHub Actions

Add a step after the existing `test-integration` job:

```yaml
- name: Web integration tests
  run: just test-integration-web
  timeout-minutes: 15
```

This runs only on the full CI pipeline (push to main, PRs), not on
every commit. The container build is shared with the existing
integration tests to avoid rebuilding.

### Performance budget

Target: the web integration tests should complete in under 3 minutes.
Pod creation is the bottleneck (~10-15s). Sharing a pod across
read-only tests (scenarios 1, 2, 5, 6) keeps total pod creation to
2 pods (one shared, one for the push test). Playwright tests
themselves should be fast (<5s each) since they're testing UI
rendering against an already-running backend.

## Relationship to Existing Tests

This complements, not replaces, the existing test infrastructure:

- **Unit tests** (`cargo test`): Pure logic, no containers
- **Integration tests** (`just test-integration`): Container structure,
  volume mounts, HTTP endpoints, SSH -- verified via Rust HTTP client
  and `podman exec`. No browser.
- **Web integration tests** (this doc): Browser-driven tests against
  real containers. Validates the full user experience from browser
  to container.
- **E2E GitHub tests** (`just test-e2e-gh`): Real GitHub API
  operations (draft PRs, etc.). Slowest, requires network access.

## References

- [lightweight-review.md](./lightweight-review.md) — git review flow design
- [test-performance.md](./test-performance.md) — integration test performance
- [opencode-webui-fork.md](./opencode-webui-fork.md) — vendored opencode SPA
- `crates/integration-tests/src/harness.rs` — `DevaipodHarness`
- `opencode-ui/packages/app/e2e/` — upstream Playwright infrastructure
- `opencode-ui/packages/app/playwright.config.ts` — Playwright config
