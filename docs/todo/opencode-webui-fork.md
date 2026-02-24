# Agent UI: native integration vs iframe

## Current state (Feb 2026)

The vendored opencode web UI (pinned at v1.1.65, built from source in the
Containerfile) runs inside an iframe at `/_devaipod/opencode-ui`. The control
plane is a separate vanilla HTML/JS page (`dist/index.html`). Cookie-based
routing (`DEVAIPOD_AGENT_POD`) directs root-level opencode API calls to the
correct pod's backend.

This works but requires ~450 lines of workaround code in `web.rs`:

- Cookie parsing and setting across two handlers
- SSE keepalive streams to prevent the opencode SDK from error-looping
  when there's no pod context
- Console error interception injected into the vendored index.html
- A complex fallback router that discriminates static files from API calls
  from SSE streams, all arriving at the same root origin
- Manual `/assets/*` routing since the opencode SPA uses absolute paths

Static file serving was recently simplified to use `tower_http::ServeDir`
instead of hand-rolled mime detection and path traversal checks, but the
fundamental complexity comes from running two apps at one origin.

## Why we vendor the opencode UI

`opencode serve` does not serve its own web UI. Non-API requests are proxied
to `https://app.opencode.ai`. This is unsuitable for devaipod because:

1. Cross-origin iframes are blocked by `X-Frame-Options`/CSP headers
2. The hosted UI would make API calls back to `app.opencode.ai`, not the
   local opencode backend
3. Air-gapped and sandboxed environments can't reach external services

Vendoring the built SPA eliminates all three problems. The opencode SPA
detects it's not on `opencode.ai` and uses `window.location.origin` for
API calls.

## Recommended path: extend the vendored UI natively

Since we already build the opencode UI from source at a pinned tag, we can
add devaipod-specific pages and components directly — no iframe needed. The
opencode web app is SolidJS + TypeScript + Vite + Tailwind, and its layout
already provides sidebar, routing, and theming infrastructure we can reuse.

### What this eliminates

The entire iframe/cookie/proxy workaround layer (~450 lines, ~25% of web.rs):

- `DEVAIPOD_AGENT_POD` cookie infrastructure
- `agent_iframe_wrapper()` and related HTML generation
- `serve_opencode_raw_ui()` with injected JavaScript
- SSE keepalive hack (`sse_keepalive_stream`, `is_event_stream_path`)
- Console error interception (`DEVAIPOD_HEAD_SCRIPT`)
- Frontend error report endpoint
- Cookie-aware fallback router (`opencode_or_static_fallback` complexity)
- Root-level opencode proxy (`opencode_root_proxy`)
- `/assets/*` route hijacking
- ~130 lines of related tests

### Architecture

The opencode SPA connects to its server via `window.location.origin`. For
multi-pod support, the devaipod server would proxy pod-specific API paths:

```
/pods/{name}/api/session    → pod's opencode backend (port 4097)
/pods/{name}/api/rpc        → pod's opencode backend
/pods/{name}/api/global/... → pod's opencode backend
```

The SPA would be configured with a per-pod `baseUrl` through the opencode
`ServerProvider` context instead of relying on cookie-based dispatch at root.
This eliminates the entire cookie routing layer.

### Minimal file changes

Files to modify in the opencode source (at build time):

| File | Change |
|------|--------|
| `packages/app/src/app.tsx` | Add `/pods` route before the `/:dir` catch-all |
| `packages/app/src/pages/layout.tsx` | Add pods icon to sidebar bottom (near settings/help) |
| `packages/app/src/pages/layout/sidebar-shell.tsx` | Add `onOpenPods` callback |

New files:

| File | Purpose |
|------|---------|
| `packages/app/src/pages/pods.tsx` | Pod list/management (replaces `dist/index.html`) |
| `packages/app/src/context/devaipod.tsx` | Devaipod state: pod list, auth token, active pod |

### Vendored frontend, not a separate repo

Rather than maintaining a separate fork repo or carrying patch files, vendor
the opencode frontend source directly into the devaipod repo under
`opencode-ui/`. This keeps everything in one repo and makes changes normal
commits.

The opencode web frontend is part of a bun workspace monorepo. The minimal
set of packages needed to build it:

| Directory | Purpose |
|---|---|
| `packages/app/` | The SPA itself (SolidJS + Vite + Tailwind) |
| `packages/ui/` | Shared component library (`@opencode-ai/ui`) |
| `packages/sdk/js/` | API client/types (`@opencode-ai/sdk`) — no runtime deps |
| `packages/util/` | Small utility library (`@opencode-ai/util`) — just zod |

