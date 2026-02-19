# Web UI

This document describes a potential web-based UI for devaipod that mirrors the TUI experience.

> **See also**: [controlplane.md](./controlplane.md) for related control plane design, which includes Phase 3 web UI plans.

## Implementation Status

**MVP implemented!** The web UI is now functional with:

- [x] `devaipod web` command to start the server
- [x] Token-based authentication (auto-generated or via podman secret)
- [x] Podman socket proxy at `/api/podman/*`
- [x] Git endpoints for workspace operations
- [x] Static HTML/JS frontend with pod list view
- [x] Default CMD in Containerfile is now `devaipod web`
- [x] Integration tests for auth, proxy, and multi-pod scenarios

### Quick Start

```bash
# Run from container (default)
podman run -d -p 8080:8080 --privileged \
  -v $XDG_RUNTIME_DIR/podman/podman.sock:/run/podman/podman.sock \
  -v ~/.config/devaipod.toml:/root/.config/devaipod.toml:ro \
  ghcr.io/cgwalters/devaipod

# Get the URL with token from logs
podman logs <container> | grep "Web UI"

# Or run locally
devaipod web --port 8080
```

### API Endpoints

| Endpoint | Description |
|----------|-------------|
| `GET /health` | Health check (no auth) |
| `GET /api/podman/v5.0.0/libpod/pods/json` | List pods |
| `GET /api/podman/v5.0.0/libpod/containers/json` | List containers |
| `POST /api/podman/v5.0.0/libpod/pods/{name}/start` | Start pod |
| `POST /api/podman/v5.0.0/libpod/pods/{name}/stop` | Stop pod |
| `GET /api/devaipod/pods/{name}/git/status` | Git status |
| `GET /api/devaipod/pods/{name}/git/diff` | Git diff |
| `GET /api/devaipod/pods/{name}/git/commits` | Recent commits |

All `/api/*` endpoints require authentication via `?token=...` or `Authorization: Bearer ...`.

### Future Work

- [ ] Migrate static HTML to Leptos for full Rust/WASM frontend
- [x] Add pod actions (start/stop/delete) to UI
- [ ] Add git diff viewer with syntax highlighting
- [ ] Add code review workflow (accept/reject commits)
- [ ] WebSocket for real-time updates
- [x] Drop `--net=host` requirement (done: use host gateway, all services have auth)

## Dropping `--net=host` Requirement (Done)

We no longer use `--network host`. The control plane container uses
`--add-host=host.containers.internal:host-gateway` and connects to pod-published
ports (auth proxy) via `host.containers.internal:<port>`. All exposed pod services
use auth. This keeps port forwarding working on macOS. See `host_for_pod_services()`
in `podman.rs` and container-mode.md.

**Future**: A [per-pod gateway sidecar](per-pod-gateway-sidecar.md) (Rust) could provide
one authenticated gateway per pod (opencode + optional podman/LLM proxy), simplifying
the central plane and keeping a single port per pod.

## Motivation

The TUI is excellent for terminal users, but a web UI would:

1. Provide access from any device with a browser (including mobile)
2. Enable richer visualizations (syntax-highlighted diffs, inline images)
3. Allow multiple users to view/interact with pods concurrently
4. Integrate with existing workflows (browser bookmarks, sharing links)

## Leveraging OpenCode's Web Interface

