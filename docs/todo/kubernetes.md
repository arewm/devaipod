# Kubernetes Support

For now a goal is that we will always support being run locally with podman
(and maybe docker too in the future).

However, there would be a lot of advantages to supporting Kubernetes as well.

The following design text is LLM generated.

---
Assisted-by: OpenCode (Claude Opus 4)


This document covers the strategy, implementation design, and migration plan for
Kubernetes support in devaipod. It defines three deployment models — devaipod
running in Kubernetes, spawning workspace pods in a cluster, and a hybrid local/remote
model — along with the abstraction layer needed to support multiple backends, a
codebase analysis of current Podman coupling, and phased implementation plan.

Today devaipod runs exclusively on Podman. Each workspace is a podman pod containing
four containers (workspace, agent, gator, api) — plus an optional worker in
orchestration mode — sharing a network namespace, with volumes for workspace
content and agent home directories. Kubernetes is the natural next step: it opens
the door to multi-user deployments, elastic compute, and integration with existing
cluster infrastructure that teams already operate.

> **Context**: See [opencode-webui-fork.md](./opencode-webui-fork.md) for the web
> UI architecture. The [subagent-container design](./subagent-container.md) also
> interacts with the Kubernetes story (ephemeral containers, sidecar patterns).


## Current Architecture / Podman Coupling

A touchpoint analysis of the codebase reveals three tiers of coupling. The
tightest is Podman-specific: pod lifecycle (`podman pod create/start/stop/rm/inspect`
in `src/podman.rs` and `src/pod.rs`), the web UI (hardcoded libpod API paths in
`src/web.rs`), volumes (`podman volume` CLI), secrets (`podman secret`), and label-based
service discovery (`io.devaipod.*`). These have no Docker equivalent. Medium coupling
is the Docker-API-compatible layer via bollard: image pull/build/inspect, container
start/stop/exec/logs, and socket discovery. Low coupling is everything OCI-standard:
the Containerfile, image format, and basic exec — these just work on Kubernetes.

The codebase also has **dual code paths**: `main.rs` shells out via `podman_command()`
for many operations while `pod.rs` uses `PodmanService` (bollard + podman CLI for
pod-level ops). The web server uses both. This split needs cleanup before adding a
second backend.

Each devaipod "pod" is a Podman pod with containers sharing a network namespace:
**workspace** (devcontainer image), **agent** (opencode AI), **worker** (orchestration
mode, optional), **gator** (service-gator MCP gateway, optional). Port 4097 (auth
proxy) is published for web UI proxying. Labels store API password, repo, task, mode.


## Model 1: devaipod Running Natively in Kubernetes

The simplest starting point. devaipod itself runs as a pod in a Kubernetes cluster,
but still uses Podman (or eventually the Kubernetes API) to create workspace pods.

### What already works

The project already publishes a container image to `ghcr.io` and has a Containerfile.
The pod-api sidecar exposes a health endpoint suitable for liveness and readiness
probes. Configuration is already environment-driven, so mapping to ConfigMaps and
Secrets is straightforward.

### Deployment options

Start with a **Deployment** (single replica). devaipod is effectively a control
plane daemon; horizontal scaling is not the primary use case. A StatefulSet is
overkill for a single replica. A Helm chart is useful for packaging ConfigMaps,
Secrets, and optional Ingress but can come later.

### Container runtime question

The key architectural decision is how devaipod creates workspace pods when it is
itself running inside Kubernetes:

**Podman-in-pod (stepping stone).** Mount the Podman socket from a sidecar or
DaemonSet running Podman on the node. This preserves the existing Podman backend
and avoids any code changes, but it requires privileged access or user-namespace
tricks on the node, and workspace pods are invisible to the Kubernetes control
plane. Useful for migration or hybrid setups (dev on podman, prod on k8s) but
not a substitute for a real k8s backend.

