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
5. The agent container never holds push credentials (only service-gator does)

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
│  • + Push/sync endpoint (calls gator)       │
└──────────────────┬──────────────────────────┘
                   │
          ┌────────┴────────┐
          ▼                 ▼
   ┌─────────────┐  ┌──────────────┐
   │ agent pod   │  │ service-gator│
   │ (opencode)  │  │ (has creds)  │
   └─────────────┘  └──────────────┘
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

## Implementation Path

We're pursuing Option A via the opencode fork described in
[opencode-webui-fork.md](./opencode-webui-fork.md). The fork adds a git
browser, commit-range diff view, and review/sync controls as native SolidJS
pages in the opencode SPA, reusing its existing diff renderer (`@pierre/diffs`)
and inline comment system (`CommentsProvider`). See that document's Phase 2
and Phase 3 tasks for the concrete implementation plan.

The backend endpoints below are needed regardless of frontend approach.

## Implementation Sketch (Option A)

### New API Endpoints

```
GET  /api/devaipod/pods/{name}/git/log?base={sha}&head={sha}
     Returns commit list in range (structured objects with parent SHAs)

GET  /api/devaipod/pods/{name}/git/diff?base={sha}&head={sha}
     Returns unified diff for a commit range (like a PR diff)

GET  /api/devaipod/pods/{name}/review
     Returns current review state (pending commits, approved set, etc.)

POST /api/devaipod/pods/{name}/review
     Body: { "action": "approve|reject|request-changes", "commits": [...], "comment": "..." }

POST /api/devaipod/pods/{name}/sync
     Triggers push of approved commits upstream via service-gator
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
  → Control plane calls service-gator to push branch
  → Optionally: service-gator creates upstream PR via forge API
  → UI shows sync status
```

### Service-gator Enforcement

The review approval state should be enforced at the service-gator level,
not just in the UI. Service-gator already has fine-grained permissions
(`read`, `create-draft`, `pending-review`, `push-new-branch`, `write` — see
`GhRepoPermission` in `config.rs`). The default for agent pods is
`read + create-draft`.

The key insight: service-gator should **refuse to create any PR — even a
draft — unless the control plane confirms human approval** for the commits
in question. PR creation is the privileged action that requires human
sign-off.

```
Human approves commits abc123..def456 in the review UI
  → control plane pushes approved SHAs to service-gator
  → service-gator updates its local allow-list

Agent calls service-gator: "create PR for branch X"
  → service-gator checks its local allow-list for the commits on branch X
  → If all commits are approved: create the PR
  → If any commit is not approved: reject (403)
```

The agent can still iterate freely — push branches, make commits, run
local tests — but it cannot create any upstream PR until a human has
reviewed and approved the commits in the opencode UI. This makes the
review UI a hard gate, not a suggestion.

Service-gator's authorization stays self-contained — no synchronous
callback to the control plane on each request. The control plane pushes
state updates to service-gator (which already receives configuration at
pod creation time; this extends that with a dynamic update channel for
approved commits).

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