OpenCode already provides a web interface via `opencode web` (see [OpenCode Web docs](https://opencode.ai/docs/web/)). Key features:

- Starts a local server with optional mDNS discovery
- Supports attaching a terminal TUI to the same server
- Authentication via `OPENCODE_SERVER_PASSWORD`
- Session management and real-time updates

**Integration approach**: Rather than building a complete custom UI, devaipod could:

1. Run `opencode web` in the agent container (already runs `opencode serve`)
2. Expose the web port through the pod network
3. Provide devaipod-specific wrapping for pod management, review workflows

However, OpenCode's web UI currently requires a terminal for full functionality. For an embedded/no-terminal experience, we'd need a native web frontend.

## Framework Options for Rust/WASM

Given the project's preference for Rust, here's the current state of Rust-based web UI frameworks:

### 1. Leptos (Recommended)

**Website**: https://leptos.dev

**Pros:**
- Fine-grained reactivity (similar to SolidJS) - minimal DOM updates
- Full-stack: server functions (`#[server]`) work seamlessly across client/server boundary
- Excellent type safety across the entire stack
- Active development, good documentation
- SSR support with hydration
- Built-in router with type-safe routes
- Integration with standard web tools (Tailwind, etc.)

**Cons:**
- Requires nightly Rust (for some features)
- Relatively new (though mature enough for production)
- Learning curve for macro-heavy syntax

**Example:**
```rust
#[component]
pub fn PodList() -> impl IntoView {
    let pods = create_resource(|| (), |_| async { fetch_pods().await });
    
    view! {
        <Suspense fallback=|| view! { <p>"Loading pods..."</p> }>
            {move || pods.get().map(|pods| view! {
                <ul>
                    {pods.into_iter().map(|pod| view! {
                        <li>{pod.name}</li>
                    }).collect_view()}
                </ul>
            })}
        </Suspense>
    }
}
```

### 2. Dioxus

**Website**: https://dioxuslabs.com

**Pros:**
- Cross-platform: web, desktop (native), mobile, TUI from one codebase
- React-like API (familiar to React developers)
- Live hot-reloading
- Growing ecosystem
- Could potentially share code between TUI and web

**Cons:**
- Less mature than Leptos for web-specific features
- Desktop/mobile focus may mean web isn't as optimized

**Unique advantage**: Dioxus can target multiple platforms. We could theoretically have:
- `dioxus-web` for browser
- `dioxus-tui` for terminal (though we'd likely keep ratatui)

### 3. Yew

**Website**: https://yew.rs

**Pros:**
- Most mature Rust web framework (longest history)
- Large community, many examples
- Stable API
- Good documentation

**Cons:**
- Virtual DOM approach (less efficient than fine-grained reactivity)
- No built-in SSR (third-party solutions exist)
- Development has slowed compared to Leptos/Dioxus

### 4. Sycamore

**Pros:**
- Fine-grained reactivity
- Smaller bundle sizes
- Simple API

**Cons:**
- Smaller community
- Less documentation
- Development pace slower

### Framework Comparison Matrix

| Aspect | Leptos | Dioxus | Yew | Sycamore |
|--------|--------|--------|-----|----------|
| **Reactivity** | Fine-grained | VDOM (React-like) | VDOM | Fine-grained |
| **SSR** | Built-in | Limited | Third-party | Built-in |
| **Server Functions** | Excellent | Good | Manual | Limited |
| **Maturity** | Production-ready | Production-ready | Mature | Experimental |
| **Bundle Size** | Small | Medium | Medium | Small |
| **Cross-platform** | Web-focused | Web/Desktop/Mobile/TUI | Web only | Web only |
| **Community** | Growing fast | Growing fast | Established | Small |
| **Type Safety** | Excellent | Excellent | Good | Good |

### Recommendation

**Leptos** is the recommended choice for a devaipod web UI:

1. **Type safety across full stack**: Server functions let us write Rust code that seamlessly spans client and server, with compile-time guarantees
2. **Fine-grained reactivity**: Efficient updates without VDOM overhead
3. **Active development**: Regular releases, responsive maintainers
4. **SSR + hydration**: Better initial load performance and SEO (if needed)
5. **Aligns with project values**: Rust-first, minimal runtime overhead

Dioxus would be a reasonable alternative if we wanted to explore cross-platform (e.g., desktop app alongside web), but for web-only, Leptos is more focused.

## Alternative: TypeScript Frontend

While the preference is Rust, it's worth noting the tradeoffs:

**TypeScript/React pros:**
- Larger talent pool
- More mature ecosystem (component libraries, tools)
- Faster iteration for UI work
- Better browser devtools integration

**TypeScript/React cons:**
- Type safety ends at the API boundary
- Different language from backend (context switching)
- Larger runtime dependencies
- Harder to share logic with Rust backend

The gap is narrowing as Rust web frameworks mature. For a project that values correctness and already uses Rust, staying in Rust makes sense.

## Architecture Options

### Option A: Thin Proxy to Podman Socket (Recommended)

Rather than building a custom "Pod Registry" that wraps podman operations, we can expose the podman socket directly (via a thin HTTP proxy) and let the frontend call podman's REST API.

**How it works:**

```
┌─────────────────────────────────────────────────────────────────┐
│  Browser (Leptos WASM)                                           │
│  ┌─────────────────────────────────────────────────────────────┐ │
│  │  Web UI                                                      │ │
│  │  • Calls podman REST API directly                           │ │
│  │  • devaipod-specific logic in WASM                          │ │
│  │  • Code review (git operations via exec)                    │ │
│  └─────────────────────────────────────────────────────────────┘ │
│               │ HTTP                                              │
└───────────────┼──────────────────────────────────────────────────┘
                │
┌───────────────▼──────────────────────────────────────────────────┐
│  Thin Proxy (axum)                                                │
│  • Serves static WASM/HTML/CSS                                   │
│  • Proxies /api/podman/* → unix:///run/user/1000/podman/podman.sock │
│  • Auth layer (optional)                                          │
│  • Minimal devaipod-specific endpoints (git diff, review state)  │
└───────────────┬──────────────────────────────────────────────────┘
                │
                ▼
    ┌─────────────────────────┐
    │  Podman Socket          │
    │  (REST API, libpod)     │
    └─────────────────────────┘
                │
    ┌───────────┼───────────┐
    ▼           ▼           ▼
┌───────┐  ┌───────┐  ┌───────┐
│ Pod 1 │  │ Pod 2 │  │ Pod 3 │
└───────┘  └───────┘  └───────┘
```

**What the frontend gets directly from podman:**

| Podman API | Use Case |
|------------|----------|
| `GET /pods/json` | List all pods (filter by `io.devaipod.*` labels) |
| `GET /pods/{name}/json` | Pod details, status |
| `POST /pods/{name}/start` | Start a stopped pod |
| `POST /pods/{name}/stop` | Stop a running pod |
| `DELETE /pods/{name}` | Remove a pod |
| `GET /containers/json` | List containers in pods |
| `GET /containers/{id}/logs` | Stream container logs |
| `POST /containers/{id}/exec` | Run commands (git status, etc.) |
| `GET /events` | Real-time pod/container events (SSE) |

**What still needs thin wrapping:**

1. **Git operations**: While we *could* use `exec` to run `git diff`, `git log`, etc., parsing the output in WASM is awkward. A thin endpoint that returns structured JSON is cleaner:
   ```
   GET /api/devaipod/pods/{name}/git/status  → { "branch": "main", "ahead": 2, "dirty": true }
   GET /api/devaipod/pods/{name}/git/diff    → [{ "file": "src/main.rs", "hunks": [...] }]
   GET /api/devaipod/pods/{name}/git/commits → [{ "sha": "abc123", "message": "...", ... }]
   ```

2. **Review state**: Accept/reject state needs persistence (SQLite), not in podman:
   ```
   GET  /api/devaipod/pods/{name}/review
   POST /api/devaipod/pods/{name}/review  { "action": "accept", "commits": ["abc123"] }
   ```

3. **OpenCode session info**: Agent status, current task, etc. (could query via exec or dedicated endpoint)

**Proxy implementation (minimal):**

```rust
use axum::{routing::any, Router};
use hyper_unix_socket::UnixSocketConnector;

async fn proxy_to_podman(req: Request) -> Response {
    // Strip /api/podman prefix, forward to socket
    let path = req.uri().path().strip_prefix("/api/podman").unwrap_or("/");
    let client = hyper::Client::unix();
    client.request(/* build request to socket */).await
}

fn app() -> Router {
    Router::new()
        // Direct podman proxy
        .route("/api/podman/*path", any(proxy_to_podman))
        // Thin devaipod-specific endpoints
        .route("/api/devaipod/pods/:name/git/status", get(git_status))
        .route("/api/devaipod/pods/:name/git/diff", get(git_diff))
        .route("/api/devaipod/pods/:name/review", get(get_review).post(post_review))
        // Static files (WASM, HTML, CSS)
        .fallback_service(ServeDir::new("dist"))
}
```

**Benefits of this approach:**

1. **Less code to maintain**: Podman already has a complete REST API with events, logs, exec, etc.
2. **API stability**: Podman's API is stable and well-documented
3. **Real-time events for free**: Podman's `/events` endpoint provides SSE for pod/container lifecycle
4. **Frontend flexibility**: All logic is in WASM, easier to iterate without redeploying backend
5. **Offline-capable**: Could cache pod state in browser for offline viewing

**Tradeoffs:**

1. **Podman dependency**: Frontend code depends on podman API schema (but we already depend on podman)
2. **Some operations need exec**: Git operations require `exec` into workspace container, which is awkward (but we wrap those anyway)

### Authentication: Auto-generated URL Token

Since we're exposing the podman socket (which grants significant system access), authentication is mandatory. The approach: auto-generate a secret token at startup and require it in the URL.

**How it works:**

1. On startup, generate a random token (e.g., 32 bytes, base64url-encoded)
2. Print the full URL with token to stdout (visible in `podman run` output)
3. All API requests must include the token (query param or header)
4. Token is stored only in memory (not persisted)

```
$ podman run --rm -it -p 8080:8080 -v .:/workspace ghcr.io/cgwalters/devaipod
devaipod v0.1.0
Workspace: /workspace
Web UI: http://localhost:8080/?token=Kx7mN2pQr9sT4uVw...

Press Ctrl+C to stop
```

The URL is printed early in startup so the user can click/copy it immediately. For container deployments where the host port differs, users would adjust the host portion accordingly.

When running with `--detach`, users can retrieve the token from logs:

```
$ podman run -d --name myagent -p 8080:8080 ... ghcr.io/cgwalters/devaipod
$ podman logs myagent | grep "Web UI"
Web UI: http://localhost:8080/?token=Kx7mN2pQr9sT4uVw...
```

**Using a podman secret for stable tokens:**

For persistent deployments or when you want a stable token across container restarts, use a podman secret:

```
# Create a secret (once)
$ openssl rand -base64 32 | podman secret create devaipod-web-token -

# Run with the secret
$ podman run -d --name myagent -p 8080:8080 \
    --secret devaipod-web-token \
    -v .:/workspace ghcr.io/cgwalters/devaipod
```

devaipod checks for the secret at `/run/secrets/devaipod-web-token` on startup. If present, it uses that token instead of generating a random one:

```rust
fn load_or_generate_token() -> String {
    let secret_path = Path::new("/run/secrets/devaipod-web-token");
    if secret_path.exists() {
        fs::read_to_string(secret_path)
            .expect("Failed to read secret")
            .trim()
            .to_string()
    } else {
        let bytes: [u8; 32] = rand::rng().random();
        base64_url::encode(&bytes)
    }
}
```

When using a secret, the startup output indicates the token source:

```
$ podman run ... --secret devaipod-web-token ...
devaipod v0.1.0
Workspace: /workspace
Web UI: http://localhost:8080/?token=<from-secret>
        (token loaded from /run/secrets/devaipod-web-token)
```

**Benefits of secrets:**

| Aspect | Auto-generated | Podman Secret |
|--------|----------------|---------------|
| Stable across restarts | No | Yes |
| Visible in logs | Yes (once) | No (shows `<from-secret>`) |
| Shareable with team | Awkward | Easy (share secret creation) |
| Quadlet/systemd friendly | No | Yes |
| Rotation | Restart container | Update secret, restart |

For Quadlet deployments, reference the secret in the unit file:

```ini
# ~/.config/containers/systemd/devaipod.container
[Container]
Image=ghcr.io/cgwalters/devaipod
PublishPort=8080:8080
Secret=devaipod-web-token
Volume=./myproject:/workspace
```

**Implementation:**

```rust
use rand::Rng;
use axum::{
    extract::Query,
    http::StatusCode,
    middleware::{self, Next},
    response::Response,
};

#[derive(Clone)]
struct AuthToken(String);

fn generate_token() -> String {
    let bytes: [u8; 32] = rand::rng().random();
    base64_url::encode(&bytes)
}

async fn auth_middleware(
    State(expected): State<AuthToken>,
    Query(params): Query<HashMap<String, String>>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    // Check query param
    if let Some(token) = params.get("token") {
        if token == &expected.0 {
            return Ok(next.run(req).await);
        }
    }
    // Also check Authorization header (for API calls after initial page load)
    if let Some(auth) = req.headers().get("Authorization") {
        if auth.to_str().ok() == Some(&format!("Bearer {}", expected.0)) {
            return Ok(next.run(req).await);
        }
    }
    Err(StatusCode::UNAUTHORIZED)
}

fn app(token: AuthToken) -> Router {
    Router::new()
        .route("/api/podman/*path", any(proxy_to_podman))
        .route("/api/devaipod/*path", /* ... */)
        .layer(middleware::from_fn_with_state(token.clone(), auth_middleware))
        // Static files don't need auth (they're just HTML/JS/WASM)
        .fallback_service(ServeDir::new("dist"))
}
```

**Frontend token handling:**

The frontend (Leptos WASM) extracts the token from the URL on initial load and includes it in subsequent API calls:

```rust
fn get_auth_token() -> Option<String> {
    let window = web_sys::window()?;
    let search = window.location().search().ok()?;
    let params = web_sys::UrlSearchParams::new_with_str(&search).ok()?;
    params.get("token")
}

async fn fetch_pods(token: &str) -> Result<Vec<Pod>, Error> {
    gloo_net::http::Request::get("/api/podman/pods/json")
        .header("Authorization", &format!("Bearer {}", token))
        .send()
        .await?
        .json()
        .await
}
```

**Security properties:**

| Property | Status |
|----------|--------|
| Token not in server logs | Yes (only printed once at startup) |
| Token not persisted to disk | Yes (memory only) |
| Token rotates on restart | Yes (new token each launch) |
| HTTPS required? | No for localhost, recommended for network |
| Token in URL (referer leakage?) | Mitigated: same-origin only, no external links |

**Comparison with alternatives:**

| Approach | Pros | Cons |
|----------|------|------|
| **URL token (chosen)** | Simple, no login flow, works immediately | Token visible in browser history/URL bar |
| **HTTP Basic Auth** | Browser-native prompt | Annoying to enter, cached awkwardly |
| **Cookie + login page** | Cleaner URL | More code, session management |
| **mTLS client certs** | Strong auth | Complex setup, not browser-friendly |

**Nice-to-haves for later:**

- `--open` flag to auto-open browser with token URL (like `jupyter notebook`)
- Copy-to-clipboard helper in terminal output
- QR code for mobile access on local network
- Optional: allow user to set `DEVAIPOD_WEB_TOKEN` env var for stable token across restarts

### Option B: Full Wrapper (Original Design)

The original architecture with a full "Pod Registry" that abstracts podman:

```
┌─────────────────────────────────────────────────────────────────┐
│  Browser (Leptos WASM)                                           │
│  ┌─────────────────────────────────────────────────────────────┐ │
│  │  Web UI                                                      │ │
│  │  • Pod list/status                                          │ │
│  │  • Code review (diff viewer)                                │ │
│  │  • Session management                                       │ │
│  └─────────────────────────────────────────────────────────────┘ │
│               │ HTTP/WebSocket                                   │
└───────────────┼─────────────────────────────────────────────────┘
                │
┌───────────────▼─────────────────────────────────────────────────┐
│  Control Plane Service (axum)                                    │
│  ┌─────────────────────────────────────────────────────────────┐ │
│  │  API Layer                                                   │ │
│  │  • REST endpoints (pods, review, sessions)                  │ │
│  │  • WebSocket (real-time updates)                            │ │
│  │  • SSR (optional, for initial HTML)                         │ │
│  └─────────────────────────────────────────────────────────────┘ │
│               │                                                   │
│  ┌─────────────────────────────────────────────────────────────┐ │
│  │  Pod Registry                                                │ │
│  │  • Watch podman pods                                        │ │
│  │  • Track git state, agent status                            │ │
│  └─────────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────┘
                │
    ┌───────────┼───────────┐
    ▼           ▼           ▼
┌───────┐  ┌───────┐  ┌───────┐
│ Pod 1 │  │ Pod 2 │  │ Pod 3 │
└───────┘  └───────┘  └───────┘
```

**When this makes sense:**

- If we need significant server-side caching/aggregation
- If we want to support non-podman backends (docker, k8s) in the future
- If security requirements demand minimal API surface

### Recommendation

**Start with Option A (thin proxy)** for MVP:

1. Faster to implement (less backend code)
2. Leverages existing podman API documentation and tooling
3. Can always add more server-side logic later if needed
4. Frontend can use the excellent [podman API docs](https://docs.podman.io/en/latest/_static/api.html) as reference

Add the thin wrapper endpoints only for:
- Git operations (structured JSON instead of parsing CLI output)
- Review state (SQLite persistence)
- Auth (if needed beyond basic HTTP auth)

## Key Components

### 1. Diff Viewer

A critical component for code review. Options:

- **monaco-diff** (via wasm-bindgen): Rich editor, but large bundle
- **Custom with syntect**: Rust syntax highlighting compiled to WASM
- **prism.js integration**: Lightweight, proven

For Leptos, we'd likely use a JavaScript diff viewer via `web-sys`/`wasm-bindgen` bindings, at least initially.

### 2. Terminal Emulation

To fully replace terminal access, we'd need a web terminal:

- **xterm.js**: Industry standard, would connect to PTY via WebSocket
- **Not initially required**: Start with structured UI, add terminal later

### 3. Real-time Updates

Leptos integrates well with WebSockets for real-time updates:

```rust
#[component]
fn PodStatus(pod_name: String) -> impl IntoView {
    let (status, set_status) = create_signal(PodState::Unknown);
    
    // WebSocket subscription for real-time updates
    create_effect(move |_| {
        spawn_local(async move {
            let mut ws = connect_pod_events(&pod_name).await;
            while let Some(event) = ws.next().await {
                set_status.set(event.status);
            }
        });
    });
    
    view! { <div class="status">{move || status.get().to_string()}</div> }
}
```

## Implementation Phases

### Phase 1: Static UI Shell

- [ ] Set up Leptos project structure
- [ ] Create basic layout (pod list, detail view)
- [ ] Wire up to existing HTTP API from controlplane
- [ ] Basic pod list with status indicators

### Phase 2: Code Review

- [ ] Integrate diff viewer component
- [ ] Fetch and display unpushed commits
- [ ] Accept/reject actions
- [ ] Comment submission

### Phase 3: Real-time + Polish

- [ ] WebSocket integration for live updates
- [ ] Notifications (agent completed, error, etc.)
- [ ] Mobile-responsive design
- [ ] Keyboard shortcuts (mirror TUI bindings)

### Phase 4: Terminal Integration (Optional)

- [ ] xterm.js integration
- [ ] PTY WebSocket proxy
- [ ] Attach to running agents from browser

## Dependencies

```toml
[dependencies]
leptos = { version = "0.7", features = ["csr", "nightly"] }
leptos_router = "0.7"
leptos_meta = "0.7"
wasm-bindgen = "0.2"
web-sys = { version = "0.3", features = ["WebSocket", "MessageEvent"] }
gloo-net = "0.6"  # HTTP client for WASM
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# For SSR (optional)
leptos_axum = "0.7"
axum = "0.8"
tokio = { version = "1", features = ["full"] }
```

## Open Questions

1. **SSR vs CSR**: Start with client-side rendering (simpler) or invest in SSR from the start? Given the auth token model, CSR is likely sufficient.

2. **Shared API types**: How to share Rust types between proxy API and web frontend? Options:
   - Shared crate (workspace member)
   - Generate types from podman's OpenAPI spec
   - Hand-write minimal types for the subset we use

3. **Bundling/deployment**: How to serve the WASM bundle?
   - Embed in devaipod binary (single binary distribution)
   - Separate static file server
   - For MVP: just serve from filesystem via axum's `ServeDir`

4. **Network access**: When binding to `0.0.0.0` for LAN access, should we require HTTPS? Options:
   - Require user to provide cert/key
   - Auto-generate self-signed (browser warnings)
   - Use mDNS + local CA (complex)
   - Accept HTTP for trusted networks (with warning)

## References

- [Leptos Book](https://book.leptos.dev)
- [Dioxus Guide](https://dioxuslabs.com/learn/0.7/)
- [Yew Docs](https://yew.rs/docs/getting-started/introduction)
- [Are We Web Yet?](https://www.arewewebyet.org/) - Rust web ecosystem overview
- [OpenCode Web](https://opencode.ai/docs/web/)
- [controlplane.md](./controlplane.md) - Related devaipod control plane design