Plus the root `package.json` (for workspace config and bun's `catalog:`
version resolution) and `bun.lock`.

The remaining ~13 packages in the opencode monorepo (CLI, desktop, docs,
extensions, etc.) are not needed and are not vendored.

**Initial import**: Extract the 4 packages + root config from the opencode
repo at the pinned tag, commit as a single "vendor opencode frontend
v1.1.65" commit. Inline any `catalog:` version references that would break
without the full monorepo, or keep the root `package.json` catalog section
intact (bun should tolerate missing workspace members).

**Devaipod changes**: Normal commits on top, isolated in
`packages/app/src/devaipod/` where possible. Modifications to upstream
files (`app.tsx`, `layout.tsx`) kept minimal.

**Upstream sync**: A script (or manual process) to pull updates from a new
opencode release:
1. Fetch the new tag
2. Extract the same 4 packages + root config
3. Apply as a commit, resolve conflicts with our changes
4. Verify the build

This is slightly more work than a `git subtree pull` but avoids the subtree
limitation of requiring a single prefix path. It also avoids maintaining a
second git repository.

**Containerfile change**: Instead of cloning opencode at build time, build
from the vendored source:

```dockerfile
# -- opencode web UI build stage --
FROM ghcr.io/bootc-dev/devenv-debian:latest AS opencode-web
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl unzip ca-certificates && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL https://bun.sh/install | bash && \
    ln -sf /root/.bun/bin/bun /usr/local/bin/bun
COPY opencode-ui/ /build/opencode/
WORKDIR /build/opencode
RUN bun install --frozen-lockfile
WORKDIR /build/opencode/packages/app
RUN bun run build
```

This eliminates the network clone, the patch application step, and the
pinned `OPENCODE_VERSION` ARG. The vendored source is the single source of
truth.

### Reusable opencode components

The opencode UI library (`@opencode-ai/ui`) provides Button, Dialog,
DropdownMenu, Tooltip, Icon, and other accessible primitives. The pods page
would use these directly, getting theme consistency for free.

### Server-side changes

The Rust web server simplifies significantly:

- The fallback handler becomes a plain `ServeDir` for the opencode SPA
- Pod-prefixed API proxy replaces the cookie-based root proxy
- No SSE keepalive needed (the SPA only connects to a pod when one is active)
- No HTML injection needed (devaipod controls are native SPA components)
- Auth can use a single mechanism (bearer token in header or query param)

### Key considerations

**Pod switching**: The opencode `ServerProvider` holds the API base URL. When
switching pods, the devaipod context would update the base URL and the
`GlobalSDKProvider`/`GlobalSyncProvider` need to remount (reconnect SSE, reload
session state). This can be done by keying the providers on the active pod name.

**API version coupling**: The opencode API has no versioning (`/session`,
`/rpc`, not `/v1/session`). Between v1.1.65 and v1.2.x, changes have been
additive (new event types, optional fields). Unknown events are dropped by
the SDK. Risk is low for minor version skew but increases if the fork falls
behind by many releases. Pinning the opencode backend version in pods to match
the UI build tag mitigates this.

**Upstream sync burden**: The opencode project releases frequently.
Keeping devaipod code in isolated files (`packages/app/src/devaipod/`)
and only touching `app.tsx` and `layout.tsx` minimally keeps merge conflicts
rare when pulling new upstream releases into the vendored directory.
Contributing multi-server/multi-pod support upstream would further reduce
the diff we carry.

## Git browser and commit-range review

> See also: [lightweight-review.md](./lightweight-review.md) for the full
> rationale on why we're building this in the opencode UI rather than
> running a local Forgejo.

OpenCode today has a "changes" view (`SessionReview` / `review-tab.tsx`)
that shows session-level file diffs and supports inline line comments that
feed back into the agent's prompt context. This is genuinely useful but
operates at the wrong abstraction level for reviewing agent work: it shows
uncommitted changes relative to session start, not commits.

What we need for a proper review workflow:

### Git commit browser

A new tab/panel (sibling to the existing "Changes" and "Files" tabs) that
shows the commit log for the workspace branch. For each commit: hash, author,
message, timestamp. Clicking a commit shows its diff using the existing
`@pierre/diffs` renderer and `SessionReview` accordion.

Data source: the control plane runs `git fetch agent` in the workspace
container to pull agent commits, then serves them via
`GET /api/devaipod/pods/{name}/git/log` returning structured commit objects.
The workspace already has the agent's git set up as a remote called `agent`
(see `REMOTE_AGENT` in `src/git.rs`), and git's content-addressed hashing
ensures fetched data is trustworthy. The existing `/git/commits` endpoint
returns recent commits but may need richer output — parent SHAs, full diff
per commit.

