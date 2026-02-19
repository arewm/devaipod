# Agent UI: fork vs improved iframe

Two approaches for a better agent UI experience: (A) fork the opencode web UI and
embed devaipod controls directly, or (B) clean up the current iframe approach to feel
more like a single app.

The desired UX is a "browser new-tab page" feel: a clean pod list (like
bookmarks/recent pages), where clicking a pod goes fullscreen into that agent's view,
with an easy way to get back.

## Current architecture

Today devaipod has three separate UI layers:

1. **Control plane** (`dist/index.html`): ~800 lines of hand-written HTML/CSS/JS
   for listing pods, launching workspaces, start/stop/delete actions.
2. **Leptos frontend** (`crates/devaipod-web/`): A Rust/WASM rewrite of (1), not yet
   wired as default.
3. **Vendored opencode** (`/usr/share/devaipod/opencode/`): Built from source at a
   pinned tag (currently v1.1.65), served at `/agent/{name}/` inside an iframe. API
   calls are proxied through the devaipod server to each pod's opencode instance.

The control plane and agent UI are entirely separate applications stitched together
with an iframe and cookie-based routing. This creates friction:

- Two navigation contexts (users click "Open Agent" and land in a different app)
- Duplicate auth patterns (Bearer token for control plane, cookie for agent proxy)
- SSE keepalive hacks needed because the opencode SDK's global-sdk assumes it's
  talking to its own server, not a proxy that may not have a pod context
- No way to see pod status or switch pods from within the agent view
- The Leptos frontend is a second codebase to maintain for the same control-plane
  features

## API version coupling (key finding)

A fork means the web UI version could drift from the opencode backend running in each
pod. Investigation of the opencode API shows this is **manageable but not zero-risk**:

- **No explicit API versioning** — routes are bare (`/session`, `/rpc`, `/global/event`)
  with no `/v1/` prefix.
- **SDK v1/v2 are codegen variants, not API versions** — both target the same endpoints
  from the same OpenAPI spec.
- **No version negotiation** — the health endpoint returns a `version` field but the
  SDK only uses it for display; there's no `minVersion` check or feature-flag handshake.
- **Changes are mostly additive** — between v1.1.65 and v1.2.0 only schema additions
  occurred (new `Event.message.part.delta` event, `Todo.id` made optional). No
  endpoints were added or removed.
- **Unknown events are ignored** — the SDK's event-reducer drops unrecognized types,
  so a newer backend sending new events to an older UI is safe.
- **Release cadence is fast** — several releases per week (v1.1.60 to v1.2.6 in ~5
  days), so drift accumulates quickly if not tracked.

**Practical risk**: Low for minor version skew (a few weeks). Medium if the fork falls
behind by many releases. The main danger is schema tightening (required fields added)
or endpoint removal, neither of which was observed in recent history.

**Mitigation**: Pin the opencode version in pods to match the UI build tag, or add a
version-mismatch warning using the health endpoint.

## Option A: Fork the opencode web UI

### What opencode's web UI is built with

- **SolidJS** + TypeScript (fine-grained reactivity, same model as Leptos)
- **Vite 7** build, **bun** package manager
- **Tailwind CSS v4** for styling
- **Kobalte** (`@kobalte/core`) for accessible headless UI primitives
- **`@solidjs/router`** for client-side routing
- **`@opencode-ai/sdk`** (workspace package) for typed API calls, OpenAPI-generated
- **`@opencode-ai/ui`** (workspace package) for shared components (Button, Dialog, etc.)

Routes: `/` (home/server picker), `/:dir` (directory layout), `/:dir/session/:id?`
(chat session). State management via SolidJS context providers and stores.

### What a fork would add

Fork `anomalyco/opencode`, work in `packages/app/`. Add devaipod-specific pages and
components using the same stack.

New routes:
- `/pods` — pod list (the "new tab page")
- `/pods/new` — launch workspace form
- `/pods/:name` — pod detail / agent view (replaces iframe wrapper)

New context (`DevaipodProvider`): fetches pod list, tracks active pod, passes the
pod's proxied opencode URL to the existing `ServerProvider`. Extend the sidebar to
show pods with status indicators and actions.

### Trade-offs

Advantages:
- Single unified UI, no iframe, one navigation flow
- Eliminates the proxy workarounds (SSE keepalive, cookie routing, CORS)
- Reuses opencode's polished components (terminal, markdown, command palette)
- Faster UI iteration (TS/SolidJS devtools, hot-reload, larger ecosystem)
- Can drop `dist/index.html` and `crates/devaipod-web/`

Costs:
- Upstream merge burden (opencode is actively developed, releases frequently)
- TypeScript in a Rust project (though we already have inline JS)
- Risk of upstream architecture changes spiking merge cost
- Merge conflicts likely in `app.tsx`, `layout.tsx`, build config

### Minimizing fork drift

- Keep devaipod code in isolated files (`src/devaipod/`, new routes, new providers)
- Use opencode's extension points (router, provider tree, sidebar) rather than
  modifying existing components
- Pin to release tags and merge forward periodically
- Consider contributing multi-pod support upstream to reduce fork surface

## Option B: Clean up the iframe approach