**Kubernetes API (native).** devaipod uses a ServiceAccount token to talk to the
kube API and creates workspace pods directly. This is the proper end state —
workspace pods show up in `kubectl get pods`, respect resource quotas and limits,
and get scheduled normally. The cost is implementing the `ContainerBackend` trait
(see [Abstraction Layer](#abstraction-layer-design)).

### Web server changes

The current web server has deep Podman assumptions. It proxies `/api/podman/*` to
the libpod unix socket and `/api/devaipod/pods/{name}/opencode/*` to
`host.containers.internal:<port>`. Neither works in k8s.

In Kubernetes: the socket proxy must be replaced or removed; service discovery
switches to Kubernetes Services (`pod-name-agent:4097`); external access uses
Ingress with TLS termination; `/health` maps to liveness/readiness probes.

The recommended approach: backend-agnostic REST endpoints (`GET /api/pods`
implemented by the backend trait) so the frontend doesn't need to know which
backend is active.

### RBAC

devaipod needs a ServiceAccount, Role, and RoleBinding in the target namespace.
No cluster-scoped permissions required.

```yaml
apiVersion: v1
kind: ServiceAccount
metadata:
  name: devaipod
  namespace: devaipod
---
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: devaipod
  namespace: devaipod
rules:
  - apiGroups: [""]
    resources: [pods, services, persistentvolumeclaims, secrets, configmaps]
    verbs: [get, list, watch, create, update, delete]
  - apiGroups: [""]
    resources: [pods/exec, pods/log]
    verbs: [create, get]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: devaipod
  namespace: devaipod
subjects:
  - kind: ServiceAccount
    name: devaipod
roleRef:
  kind: Role
  name: devaipod
  apiGroup: rbac.authorization.k8s.io
```

### Storage and secrets

Workspace data currently lives in podman volumes (`{pod}-workspace`,
`{pod}-agent-home`, etc.). In Kubernetes, use PersistentVolumeClaims. Each
workspace pod gets PVCs for workspace content, agent home, agent workspace (git
reference clone), and worker home/workspace (if orchestration mode). StorageClass
selection should be configurable; default to the cluster default.

LLM API keys and provider credentials go into a Kubernetes Secret mounted as
environment variables. For multi-user scenarios, authentication should sit in
front (OAuth proxy, for instance) rather than being built into devaipod.


## Model 2: Spawning Workspace Pods in a Cluster

This is the core value proposition. devaipod orchestrates workspace pods as
first-class Kubernetes objects in a target cluster. The target cluster may or may
not be the same cluster devaipod runs in.

### Pod spec mapping

The existing Podman pod structure maps naturally to a Kubernetes Pod. A podman pod
with containers sharing a network namespace is conceptually identical to a k8s Pod
with multiple containers.

| Podman | Kubernetes |
|--------|-------------|
| `podman pod create` | Create Pod manifest |
| `podman pod start` | Pod starts when created (no separate start) |
| Containers in pod | Containers in Pod spec |
| Published ports | Service (ClusterIP, NodePort, or LB) |
| Pod labels | Pod labels / annotations |
| Named volumes | PVCs |

The four containers (workspace, agent, gator, api) become entries in
`spec.containers`, and shared localhost networking works the same way. The
[subagent-container design](./subagent-container.md) fits here too — spawning an
additional container is a matter of patching the Pod spec or using ephemeral
containers.

### Creating and managing pods (kube-rs)

[kube-rs](https://kube.rs/) provides `Api<Pod>` for CRUD, watchers for streaming
updates, and an optional `ws` feature for exec/attach/port-forward.

```rust
let client = Client::try_default().await?;
let pods: Api<Pod> = Api::namespaced(client, "devaipod-workspaces");
let pod = build_devaipod_pod_manifest(&config);
pods.create(&Default::default(), &pod).await?;
```

Kubernetes is declarative: create a Pod and it runs. Delete to stop; delete and
recreate to restart.

### Kubeconfig and authentication

In-cluster, devaipod uses the ServiceAccount token automatically. Out-of-cluster,
it needs an explicit kubeconfig (mounted Secret, cloud provider auth plugin, or
local `~/.kube/config`). Accept `--kubeconfig` / `KUBECONFIG` and fall back to
in-cluster detection; kube-rs handles this with `Client::try_default().await`.

### Service discovery

In Kubernetes, the same `io.devaipod.*` label keys go into Pod labels and
annotations. Query via `Api::list()` with label selectors. Each workspace pod
gets a Service (`{pod-name}-agent`, target port 4097); devaipod connects to
`{pod-name}-agent.devaipod-workspaces.svc.cluster.local:4097`. The API password
comes from a Pod annotation.

### Networking

| Approach | Pros | Cons |
|----------|------|------|
| **ClusterIP Service** | Simple, internal only | devaipod must run in-cluster |
| **NodePort** | Accessible from outside | Port range limits, not ideal for many pods |
| **LoadBalancer** | External access | One LB per pod is expensive |
| **Port-forward** | No Service needed | Ephemeral, not for production |

Recommendation: ClusterIP Service per workspace pod. devaipod runs in-cluster and
connects via Service DNS. For external browser access, use an Ingress that routes
by host/path to the appropriate Service, or a single proxy using the pod name in
the path (current pattern).

### Storage (PVCs)

Podman volumes become PVCs, created before the Pod. Naming: `{pod-name}-workspace`,
`{pod-name}-agent-home`, etc.

```yaml
volumes:
  - name: workspace
    persistentVolumeClaim:
      claimName: devaipod-myproject-abc123-workspace
```

`ReadWriteOnce` suffices for single-node clusters; multi-node may need
`ReadWriteMany` (NFS, CephFS). For ephemeral workspaces, `emptyDir` avoids PVC
lifecycle management.

### Secrets

Podman secrets (`podman secret create`, `--secret type=env`) become Kubernetes
Secrets:

```yaml
env:
  - name: GH_TOKEN
    valueFrom:
      secretKeyRef:
        name: devaipod-gh-token
        key: token
```

The backend creates Secrets from config, or references existing Secrets.

### Exec

Interactive terminal access uses the Kubernetes exec subresource
(`/api/v1/namespaces/{ns}/pods/{pod}/exec`). kube-rs with the `ws` feature provides
`Api<Pod>::exec()` which handles the WebSocket upgrade. This replaces bollard's
`create_exec`/`start_exec` and is how the agent container runs commands in the
workspace.

### Resource limits, security, and image pull

Workspace pods should include configurable resource requests/limits (default: 2 CPU
/ 4Gi memory) with per-pod overrides. The current Podman containers sometimes need
`--privileged` for nested builds; in k8s this requires the `privileged` Pod Security
Standard. Devaipod should also support unprivileged mode (Buildah rootless, Kaniko).

Private registries need `imagePullSecrets` on workspace pod specs. Configure a
default pull secret per namespace and propagate it to all spawned pods.


## Model 3: Hybrid — Local devaipod, Remote Kubernetes

The most interesting model for individual developers. devaipod runs locally (in
Podman Desktop, in a terminal, wherever) but spawns workspace pods on a remote
Kubernetes cluster. The developer keeps their local tooling and workflow but
offloads heavy compute — LLM agent loops, big builds — to cluster resources.

### Credential flow

The kubeconfig comes from the local filesystem, typically via cloud provider CLI.
LLM API keys exist locally but need to reach the remote agent container — either
as a Kubernetes Secret (better for rotation, avoids keys in pod specs) or injected
as env vars in the pod spec at creation time. The Secret approach requires write
access to Secrets in the target namespace.

### Network connectivity

The hardest part of Model 3. ClusterIP services are not routable from outside.
Options: port-forward (most portable, but fragile — needs reconnection logic),
NodePort/LB per workspace (wasteful), or VPN/tunnel (Tailscale, WireGuard,
Telepresence — cleanest but requires external setup).

The pragmatic first step is port-forward with automatic reconnection. It works
everywhere and needs no cluster-side changes.

### Storage divergence

The named volumes (workspace content, agent home, agent workspace, etc.) map
cleanly to PVCs in any model — that part is straightforward. The problem in
Model 3 is the host-path bind mounts and host-dependent file injection that
devaipod currently relies on:

**Container runtime socket.** The pod-api sidecar bind-mounts the host's
podman/docker socket (`/run/docker.sock`) to exec into workspace and agent
containers for PTY sessions. In remote k8s, there is no local socket to mount.
The k8s backend must use the exec subresource instead (already covered in the
`ContainerBackend` trait), so this mount simply doesn't apply — but the api
sidecar needs to be taught to use k8s exec rather than the socket.

**Local `.git` directory.** For `LocalRepo` workspace sources, an init container
bind-mounts the project's `.git` directory (read-only at `/mnt/host-git`) to
bootstrap the workspace clone via `git clone --reference`. With a remote cluster,
the host filesystem isn't available. The workspace pod must clone the git repo
at startup from the remote (init container or entrypoint). devaipod tells the
remote pod which repo URL and branch to clone; syncing changes back means
pushing to a branch — there's no shared filesystem.

**`bind_home` file injection.** devaipod copies host files (`.ssh/config`,
`.gitconfig`, etc.) into containers via `podman cp` based on the `[bind_home]`
config. This is already rejected when devaipod itself runs in a container
(users are told to use podman secrets instead). For Model 3, the same applies:
these files need to go into k8s Secrets or ConfigMaps and be mounted into the
pod spec.

**Script injection.** Clone scripts and `opencode-connect` are written into
containers at startup (see [minimize-injection.md](./minimize-injection.md)).
These don't depend on host bind mounts — they're embedded in the devaipod
binary and written via init containers or `tee` — so they work fine in remote
pods as long as the devaipod image is available.

### Web UI proxy

The port-forward approach makes this transparent: the browser hits `localhost:PORT`
which tunnels to the remote api container, same as the current Podman model.


## Abstraction Layer Design

### ContainerBackend Trait

The core design enabling multiple backends. The trait abstracts over Podman and
Kubernetes so that the rest of the codebase doesn't need to know which backend
is active.

```rust
#[async_trait]
pub trait ContainerBackend: Send + Sync {
    // Pod lifecycle
    async fn create_pod(&self, name: &str, labels: &[(String, String)], publish_ports: &[String]) -> Result<String>;
    async fn start_pod(&self, name: &str) -> Result<()>;
    async fn stop_pod(&self, name: &str) -> Result<()>;
    async fn remove_pod(&self, name: &str, force: bool) -> Result<()>;
    async fn list_pods(&self, label_filter: Option<&str>) -> Result<Vec<PodInfo>>;

    // Volumes / PVCs
    async fn create_volume(&self, name: &str) -> Result<()>;
    async fn remove_volume(&self, name: &str, force: bool) -> Result<()>;

    // Containers
    async fn create_container(&self, name: &str, image: &str, pod_name: &str, config: ContainerConfig) -> Result<String>;
    async fn start_container(&self, name: &str) -> Result<()>;
    async fn stop_container(&self, name: &str, timeout_secs: i64) -> Result<()>;

    // Exec & logs
    async fn exec(&self, container: &str, cmd: &[&str], user: Option<&str>, workdir: Option<&str>) -> Result<i64>;
    async fn exec_output(&self, container: &str, cmd: &[&str]) -> Result<(i64, Vec<u8>, Vec<u8>)>;

    // Image operations
    async fn pull_image(&self, image: &str) -> Result<()>;
    async fn build_image(&self, tag: &str, context_path: &Path, dockerfile: &str, ...) -> Result<()>;

    // Service discovery (for web proxy)
    async fn get_pod_port(&self, pod_name: &str, container_port: u16) -> Result<u16>;
    async fn get_pod_auth_password(&self, pod_name: &str) -> Result<String>;
}
```

### Handling Differences

| Aspect | Podman | Kubernetes |
|--------|--------|------------|
| **Pod lifecycle** | create → start (imperative) | create = run (declarative) |
| **Start/stop** | Explicit start/stop | Delete pod to stop; recreate to start |
| **Port publishing** | Random host port at create | Service with ClusterIP; no host port |
| **Volumes** | Named volumes, local | PVCs, cluster storage |
| **Secrets** | Podman secrets | K8s Secrets |
| **Image build** | Build locally | Build in CI, push to registry |
| **Exec** | Docker API (bollard) | K8s exec subresource |

Key adaptation: `start_pod` is a no-op in k8s (Pod runs on create); `stop_pod`
deletes the Pod; `get_pod_port` returns the Service port; `build_image` pushes to
a registry or requires pre-built images.

Bollard remains the primary client for the Podman backend. The k8s backend does
not use bollard — except in the podman-in-pod stepping stone, where bollard talks
to a podman socket inside a privileged sidecar.


## Implementation Plan

The migration is phased. Phase 1 is a prerequisite refactor; Phases 2-4 deliver
Models 1 and 2 progressively. Model 3 builds on Phase 2.

**Phase 1: Extract Backend Trait** (2–3 weeks, low risk). Define `ContainerBackend`
trait in `src/backend/mod.rs`. Implement `PodmanBackend` wrapping current logic.
Replace all direct `PodmanService`/`podman_command()` usage. Update web server to
use backend for `get_pod_opencode_info`. Run integration tests; no behavior change.

**Phase 2: Kubernetes Backend** (4–6 weeks, medium risk). `KubernetesBackend` using
kube-rs: pod creation, PVC creation, Secret handling, Service creation per pod,
exec via `ws` feature, label/annotation service discovery. Assume images exist in
registry; optionally support Kaniko/Buildah for on-cluster build. Configurable
namespace (e.g., `devaipod-workspaces`).

**Phase 3: Runtime Selection** (1–2 weeks, low risk). Config-driven:
`[backend] type = "podman" | "kubernetes"`, env override `DEVAIPOD_BACKEND`,
kubeconfig via `DEVAIPOD_KUBECONFIG` or in-cluster detection. Web server uses
backend for all operations; conditionalize podman proxy.

**Phase 4: Deploy in Kubernetes** (1–2 weeks, low risk). Helm chart or manifests:
Deployment, Service, ConfigMap, Secret templates, optional Ingress. Test: devaipod
in cluster creates workspace pods in same cluster.

### Incremental Deliverables

| Milestone | Deliverable | Model |
|-----------|-------------|-------|
| Phase 1 | Podman backend behind trait; all tests pass | Prerequisite |
| Phase 2a | k8s backend creates pods (no exec, minimal) | Model 2 (partial) |
| Phase 2b | k8s exec, logs, service discovery | Model 2 (partial) |
| Phase 2c | k8s volumes (PVCs), secrets | Model 2 (complete) |
| Phase 3 | Config-driven backend selection | Models 1+2 |
| Phase 4 | Helm chart for devaipod | Model 1 |
| Future | Port-forward tunneling, git-clone storage | Model 3 |


## Technical Decisions

**kube-rs vs kubectl:** Use **kube-rs** — native Rust, type-safe, exec/port-forward
via `ws` feature. kubectl only as debugging fallback.

**Multi-tenancy:** Start with a single namespace (`devaipod-workspaces`).
Namespace-per-tenant adds RBAC complexity; defer until needed.

**Image registry:** Workspace images must be pullable by the cluster. Private
registries use `imagePullSecrets`. For in-cluster builds: require pre-built images
or run Kaniko/Buildah jobs.

**Dev vs production:** Backend is environment-agnostic. Config (namespace, storage
class, resource limits) varies. Dev (minikube, kind, k3d) uses local registry and
small storage; production needs HA, network policies, and image pull secrets.


## Risks and Estimates

**Total effort: 8–13 weeks.** Phase 1 (trait refactor) 2–3 weeks low risk;
Phase 2 (k8s backend) 4–6 weeks medium risk; Phase 3 (runtime selection) 1–2
weeks low risk; Phase 4 (deploy in k8s) 1–2 weeks low risk.

The things that break are the web UI podman proxy (must be replaced or
conditionalized), `podman_command()` calls in `main.rs` (must go through backend),
`get_pod_opencode_info` (backend-specific port discovery and password retrieval),
image build (needs registry workflow in k8s), and `host_for_pod_services()` (k8s
uses Service DNS instead of host gateway). The things that are easy: pod structure
maps 1:1, labels use the same keys, container config (env, mounts) maps directly,
and OCI images work in both backends.

### Dependencies

- **Rust:** kube = "0.92", k8s-openapi = { version = "0.24", features = ["v1_28"] }
- **Kubernetes:** 1.28+
- **Cluster:** Default storage class, optional Ingress controller
- **Registry:** Accessible from cluster (public or imagePullSecrets)


## Comparison

| Aspect | Model 1: devaipod in k8s | Model 2: Spawning pods | Model 3: Hybrid |
|---|---|---|---|
| devaipod runs | In the cluster | In the cluster | Locally |
| Workspace pods run | Same cluster (podman or k8s) | Target cluster | Remote cluster |
| Networking | Trivial (in-cluster) | ClusterIP Services | Port-forward or tunnel |
| Storage | PVCs or local volumes | PVCs | Git clone (no shared FS) |
| Auth | ServiceAccount | ServiceAccount or kubeconfig | Local kubeconfig |
| Complexity | Low | Medium | High |
| Primary audience | Platform teams, multi-user | Platform teams, CI/CD | Individual developers |


## Open Questions

- Should devaipod manage its own namespace or expect an admin to pre-create one?
  Namespace-per-user is clean for isolation but adds RBAC complexity.

- For Model 3, is `kubectl port-forward` reliable enough for production use, or
  do we need to invest in a proper tunnel solution from the start?

- How does workspace garbage collection work? Podman pods are easy to clean up
  locally. Kubernetes pods left running burn cluster resources. We likely need a
  TTL or idle-timeout mechanism with a finalizer.

- Should we support CRDs (a `Workspace` custom resource) or keep everything as
  plain Pods and Services? CRDs give better tooling integration (`kubectl get
  workspaces`) but are heavier to maintain.

- For Model 2 with a different target cluster, how do we handle image registry
  authentication? The target cluster may not have access to the same registries
  as devaipod's cluster.

- How does the [subagent-container](./subagent-container.md) model interact with
  Kubernetes pod immutability? Ephemeral containers are GA since k8s 1.25 but
  have inherent limitations (no resource limits, no probes) by design.
  Alternatively, we could pre-declare subagent containers in the pod spec as
  sidecars that start paused.


## References

- [kube-rs](https://kube.rs/) — Rust Kubernetes client
- [kube-rs features](https://kube.rs/features) — ws for exec, attach, port-forward
- [Podman REST API](https://docs.podman.io/en/latest/_static/api.html)
- [opencode-webui-fork.md](./opencode-webui-fork.md) — Web UI architecture
- [subagent-container.md](./subagent-container.md) — Subagent container design
- [Quick Start](../src/quickstart.md) — Container deployment and setup
