# Agent UI: vendored opencode SPA in devaipod

## Current state (Feb 2026)

The vendored opencode web UI (built from source in the Containerfile with
`VITE_DEVAIPOD=true`) is served by the **pod-api sidecar** container. Each
pod has a sidecar that serves the SPA, proxies opencode API calls to
`localhost:4096` (within the pod's network namespace), and handles git and
PTY endpoints directly.

The control plane's role is limited to:
- Pod lifecycle management (create, start, stop, rebuild)
- Auth (cookie-based login, token validation)
- Discovering the pod-api sidecar's published port
- Serving the iframe wrapper page with a "Back to Pods" navigation bar
- The `/pods` management page (SPA route, outside the opencode SDK stack)

### Architecture

```
Browser → control plane:8080
  ├─ /pods                    Pod management page (SPA route)
  ├─ /_devaipod/agent/{name}/ Iframe wrapper (discovers pod-api port)
  └─ /api/devaipod/...        Pod lifecycle, agent status, proposals

Browser → pod-api:{port}      (via iframe, each pod has its own port/origin)
  ├─ /                        Vendored opencode SPA (index.html)
  ├─ /assets/*                SPA static files (JS, CSS, fonts)
  ├─ /git/*                   Git endpoints (direct process, no exec overhead)
  ├─ /pty/*                   Workspace PTY (WebSocket, bollard exec)
  ├─ /git/events              SSE stream (inotify-based git watcher)
  └─ /*                       Fallback: proxy to opencode at localhost:4096
                              (session, rpc, event, config, etc.)
                              with Basic auth, SSE keepalive for readiness
```

Each pod exposes only one published port (the pod-api sidecar at 8090
internal, random host port). The opencode server port (4096) is NOT
published — the sidecar proxies to it internally.

Since each pod runs on its own origin (different host port), localStorage
is naturally isolated per pod — no monkey-patching or cookie-based scoping
needed.

### Key files

| File | Purpose |
|------|---------|
| `src/pod_api.rs` | Pod-api sidecar: git, PTY, opencode proxy, static UI |
| `src/web.rs` | Control plane: auth, pod management, iframe wrapper |
| `src/pod.rs` | Pod/container creation, sidecar config |
| `opencode-ui/packages/app/src/context/devaipod.tsx` | Pod management context |
| `opencode-ui/packages/app/src/pages/pods.tsx` | Pod management page |
| `opencode-ui/packages/app/src/context/workspace-terminal.tsx` | Workspace PTY client |
| `opencode-ui/packages/app/src/pages/session/git-review-tab.tsx` | Git diff review |
| `opencode-ui/packages/app/src/pages/session/terminal-panel.tsx` | Agent/Workspace terminal tabs |
| `opencode-ui/packages/app/src/utils/devaipod-api.ts` | `isDevaipod()`, `apiFetch`, error reporting |

## Why we vendor the opencode UI

`opencode serve` does not serve its own web UI. Non-API requests are proxied
to `https://app.opencode.ai`. This is unsuitable for devaipod because:

1. Cross-origin iframes are blocked by `X-Frame-Options`/CSP headers
2. The hosted UI would make API calls back to `app.opencode.ai`, not the
   local opencode backend
3. Air-gapped and sandboxed environments can't reach external services

Vendoring the built SPA eliminates all three problems. The opencode SPA
detects it's not on `opencode.ai` and uses `window.location.origin` for
API calls — which on the pod-api sidecar routes to the correct opencode
server automatically.

## Tasks

### Phase 0: Vendor the frontend — DONE

- [x] Extract the 4 packages + root config from opencode v1.1.65 into
      `opencode-ui/`
- [x] Verify the vendored source builds
- [x] Apply existing `opencode-devaipod.patch` changes as a normal commit
- [x] Update Containerfile to build from `opencode-ui/` instead of cloning
      from GitHub
- [x] Remove `patches/opencode-devaipod.patch` (deleted, was dead code)
- [ ] Write an `update-opencode-ui.sh` script for pulling new upstream releases

### Phase 1: Pod management in SPA — DONE

- [x] Create `DevaipodProvider` context with pod list, launch state, agent
      status, proposals, and pod lifecycle actions
- [x] Create pods page with pod list, launch form, advisor section, proposals
- [x] Add `/pods` route in `app.tsx` outside the opencode SDK provider stack
- [x] Redirect `/` to `/pods`
- [x] Add `VITE_DEVAIPOD=true` to Containerfile
- [x] Serve SPA index.html at `/pods`
- [x] Remove terminal button and xterm.js from old control plane UI
- [x] Cookie-based auth (`/_devaipod/login` sets HttpOnly cookie)
- [ ] Add sidebar pod icon with navigation to `/pods`
- [ ] Add "Back to Pods" navigation from session view (currently via
      iframe wrapper bar)

### Phase 2: Git browser and commit-range review — mostly done

- [x] Add `GET /git/log` endpoint with structured commit objects
- [x] Add `GET /git/diff-range` endpoint with per-file before/after content
- [x] Add `POST /git/fetch-agent` and `POST /git/push` endpoints
- [x] Create `GitReviewTab` component reusing `SessionReview` diff renderer
- [x] Wire into session.tsx (conditional on `isDevaipod()`)
- [x] Move git endpoints to pod-api sidecar (direct git, no exec overhead)
- [x] Add inotify-based git watcher + SSE endpoint (`GET /git/events`)
- [x] Subscribe GitReviewTab to SSE for automatic refresh on new commits
- [x] Integration tests for api container, endpoints, and SSE
- [ ] Add push/sync button to GitReviewTab
- [ ] Wire inline comments from commit-range view back to agent prompt context
- [ ] Debug "expand" button in diff view

### Phase 2.5: Workspace and agent terminals — DONE

- [x] Add optional `wsUrl` and `onPtyResize` props to `Terminal` component
- [x] Create `context/workspace-terminal.tsx` (pod-api PTY client at `/pty/*`)
- [x] Add Agent/Workspace type selector toggle to terminal panel
- [x] Wire into session.tsx (conditional on `isDevaipod()`)
- [x] Implement PTY in pod-api sidecar via bollard exec into workspace
      container; session management with ring buffer, WebSocket handler
- [x] Delete `web_terminal.rs`; `forbid(unsafe_code)` crate-wide
- [x] Mount podman socket into api container (`label=disable` for SELinux)
- [ ] Rename existing terminal label to "Agent Terminal" in devaipod mode
      (the `kind` parameter exists in `terminalTabLabel()` but is never passed)

### Phase 3: Review state and sync

- [ ] Add review state endpoints
- [ ] Add sync endpoint — control plane runs `git push origin {branch}` in
      workspace container after verifying commits are in "approved" state
- [ ] Create review controls — approve/reject/sync buttons
- [ ] Review state persistence

### Phase 4: Cleanup — DONE

The pod-api sidecar architecture eliminated the entire iframe/cookie/proxy
workaround layer. The following have been removed:

- [x] `DEVAIPOD_AGENT_POD` cookie infrastructure
- [x] `DEVAIPOD_HEAD_SCRIPT` (server-side script injection)
- [x] `serve_opencode_raw_ui()` and `opencode_index_with_script()`
- [x] SSE keepalive hack in web.rs (moved to pod_api.rs where it belongs)
- [x] Cookie-aware fallback router (`opencode_or_static_fallback`)
- [x] Root-level opencode proxy (`opencode_root_proxy`, `opencode_proxy_impl`)
- [x] All git endpoint handlers in web.rs (replaced by pod_api.rs)
- [x] `exec_in_container` helper (~200-500ms overhead per call)
- [x] `get_pod_opencode_info` (port discovery + password lookup)
- [x] Stop publishing opencode port (4096) to host
- [x] `getPodName`, `getControlPlaneUrl`, `scopeLocalStorageToPod` in SPA
- [x] `patches/opencode-devaipod.patch` (deleted earlier)
- [ ] Drop `dist/index.html` (old control plane UI, still served at
      `/_devaipod/oldui` as fallback)

### Phase 5: Iframe removal

Currently the agent view is embedded in an iframe (wrapper page with "Back
to Pods" bar). Removing the iframe requires:

- [ ] Navigate between `/pods` and agent sessions within the SPA router
      (no full page reload)
- [ ] The `ServerProvider` / `GlobalSDKProvider` must remount when switching
      pods (they read `server.url` once at init)
- [ ] Auth token must reach the pod-api sidecar (currently the SPA runs at
      the sidecar's origin, so no cross-origin issues — but navigating away
      from `/pods` to a different origin requires solving this)

This is a larger refactor. The iframe approach works well enough for now.

## Testing

### Strategy

**Rust unit tests** (`cargo test`): 274 tests covering web.rs routing,
proxy behavior, pod configuration, git operations. Run via `just test-container`
(278 → 274 after removing tests for deleted cookie/git code).

**Bun unit tests** (`bun test` with happy-dom): 46 existing test files.
Good for testing devaipod-specific modules:
- `utils/devaipod-api.ts` — `apiFetch`, error reporting
- `context/workspace-terminal.tsx` — session lifecycle
- `pages/session/terminal-label.ts` — `kind` prefix formatting

**Rust integration tests** (`cargo test -p integration-tests`): verify HTTP
endpoints, auth, static files, and proxying using curl inside a running
devaipod container.

**Playwright E2E tests** (`bun test:e2e`): 33 existing specs. For devaipod
features, the SPA can be served directly from the pod-api sidecar — no
cookie injection needed since `VITE_DEVAIPOD=true` enables all devaipod
code paths at build time.

## Discoveries

- **`exec_in_container` has ~200-500ms overhead per call** through the podman
  VM on macOS — this motivated creating the pod-api sidecar
- **SELinux is enforcing** on the podman machine VM; api container needs
  `label=disable` for the podman socket
- **GlobalSDKProvider does NOT react to URL changes** — it reads `server.url`
  once at init time. This is why iframe removal (Phase 5) is deferred
- **SolidJS `createEffect` reactive tracking** — async functions reading
  store properties inside `createEffect` cause accidental tracking loops;
  must wrap in `untrack()`
- **Each pod on its own origin** naturally isolates localStorage, eliminating
  the need for the `scopeLocalStorageToPod` monkey-patching approach
