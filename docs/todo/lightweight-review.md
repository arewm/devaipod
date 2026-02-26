# Lightweight Agent Change Review

**Background**: The [forgejo-integration.md](./forgejo-integration.md) spec describes
a full local Forgejo instance for agent code review and CI/CD. While comprehensive,
it adds significant moving parts (~200-400MB RAM, mirroring logic, Forgejo lifecycle
management, sync workflows). This document explores lighter-weight alternatives that
get us a usable review workflow sooner, potentially as stepping stones toward
the full Forgejo integration if we decide we need it.

The core requirements are:

1. Human can review a set of commits (like a PR diff), not just raw `git diff`
2. Inline comments on specific lines route back to the agent as context
3. Accept/reject controls per commit or per batch
4. Approved changes can be synced upstream (push, create PR, etc.)
5. The workspace container has GH_TOKEN for pushing (it's trusted; the
   human controls the review UI)

OpenCode today already has a "changes" interface where clicking a change
sends file+line context plus a comment to the agent. That's a real foundation.

However, opencode's session diffs are based on a **shadow git repository**
(separate from the project's `.git`) that snapshots the working tree at
LLM step boundaries. This can get out of sync — e.g. when the agent runs
`git checkout`/`rebase`/`reset`, or when external processes modify files.
Building review on top of the **real git commit history** avoids this
entirely: commits are immutable, and `git diff base..head` is always
correct regardless of what happened in between.

## Options

### Option A: Extend the OpenCode Web UI (Recommended starting point)

OpenCode's web UI already shows file changes and supports inline commenting
that feeds back to the agent. Extend it with commit-level review:

**What to add:**
- Commit-range diff view: select a base and head commit, see grouped changes
  (like GitHub's "compare" or PR files view)
- Per-commit or per-range approve/reject buttons
- "Push approved" action that calls the devaipod control plane API, which
  in turn talks to service-gator to push

**Architecture:**

The workspace container already has the agent's git set up as a remote
called `agent` (see `REMOTE_AGENT` in `src/git.rs`). The control plane
runs `git fetch agent` in the workspace to pull agent commits, then reads
from the workspace's copy. Git's content-addressed hashing ensures data
fetched from the untrusted agent is trustworthy. The workspace has
GH_TOKEN and pushes directly after human approval — no service-gator
mediation needed for the push itself.

```
┌─────────────────────────────────────────────┐
│ Browser: OpenCode web UI                    │
│  • Changes view (already exists)            │
│  • + Commit-range diff view                 │
│  • + Approve / Request Changes buttons      │
│  • + "Sync to upstream" button              │
└──────────────────┬──────────────────────────┘
                   │ HTTP/WS (already exists)
                   ▼
┌─────────────────────────────────────────────┐
│ devaipod control plane                      │
│  • Proxies opencode API (already exists)    │
│  • + Review state endpoints                 │
│  • + git fetch agent / git log / git diff   │
│  • + git push origin (after approval)       │
└──────────────────┬──────────────────────────┘
                   │
          ┌────────┴────────┐
          ▼                 ▼
   ┌─────────────┐  ┌──────────────────────┐
   │ agent pod   │  │ workspace container  │
   │ (opencode)  │  │ (has GH_TOKEN,       │
   └─────────────┘  │  agent git remote)   │
                    └──────────────────────┘
```

**Pros:**
- Builds on existing UI and infrastructure, no new services
- Inline commenting already routes to the agent
- Minimal new code: commit-range diff API endpoint + thin UI on top
- No container lifecycle to manage
- opencode upstream may accept generic review features

**Cons:**
- Diff viewer quality depends on what opencode provides (may need Monaco/similar)
- Not a "real forge" experience (no CI integration, no PR semantics)

**Isolation variant:** Run the opencode web frontend in a separate container
from the opencode backend. The frontend container serves static assets and
proxies API calls to the backend. This gives:
- Network isolation: frontend container has no direct access to workspace
  filesystem or credentials
- The control plane can inject the sync/push controls without modifying the
  agent container at all
- Aligns with existing architecture (control plane already proxies opencode)

### Option B: Dedicated Review Container (Thin Custom UI)

A small standalone container running a purpose-built review UI. Not a full
forge, just a git diff viewer with review controls.

**What it does:**
- Mounts the agent's git repo read-only (or clones from it)
- Serves a web UI for browsing commits, viewing diffs, leaving comments
- Comments are sent to the agent via the opencode API (or written to a
  shared file/queue the agent polls)
- Has a "push" button that calls service-gator

**Implementation:** Could be a Rust (axum + askama/maud) or even a vendored
JS diff viewer (e.g., react-diff-viewer) with a minimal backend.

**Pros:**
- Complete control over the review UX
- Isolated from agent — read-only access to git
- Could be very lightweight (~20MB container, ~10MB RAM)

**Cons:**
- We're building a diff viewer from scratch (or assembling one from parts)
- Duplicates effort if we later adopt Forgejo
- Another container to manage

### Option C: git-appraise or Similar Review-in-Git

Use a tool like [git-appraise](https://github.com/google/git-appraise) that
stores reviews as git notes. The human runs review commands locally, the agent
reads review notes from the repo.

**Pros:**
- No additional services
- Reviews travel with the repo
- Very Unix-philosophy

**Cons:**
- git-appraise is largely unmaintained
- No web UI without building one
- Awkward UX for inline comments

### Option D: Forgejo (Full Spec)

As described in [forgejo-integration.md](./forgejo-integration.md). Provides
a real forge experience but with higher resource and complexity cost.
Still the right long-term answer if we want local CI/CD (Forgejo Actions)
or if the lightweight options prove insufficient.

## Recommendation

Start with **Option A** (extend OpenCode web UI). The key insight is that
opencode *already* has the hardest part: a changes view that sends inline
comments to the agent with file+line context. What's missing is:

1. A commit-range selector (show me commits 3..7 as a unified diff)
2. Explicit approve/reject state
3. A "sync upstream" action via the control plane

These are incremental additions to existing infrastructure. The isolation
variant (frontend in separate container) is worth pursuing but not blocking;
the control plane proxy already provides a natural boundary.

If the diff viewer quality proves insufficient, we can integrate Monaco
(which opencode may already use or plan to use) before jumping to Forgejo.

Reserve **Option D** (Forgejo) for when we need local CI/CD or when we have
multiple repos and want a unified dashboard. It's not mutually exclusive —
if we build commit-range review in OpenCode, that work is useful regardless.

## Implementation Status

We're pursuing Option A. The vendored opencode web UI has been extended
with a `GitReviewTab` SolidJS component that replaces the default
session-level review panel when a `DEVAIPOD_AGENT_POD` cookie is present.
The component reuses the existing `SessionReview` diff renderer and
inline comment system from `@opencode-ai/ui`.

### What's done

**Backend (all in `src/web.rs`):**

- `GET /git/log` — structured commit log with `base`/`head` range params,
  defaults to `agent/HEAD`. Uses NUL/RS-delimited `git log` format. (line ~2054)
- `GET /git/diff-range` — per-file diffs with before/after content via
  `git show`, concurrent file fetching, max 100 files. (line ~2216)
- `POST /git/fetch-agent` — runs `git fetch agent` in the workspace
  container. (line ~2431)
- `POST /git/push` — runs `git push origin <branch>` in the workspace
  container. (line ~2476)
- `exec_in_container()` helper for running commands in workspace via
  bollard. (line ~2517)
- Input validation (`is_valid_git_ref`) to block shell injection. (line ~2040)
- Older/simpler endpoints: `GET /git/status`, `GET /git/diff`,
  `GET /git/commits` (lines ~1957-2037).
- Git remote setup during pod creation (`src/pod.rs` `setup_git_remotes()`)
  configures bidirectional `agent`/`workspace` remotes between containers,
  using read-only volume mounts — no network needed for fetch.

**Frontend:**

- `GitReviewTab` component (`opencode-ui/.../git-review-tab.tsx`, ~210 lines):
  fetches commit log, provides a base-commit selector dropdown, fetches
  per-file diffs for the selected range, and delegates rendering to
  `SessionReview` (which provides unified/split diff view, line commenting).
- Wired into `session.tsx`: conditionally replaces the standard opencode
  review panel when the devaipod cookie is detected.

### What's remaining

1. **Fetch trigger in UI** — The `GitReviewTab` reads `agent/HEAD` but
   never calls `POST /git/fetch-agent`. The user has no way to refresh
   agent commits from the UI. Need a "Refresh" button or auto-fetch on
   tab open.

2. **Push/sync button** — The `POST /git/push` endpoint exists but no
   frontend button calls it. Need an "Approve & Push" or "Sync upstream"
   control in the review panel.

3. **Review state tracking** — No backend state machine for
   approve/reject/sync. The push endpoint has no approval gate. The
   design calls for a `ReviewState` struct tracking pending/approved/
   rejected/synced commit ranges, but none of this is implemented yet.
   This may be fine to defer — a simple "push what's visible" flow
   (without formal state tracking) could be a useful first step.

4. **Merge/cherry-pick flow** — The workspace container has the `agent`
   remote, but there's no endpoint for merging or cherry-picking agent
   commits into the workspace branch before pushing. Currently the
   push just pushes whatever the workspace branch points to.

### Implementation path forward

The backend endpoints are complete for the core read path. The immediate
next steps are frontend: add a fetch button and a push button to the
`GitReviewTab`, then decide whether review state tracking is needed
before we can ship a usable flow.

The backend endpoints below are from the original design and remain relevant.

## Implementation Sketch (Option A)

### API Endpoints

```
# Implemented:
GET  /api/devaipod/pods/{name}/git/log?base={sha}&head={sha}
     Returns commit list in range (structured objects with parent SHAs)

GET  /api/devaipod/pods/{name}/git/diff-range?base={sha}&head={sha}
     Returns per-file diffs with before/after content

POST /api/devaipod/pods/{name}/git/fetch-agent
     Runs git fetch agent in workspace container

POST /api/devaipod/pods/{name}/git/push
     Runs git push origin <branch> in workspace container

# Not yet implemented:
GET  /api/devaipod/pods/{name}/review
     Returns current review state (pending commits, approved set, etc.)

POST /api/devaipod/pods/{name}/review
     Body: { "action": "approve|reject|request-changes", "commits": [...], "comment": "..." }
```

### Review State

Persisted in the control plane (SQLite or flat file per pod):

```rust
struct ReviewState {
    /// Commits the agent has produced, grouped into "review sets"
    pending: Vec<CommitRange>,
    /// Ranges the human has approved
    approved: Vec<CommitRange>,
    /// Ranges the human has rejected (with comments sent back to agent)
    rejected: Vec<CommitRange>,
    /// Ranges that have been synced upstream
    synced: Vec<CommitRange>,
}

// CommitRange tracks by SHA. If the agent rebases or amends after a
// rejection, the old SHAs become invalid. Policy: rejected ranges are
// archived (kept for history) and the new commits appear as fresh
// "pending" entries. The control plane detects SHA invalidation by
// checking if the SHAs still exist in the repo's history. This is
// simpler than tracking PR-style "force push" semantics — we just
// treat post-rejection work as a new review round.
```

### Sync Flow

```
Human clicks "Sync" in UI
  → POST /api/devaipod/pods/{name}/sync
  → Control plane verifies commits are in "approved" state
  → Control plane runs `git push origin {branch}` in workspace container
  → Workspace has GH_TOKEN, push goes directly to upstream
  → UI shows sync status
```

### Push Enforcement

Since the workspace container has GH_TOKEN and the human controls the
review UI, the push gate is straightforward: the control plane only runs
`git push` when the commits being pushed are in "approved" state. The
agent container has no push credentials and no direct access to the
upstream remote — it can only produce commits locally.

The control plane is the sole actor that triggers pushes, and it only
does so after explicit human approval in the review UI. This makes the
review UI a hard gate, not a suggestion, without requiring service-gator
to mediate the push flow or maintain an allow-list of approved SHAs.

Service-gator's role is limited to its existing credential scoping for
agent-initiated API calls (e.g. read access, draft PR creation). It does
not participate in the push-after-review flow.

## Open Questions

1. **Upstream opencode changes?** The commit-range diff and review state
   features are generic enough to propose upstream. Worth a conversation
   with the opencode maintainers.

2. **Review granularity**: Per-commit, per-range, or per-file? Starting
   with commit ranges (like PR review) seems right.

3. **Agent notification**: When the human rejects/requests-changes, how
   does the agent learn? For Option A, the primary path is opencode's
   existing inline comment flow (clicking a change sends file+line context
   to the agent). The open question is whether that's sufficient for batch
   rejections (rejecting a whole range with a top-level comment), or whether
   we also need to inject a message into the opencode session directly.

4. **Multi-pod dashboard**: The control plane already knows about all pods.
   A summary view ("Pod A: 3 commits pending review, Pod B: approved,
   ready to sync") is straightforward to add.

## References

- [forgejo-integration.md](./forgejo-integration.md) — full Forgejo spec
- [opencode-web-enhancements.md](./opencode-web-enhancements.md) — prior notes on review in opencode
- [webui.md](./webui.md) — web UI design and status
- [opencode-webui-fork.md](./opencode-webui-fork.md) — plan to extend vendored opencode SPA
