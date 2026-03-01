# Design Philosophy

## Mid-Level Infrastructure

devaipod is designed as **mid-level infrastructure** for AI coding workflows.

**More opinionated than raw tools**: Unlike running opencode or Claude Code directly, devaipod provides structure around sandboxing, credential isolation, and workspace lifecycle. You don't have to figure out container security yourself.

**Less opinionated than full platforms**: Unlike monolithic solutions (OpenHands Cloud, Cursor), devaipod focuses on the primitives and leaves room for building different workflows on top. Want a web UI? Build one that talks to our pods. Prefer a TUI? That works too.

**Composable building blocks**: The pod abstraction and service-gator MCP are independent pieces. Use what you need, skip what you don't.

This design enables:

- Custom control planes (web UI, TUI, or API-driven)
- Integration with existing CI/CD and review workflows
- Different human-in-the-loop patterns for different teams
- Extension via MCP servers and external tooling

## Security First

The fundamental design principle is that AI agents should have minimal access to credentials and external services. Rather than trusting the agent with your GitHub token, devaipod:

1. Runs the agent in an isolated container without trusted credentials (GH_TOKEN, etc.)
2. Routes external service access through [service-gator](service-gator.md), which enforces fine-grained scopes
3. By default, only allows the agent to read repositories and create *draft* pull requests

This means a prompt injection attack or misbehaving agent cannot:

- Push directly to your repositories
- Access other repositories you have access to
- Merge pull requests
- Create non-draft PRs (which could trigger CI in surprising ways)

## Human-in-the-Loop

devaipod is built for workflows where humans review AI-generated code before it becomes permanent. The default permissions (read + draft PR) reflect this: the agent can propose changes, but a human must mark them ready for merge.

This isn't about distrusting AI capabilities—it's about maintaining auditability and preventing automation failures from having outsized impact.

## Web UI Architecture

The web UI is a vendored build of the [opencode](https://opencode.ai) SPA,
built from source in the Containerfile with `VITE_DEVAIPOD=true`. It is
served by a **pod-api sidecar** container that runs alongside each agent pod.

The control plane handles pod lifecycle (create, start, stop, rebuild),
authentication (cookie-based login), discovering each pod-api sidecar's
published port, and serving the iframe wrapper with navigation. The `/pods`
management page is an SPA route outside the opencode SDK provider stack.

Each pod's sidecar handles everything else: serving the SPA, proxying
opencode API calls to `localhost:4096` within the pod's network namespace,
and providing git and PTY endpoints directly.

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
published — the sidecar proxies to it internally. Since each pod runs on
its own origin (different host port), localStorage is naturally isolated
per pod.

### Why we vendor the opencode UI

`opencode serve` does not serve its own web UI — non-API requests are
proxied to `https://app.opencode.ai`. This is unsuitable for devaipod
because cross-origin iframes are blocked by `X-Frame-Options`/CSP headers,
the hosted UI would make API calls back to `app.opencode.ai` instead of the
local backend, and air-gapped environments can't reach external services.

Vendoring the built SPA eliminates all three problems. The opencode SPA
detects it's not on `opencode.ai` and uses `window.location.origin` for API
calls, which on the pod-api sidecar routes to the correct opencode server
automatically.