### Commit-range diff view

Select two commits (or a base ref + HEAD) and see the combined diff, exactly
like a GitHub PR "Files changed" view. This is the core review primitive.

The opencode `Diff` component already supports rendering arbitrary before/after
content pairs. The new piece is a control plane endpoint that returns the
diff for a commit range:

```
GET /api/devaipod/pods/{name}/git/diff?base={sha}&head={sha}
```

returning an array of `FileDiff` objects compatible with the existing
`sync.data.session_diff` format.

### Review actions

Approve/reject/request-changes controls attached to a commit range. The
inline comment system already works — what's missing is:

- A per-range review state (pending → approved/rejected → synced)
- An "approve" action that marks the range and enables the sync button
- A "request changes" action that sends the review comments to the agent
  (the inline comment → prompt context flow already exists; we just need
  to also inject a top-level "changes requested" message)

### Upstream sync

A "Push" / "Create PR" button visible only for approved commit ranges.
Calls the control plane which runs `git push origin {branch}` in the
workspace container (which has GH_TOKEN). The agent container never sees
this flow.

### Upstream-ability

The git browser and commit-range diff are genuinely useful features for any
opencode user, not just devaipod. The review state and sync controls are
devaipod-specific. Structuring the code so the git browser lives in opencode
core (or is easily proposed upstream) while review/sync lives in
`src/devaipod/` keeps the fork surface minimal.

## Tasks

### Phase 0: Vendor the frontend
- [ ] Extract the 4 packages + root config from opencode v1.1.65 into
      `opencode-ui/` (packages/app, packages/ui, packages/sdk/js,
      packages/util, root package.json, bun.lock)
- [ ] Verify the vendored source builds: `cd opencode-ui && bun install && cd packages/app && bun run build`
- [ ] Apply existing `opencode-devaipod.patch` changes as a normal commit
- [ ] Update Containerfile to `COPY opencode-ui/` instead of cloning from GitHub
- [ ] Remove `patches/opencode-devaipod.patch` and `OPENCODE_VERSION` ARG
- [ ] Write an `update-opencode-ui.sh` script for pulling new upstream releases

### Phase 1: Pod management in SPA (replaces dist/index.html)
- [ ] Implement pod-prefixed API proxy in `web.rs`
      (`/pods/{name}/api/{*path}` → pod's opencode backend) — needed before
      the SPA can talk to pod backends without the cookie hack
- [ ] Spike: create `src/devaipod/pods.tsx` with a basic pod list page,
      patch `app.tsx` to add the route, verify it builds and renders
- [ ] Wire pod list to devaipod REST API (`/api/podman/...`, `/api/devaipod/run`)
- [ ] Add sidebar pod icon with navigation to `/pods`
- [ ] Create `DevaipodProvider` context for pod state and auth
- [ ] Wire pod selection to `ServerProvider` base URL

### Phase 2: Git browser and commit-range review
- [ ] Add `GET /api/devaipod/pods/{name}/git/log` endpoint — runs
      `git fetch agent` then `git log` in the workspace container, returns
      structured commit objects with parent SHAs
- [ ] Add `GET /api/devaipod/pods/{name}/git/diff?base={sha}&head={sha}`
      endpoint — reads from workspace's copy of agent commits, returns
      `FileDiff[]`-compatible output
- [ ] Create `src/devaipod/git-browser.tsx` — commit log panel using
      `@opencode-ai/ui` components
- [ ] Create `src/devaipod/commit-review.tsx` — commit-range diff view
      reusing the existing `Diff` and `SessionReview` components
- [ ] Wire inline comments from commit-range view back to agent prompt
      context (extend `CommentsProvider` to support commit-scoped comments)

### Phase 3: Review state and sync
- [ ] Add review state endpoints (`GET/POST /api/devaipod/pods/{name}/review`)
- [ ] Add sync endpoint (`POST /api/devaipod/pods/{name}/sync`) — control
      plane runs `git push origin {branch}` in workspace container (which
      has GH_TOKEN) after verifying commits are in "approved" state
- [ ] Create `src/devaipod/review-controls.tsx` — approve/reject/sync buttons
- [ ] Review state persistence in control plane (SQLite or JSON per pod)
- [ ] Push gate enforcement: control plane only triggers `git push` in the
      workspace when commits are in "approved" state. No service-gator
      callback needed — the workspace pushes directly after human approval.

### Phase 4: Cleanup
- [ ] Remove iframe wrapper, cookie routing, SSE keepalive, and related code
- [ ] Drop `dist/index.html` once the pods page is functional