The current iframe wrapper (`agent_wrapper` in `web.rs`) renders a 40px header with
a "Back to control plane" link plus a fullscreen iframe. This can be improved
significantly without forking opencode.

### B1: Full-page navigation with injected back button (recommended)

Instead of wrapping opencode in an iframe, navigate directly to `/agent/{name}/`
which serves the vendored opencode SPA. Inject a small floating "back to pods" button
into the served `index.html`.

How it works:
1. Control plane stores the auth token in `sessionStorage` before navigating
2. `window.location.href = '/agent/{name}/'` (full page navigation, no iframe)
3. When serving opencode's `index.html`, the Rust server injects a `<div>` and
   `<script>` before `</body>`: a floating overlay button that reads the token from
   `sessionStorage` and links back to `/?token=...`
4. The opencode SPA loads normally at `/agent/{name}/`, uses `window.location.origin`
   for API calls — routing already works via the existing proxy

What changes in `web.rs`:
- `serve_opencode_static` for `index.html`: read the file, inject the overlay HTML
  before `</body>`, return the modified content
- `agent_wrapper` becomes a redirect to `/agent/{name}/` (or is removed entirely)
- Cookie is still set for root-level API routing

The overlay:
```html
<div id="devaipod-nav" style="position:fixed;top:12px;left:12px;z-index:9999;">
  <a id="devaipod-back" style="background:#16213e;color:#e6e6e6;padding:6px 12px;
    border-radius:6px;text-decoration:none;font-size:13px;opacity:0.7;
    font-family:system-ui;border:1px solid #2a2a4a;"
    onmouseover="this.style.opacity='1'" onmouseout="this.style.opacity='0.7'">
    ← Pods
  </a>
</div>
<script>
  (function(){
    var t = sessionStorage.getItem('devaipod_token');
    var a = document.getElementById('devaipod-back');
    a.href = t ? '/?token=' + encodeURIComponent(t) : '/';
  })();
</script>
```

Advantages:
- No iframe (opencode runs as a real full-page app, no viewport issues)
- Minimal server-side change (~30 lines of Rust for the injection)
- No opencode fork needed
- The floating button is unobtrusive and doesn't interfere with opencode's layout
- Works with any opencode version (just injects HTML, no JS coupling)

Costs:
- Still two separate apps (control plane + opencode), just smoother transitions
- The injected button could visually conflict with opencode's own UI (mitigated by
  positioning and z-index; could also auto-hide after a few seconds)
- No pod switching from within the agent view (must go back to pod list first)

### B2: SPA shell with pre-created iframes

A lightweight SPA shell (vanilla JS) that shows the pod list as the "home" view. On
pod click, hides the pod list and shows a fullscreen iframe (no header bar). A small
floating nav provides pod switching without full page reloads. Pre-created iframes
enable instant switching between pods.

This gives the best UX of the iframe approaches but is more code to maintain and
still has the fundamental iframe limitations (no shared navigation, cookie juggling).

## Comparison

| Approach | UX | Maintenance | Version coupling risk | Effort |
|----------|-----|-------------|----------------------|--------|
| Current (40px header + iframe) | Janky | Low | None (same tag) | Done |
| B1: Full-page nav + injected back button | Good | Low | None (same tag) | Small |
| B2: SPA shell + fullscreen iframes | Better | Medium | None (same tag) | Medium |
| A: Fork opencode UI | Best | Higher | Low-medium (API drift) | Large |

## Recommendation

**Start with B1** (full-page navigation + injected back button). It's the smallest
change that gets the biggest UX improvement: no more visible iframe wrapper, opencode
runs as a real fullscreen app, and there's a clean way back to the pod list. It
requires no opencode fork and no version coupling concerns.

If B1 proves insufficient (e.g., users want pod switching without leaving the agent
view, or we want devaipod controls integrated into the opencode sidebar), then
escalate to option A (fork). The API stability investigation shows version drift is
manageable, so the fork isn't blocked on technical grounds — it's a question of
whether the UX benefit justifies the ongoing maintenance.

## Tasks

### B1 (immediate, low-effort)

- [ ] Modify `serve_opencode_static` in `web.rs` to inject the floating back button
      when serving opencode's `index.html`
- [ ] Store token in `sessionStorage` from control plane before navigation
- [ ] Change control plane "Open Agent" to navigate to `/agent/{name}/` directly
- [ ] Convert `agent_wrapper` to a redirect (or remove it)
- [ ] Test that opencode's SPA routing, SSE, and API calls all work without iframe

### A (future, if B1 isn't enough)

- [ ] Spike: fork opencode, add a `/pods` route with pod list fetched from devaipod API
- [ ] Evaluate: how invasive are the changes to `app.tsx` and `layout.tsx`?
- [ ] Prototype sidebar pod list with status indicators
- [ ] Wire pod selection to opencode's `ServerProvider` (dynamic server URL)
- [ ] Remove iframe wrapper and cookie-based routing
- [ ] Update Containerfile to build from fork instead of upstream
- [ ] Establish upstream merge workflow (periodic rebase on release tags)
- [ ] Drop `dist/index.html` and `crates/devaipod-web/` once fork is stable
