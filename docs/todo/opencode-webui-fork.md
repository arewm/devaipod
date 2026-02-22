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

### Fork branch, not patches

Rather than carrying patch files in the devaipod repo, maintain a fork with
a `devaipod` branch (initially at `github.com/cgwalters/opencode`, eventually
`github.com/devaipod/opencode`). The branch tracks upstream releases and
carries the devaipod-specific changes on top. The Containerfile clones from
the fork:

```dockerfile
ARG OPENCODE_REPO=https://github.com/cgwalters/opencode
ARG OPENCODE_REF=devaipod  # branch tracking upstream + devaipod changes
RUN git clone --depth=1 -b $OPENCODE_REF $OPENCODE_REPO /build/opencode
RUN cd /build/opencode/packages/app && bun install --frozen-lockfile && bun run build
```

Keeping devaipod code in isolated files (`src/devaipod/`) and only touching
`app.tsx` and `layout.tsx` minimally keeps rebases on upstream tags clean.
Automation (Dependabot, Renovate, or a simple CI job) can open PRs when
upstream cuts a new release, rebasing the `devaipod` branch forward.

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

**Upstream merge burden**: The opencode project releases multiple times per
week. The fork branch approach (rather than carrying patches) makes rebasing
straightforward. Keeping devaipod code in isolated files (`src/devaipod/`)
and only touching `app.tsx` and `layout.tsx` minimally keeps merge conflicts
rare. Contributing multi-server/multi-pod support upstream would further
reduce fork surface.

## Tasks

- [ ] Spike: create `src/devaipod/pods.tsx` with a basic pod list page,
      patch `app.tsx` to add the route, verify it builds and renders
- [ ] Wire pod list to devaipod REST API (`/api/podman/...`, `/api/devaipod/run`)
- [ ] Add sidebar pod icon with navigation to `/pods`
- [ ] Implement pod-prefixed API proxy in `web.rs`
      (`/pods/{name}/api/{*path}` → pod's opencode backend)
- [ ] Create `DevaipodProvider` context for pod state and auth
- [ ] Wire pod selection to `ServerProvider` base URL
- [ ] Remove iframe wrapper, cookie routing, SSE keepalive, and related code
- [ ] Drop `dist/index.html` once the pods page is functional
- [ ] Set up fork repo (cgwalters/opencode, `devaipod` branch) and update
      Containerfile to clone from it
- [ ] Set up automation to rebase `devaipod` branch on upstream releases
