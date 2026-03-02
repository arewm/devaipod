# Per-Pod Gateway Sidecar (Rust)

This document captures the plan to run a **gateway/proxy container as a sidecar in each pod**, implemented in Rust, providing a single authenticated entry point for all pod services. This aligns with the existing "expose services with auth" approach and avoids `--network host` (see [Quick Start](../src/quickstart.md)).

## Motivation

Today we have:

- **Central devaipod container** (optional): runs the web UI and talks to pod-published ports via `host.containers.internal` (host gateway). Each pod publishes the **auth proxy** port (4097) to a random host port.
- **Per-pod auth**: Python `auth_proxy.py` in the agent container, listening on 4097 with Basic Auth, forwarding to opencode on 4096.
- **Per-pod service-gator**: already a sidecar in the pod (when enabled) for scoped forge/API access.

Problems / opportunities:

1. **One service, one port today**: We only expose the opencode auth proxy. Any new "thing the control plane needs from the pod" (e.g. future LLM proxy, health, metrics) would mean another published port and more host-gateway logic.
2. **Python in the loop**: The auth proxy is an injected Python script; moving logic into Rust reduces injection and keeps one stack (see [minimize-injection.md](./minimize-injection.md)).
3. **macOS / port forwarding**: Host gateway works but is a workaround. A single **gateway** per pod (one published port) that multiplexes all proxy traffic would keep the model simple and portable.

## Proposed Design: Gateway Sidecar

Add an optional **gateway** container to each pod:

- **Image**: Same devaipod image (or a slim variant); binary runs a new subcommand, e.g. `devaipod gateway`.
- **Role**: Single HTTP(S) service inside the pod that:
  - Proxies to **opencode** (localhost:4096) with auth (replacing or wrapping the current auth_proxy.py).
  - Optionally proxies **podman socket** (e.g. for web UI podman API calls scoped to this pod).
  - Can be extended later: LLM proxy (see [openai-compat-proxy.md](./openai-compat-proxy.md)), health, metrics, etc.
- **Network**: In the pod network namespace, so it can reach `localhost:4096`, `localhost:4097`, and the podman socket if we mount it into the gateway.
- **Exposure**: One published port per pod (e.g. `127.0.0.1::9292`), or a fixed port with auth. The central devaipod container (or host CLI) talks to `host.containers.internal:<published_port>` (or `127.0.0.1` when on host) with a single token or Basic Auth per pod.

Benefits:

- **Universal gateway**: One port per pod, one auth story; all future "control plane → pod" traffic goes through it.
- **Rust everywhere**: Gateway is the same binary, different subcommand; no Python auth proxy in the agent.
- **macOS-friendly**: No dependency on host network; one published port per pod works with normal port forwarding.
- **Aligns with existing per-pod patterns**: Service-gator is already per-pod; the gateway is another first-class pod member.

## Relation to Existing Code and Docs

| Piece | Relation |
|-------|----------|
| **SidecarConfig** ([config.rs](../../src/config.rs)) | Planned "sidecar" is for an **extra agent container** (e.g. goose), not the gateway. Gateway could be a separate config (e.g. `[gateway]` or a dedicated container type). |
| **auth_proxy.py** ([pod.rs](../../src/pod.rs)) | Replaced or fronted by the gateway: gateway implements Basic Auth and forwards to opencode:4096. |
| **openai-compat-proxy.md** | "Proxy container in the pod" and "per-pod proxy" align with this: the **gateway** can host or route to the LLM proxy (or we add a separate LLM container and the gateway proxies to it). |
| **rust-sidecar-monitoring.md** | That sidecar is a **host** process (nsenter into pod netns). Here we mean a **container** in the pod (simpler lifecycle, no host PID/nsenter). |
| **container deployment** | Central devaipod container would talk to each pod’s **gateway** (one port per pod) instead of directly to auth proxy port; host gateway or podman exec to gateway both remain options. |

## Implementation Sketch

1. **New subcommand**: `devaipod gateway --port 9292 --token-file /run/secrets/gateway-token`
   - Listens on `0.0.0.0:9292` (or configurable).
   - Checks token or Basic Auth on every request.
   - Routes:
     - `GET/POST /opencode/*` → `http://127.0.0.1:4096/*` (opencode API).
     - Later: `/podman/*` (if we mount socket into gateway), `/llm/*`, etc.

2. **Pod creation** ([pod.rs](../../src/pod.rs)):
   - When gateway is enabled (e.g. config or default): create a fourth container (e.g. `{pod}-gateway`), same image, cmd `devaipod gateway ...`, bind-mount podman socket if needed, publish one port `127.0.0.1::9292`.
   - Store gateway port (and token) in pod labels so the central plane can discover them.

3. **Central devaipod / web UI**:
   - Replace "get auth proxy port 4097 from container inspect" with "get gateway port from pod label (or inspect gateway container)".
   - Single code path: `host_for_pod_services():<gateway_port>` with one auth token per pod.

4. **Auth**: One token per pod (or reuse api-password). Stored in pod label and in gateway’s `/run/secrets` (or env). No need for multiple passwords (opencode vs gateway) if the gateway is the only entry point.

5. **Migration**: Keep current auth_proxy.py path working until gateway is stable; feature-flag or config to switch (e.g. `gateway.enabled = true`).

## Open Questions

- **Default on or off**: Should the gateway be enabled by default for new pods, or opt-in?
- **Slim image**: Do we want a minimal "devaipod-gateway" image (only the binary + deps for the gateway) to keep pod footprint small?
- **Podman in gateway**: If the web UI needs podman API scoped to "this pod only", we’d mount the socket into the gateway and proxy; we need to ensure podman allows that (e.g. same as current central container).
- **Lifecycle**: Gateway starts with the pod; if it crashes, podman restarts it (same as other containers). No systemd/nsenter on the host.

## Status

**Planned.** Config types and secrets already have a `Sidecar` target for a future sidecar; the gateway is a concrete use case. Implementing the `devaipod gateway` subcommand and wiring it into pod creation is the main work; then switch the web UI and host gateway logic to use the gateway port and token.
