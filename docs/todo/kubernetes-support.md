# Kubernetes Support Investigation

This document analyzes what it would take to add Kubernetes support to devaipod — both running devaipod itself in Kubernetes and scheduling agent pods in Kubernetes clusters.

> **Context**: devaipod currently uses Podman exclusively. See [controlplane.md](./controlplane.md) for the control plane design and [webui.md](./webui.md) for the web UI architecture. The roadmap mentions Kubernetes support as a future idea.

## Current Architecture Summary

### Podman Touchpoints (from codebase analysis)

**High coupling (Podman-specific, no Docker equivalent):**

| Component | Location | Usage |
|-----------|----------|-------|
| **Pods** | `src/podman.rs`, `src/pod.rs` | `podman pod create/start/stop/rm/inspect` — core abstraction. Docker has no pods; Kubernetes does. |
| **Web UI** | `src/web.rs` | Hardcoded libpod API paths (`/podman/v5.0.0/libpod/pods/json`, `/pods/{name}/start`, etc.) proxied to unix socket |
| **Volumes** | `src/podman.rs` | `podman volume create/exists/rm` CLI |
| **Secrets** | `src/podman.rs`, `src/pod.rs` | `podman secret create`, `--secret` flag on containers |
| **Labels** | `src/pod.rs`, `src/main.rs`, `src/web.rs` | `io.devaipod.api-password`, `io.devaipod.repo`, `io.devaipod.task`, etc. for service discovery |

**Medium coupling (Docker-API compatible via bollard):**

| Component | Location | Usage |
|-----------|----------|-------|
| **Image ops** | `src/podman.rs` | Pull, build, inspect via bollard |
| **Container ops** | `src/podman.rs` | Start, stop, remove, exec, logs via bollard |
| **Socket** | `src/podman.rs` | Podman socket with Docker fallback (`/var/run/docker.sock`) |

**Low coupling:**

- Containerfile is OCI-standard
- Exec uses Docker API (bollard)
- Image operations use Docker API

**Dual code paths:** `main.rs` uses `podman_command()` (shell out) for many operations (list, start, stop, delete, inspect, exec) while `pod.rs` uses `PodmanService` (bollard + podman CLI for pods). The web server uses both: bollard for container inspect, `podman pod inspect` for labels.

### Pod Structure

Each devaipod "pod" is a Podman pod containing multiple containers sharing a network namespace:

- **workspace** — user's dev environment (devcontainer image)
- **agent** — opencode AI agent
- **worker** — worker agent (orchestration mode)
- **gator** — service-gator MCP gateway (optional)

Port 4097 (auth proxy) is published on the pod for web UI proxying. Labels store API password, repo, task, mode.

---

## 1. Running devaipod in Kubernetes

### 1.1 Deployment Options

| Option | Pros | Cons |
|--------|------|------|
| **Deployment** | Simple, stateless, horizontal scaling | No persistent identity; state in PVC |
| **StatefulSet** | Stable network identity, ordered rollout | Overkill if single replica |
| **Helm chart** | Standard packaging, configurable | Extra tooling, versioning |

**Recommendation:** Start with a **Deployment** (single replica). devaipod is effectively a control plane daemon; horizontal scaling is not a primary use case. A Helm chart is useful for packaging ConfigMaps, Secrets, and optional Ingress.

### 1.2 Web Server Changes

**Current behavior:** devaipod binds to port 8080, serves static files, and proxies:

- `/api/podman/*` → unix socket (libpod REST API)
- `/api/devaipod/pods/{name}/opencode/*` → `host.containers.internal:<port>` (auth proxy on each pod)

**In Kubernetes:**

1. **No Podman socket** — The socket proxy at `/api/podman/*` cannot work. It must be replaced with a Kubernetes API proxy or removed when using the k8s backend.

2. **Service discovery** — Instead of `host.containers.internal:<random_port>`, devaipod would use Kubernetes Services. Each agent pod gets a Service; devaipod proxies to `pod-name-agent:4097` (or similar) within the cluster.

3. **Ingress** — For external access, add an Ingress (or LoadBalancer/NodePort) pointing to the devaipod Service. TLS termination at Ingress is standard.

4. **Health checks** — `/health` already exists; add Kubernetes liveness/readiness probes.

### 1.3 Podman Socket Proxy in K8s

**The socket proxy does not apply** when devaipod uses Kubernetes as the backend. Two deployment modes:

| Mode | Backend | Web UI pod list | Notes |
|------|---------|-----------------|-------|
| **Podman** | Host podman socket | Proxy to libpod API | Current; requires socket mount |
| **Kubernetes** | kube-api | Custom API or kube proxy | No socket; use kube client |

When backend is Kubernetes, the web UI must not call `/api/podman/*`. Options:

- **A)** Replace with `/api/kubernetes/*` that proxies to the Kubernetes API (similar pattern, different upstream)
- **B)** Implement devaipod-native REST endpoints that abstract over the backend (e.g., `GET /api/pods` implemented by backend trait)
- **C)** Serve a different frontend that uses k8s-specific API shapes

**Recommendation:** Option B — abstract API. The backend trait (Section 3) would have `list_pods()`, etc.; the web server calls the backend, not the socket. This keeps the frontend backend-agnostic.

### 1.4 Storage (PVCs)

**Current:** Workspace data in podman volumes (`{pod}-workspace`, `{pod}-agent-home`, etc.).

**In Kubernetes:** Use PersistentVolumeClaims. Each workspace pod gets PVCs for:

- Workspace content (clone target)
- Agent home (credentials, config)
- Agent workspace (git reference clone)
- Worker home/workspace (if orchestration mode)

StorageClass selection (e.g., `standard`, `fast`) should be configurable. Default to cluster default.

### 1.5 Secrets

**Current:** Podman secrets (`podman secret create`), mounted via `--secret type=env`.

**In Kubernetes:** Use Kubernetes Secrets. Create Secrets for GH_TOKEN, ANTHROPIC_API_KEY, etc. Mount as env vars or files in the devaipod Deployment and in workspace pods.

---

## 2. Scheduling Agent Pods in Kubernetes

### 2.1 Podman Pods → Kubernetes Pods

**Natural fit:** A Podman pod (containers sharing network) maps directly to a Kubernetes Pod (containers in same pod share network namespace).

| Podman | Kubernetes |
|--------|-------------|
| `podman pod create` | Create Pod manifest |
| `podman pod start` | Pod starts when created (no separate start) |
| Containers in pod | Containers in Pod spec |
| Published ports | Service with NodePort or LoadBalancer, or port-forward |
| Pod labels | Pod labels / annotations |
| Named volumes | PVCs |

### 2.2 Creating/Managing Pods (kube-rs)

The [kube-rs](https://kube.rs/) crate provides:

- `Api<Pod>` for CRUD operations
- `Api::create()`, `Api::delete()`, `Api::get()`, `Api::list()`
- Watchers for streaming updates
- **Optional `ws` feature:** exec, attach, port-forward via WebSockets

**Example (conceptual):**

```rust
use kube::{Api, Client};
use k8s_openapi::api::core::v1::Pod;

let client = Client::try_default().await?;
let pods: Api<Pod> = Api::namespaced(client, "devaipod-workspaces");

let pod = build_devaipod_pod_manifest(&config);
pods.create(&Default::default(), &pod).await?;
```

**Declarative vs imperative:** Kubernetes is declarative. You create a Pod; the kubelet reconciles. No "start" — the Pod is created and it runs. For "stop", delete the Pod (or scale down). For "restart", delete and recreate.

### 2.3 Service Discovery

**Current:** `podman pod inspect` → labels `io.devaipod.api-password`, `io.devaipod.repo`, etc.

**Kubernetes:** Use Pod labels and annotations. Same keys: `io.devaipod.api-password`, `io.devaipod.repo`. Query via `Api::list()` with label selectors, or `Api::get()` and read `pod.metadata.labels`.

For the opencode proxy, devaipod needs:
- **Port:** In k8s, use a Service per pod. The Service name could be `{pod-name}-agent`; port 4097 is the target port. devaipod connects to `{pod-name}-agent.devaipod-workspaces.svc.cluster.local:4097`.
- **Password:** From Pod annotation `io.devaipod.api-password` (or label; annotations support larger values).

### 2.4 Networking

**Current:** Port 4097 published to random host port; devaipod connects to `host.containers.internal:<port>`.

**Kubernetes options:**

| Approach | Pros | Cons |
|----------|------|------|
| **ClusterIP Service** | Simple, internal only | devaipod must run in-cluster to reach it |
| **NodePort** | Accessible from outside | Port range limits, not ideal for many pods |
| **LoadBalancer** | External access | One LB per pod is expensive |
| **Port-forward** | No Service needed | Ephemeral, not for production |

**Recommendation:** ClusterIP Service per workspace pod. devaipod runs in-cluster and connects via Service DNS. For external access to opencode (e.g., browser), use an Ingress that routes by host/path to the appropriate Service, or a single proxy that uses the pod name in the path (current pattern).

### 2.5 Volumes → PVCs

**Current:** `podman volume create`, mount with `-v volume:path`.

**Kubernetes:** Create PVCs, reference in Pod spec:

```yaml
volumes:
  - name: workspace
    persistentVolumeClaim:
      claimName: devaipod-myproject-abc123-workspace
```

The backend would create PVCs before creating the Pod. Naming: `{pod-name}-workspace`, `{pod-name}-agent-home`, etc.

### 2.6 Secrets → Kubernetes Secrets

**Current:** `podman secret create`, `--secret type=env,target=GH_TOKEN`.

**Kubernetes:** Create Secret, mount as env:

```yaml
env:
  - name: GH_TOKEN
    valueFrom:
      secretKeyRef:
        name: devaipod-gh-token
        key: token
```

The backend creates Secrets from config, or references existing Secrets.

### 2.7 Exec → Kubernetes Exec API

**Current:** bollard `create_exec` / `start_exec` (Docker API).

**Kubernetes:** Use the exec subresource. kube-rs with `ws` feature provides `Api::exec()` for streaming exec. Alternatively, shell out to `kubectl exec`.

---

## 3. Abstraction Layer Design

### 3.1 ContainerBackend Trait

Proposed trait to abstract over Podman and Kubernetes:

```rust
#[async_trait]
pub trait ContainerBackend: Send + Sync {
    // Pod lifecycle
    async fn create_pod(&self, name: &str, labels: &[(String, String)], publish_ports: &[String]) -> Result<String>;
    async fn start_pod(&self, name: &str) -> Result<()>;
    async fn stop_pod(&self, name: &str) -> Result<()>;
    async fn remove_pod(&self, name: &str, force: bool) -> Result<()>;
    async fn get_pod_labels(&self, name: &str) -> Result<HashMap<String, String>>;
    async fn list_pods(&self, label_filter: Option<&str>) -> Result<Vec<PodInfo>>;

    // Volumes / PVCs
    async fn create_volume(&self, name: &str) -> Result<()>;
    async fn volume_exists(&self, name: &str) -> Result<bool>;
    async fn remove_volume(&self, name: &str, force: bool) -> Result<()>;

    // Containers
    async fn create_container(&self, name: &str, image: &str, pod_name: &str, config: ContainerConfig) -> Result<String>;
    async fn start_container(&self, name: &str) -> Result<()>;
    async fn stop_container(&self, name: &str, timeout_secs: i64) -> Result<()>;
    async fn remove_container(&self, name: &str, force: bool) -> Result<()>;

    // Exec & logs
    async fn exec(&self, container: &str, cmd: &[&str], user: Option<&str>, workdir: Option<&str>) -> Result<i64>;
    async fn exec_output(&self, container: &str, cmd: &[&str]) -> Result<(i64, Vec<u8>, Vec<u8>)>;
    async fn logs(&self, container: &str, follow: bool) -> Result<()>;

    // Image operations (may differ: k8s typically assumes pre-pulled images)
    async fn pull_image(&self, image: &str) -> Result<()>;
    async fn build_image(&self, tag: &str, context_path: &Path, dockerfile: &str, ...) -> Result<()>;
    async fn ensure_image(&self, source: &ImageSource, tag: &str, ...) -> Result<String>;

    // Service discovery (for web proxy)
    async fn get_pod_port(&self, pod_name: &str, container_port: u16) -> Result<u16>;
    async fn get_pod_auth_password(&self, pod_name: &str) -> Result<String>;
}
```

### 3.2 Handling Differences

| Aspect | Podman | Kubernetes |
|--------|--------|------------|
| **Pod lifecycle** | create → start (imperative) | create = run (declarative) |
| **Start/stop** | Explicit start/stop | Delete pod to stop; recreate to start |
| **Port publishing** | Random host port at create | Service with ClusterIP; no host port by default |
| **Volumes** | Named volumes, local | PVCs, cluster storage |
| **Secrets** | Podman secrets | K8s Secrets |
| **Image build** | Build locally | Typically build in CI, push to registry |
| **Exec** | Docker API | K8s exec subresource |

**Adaptation strategies:**

- **start_pod:** No-op for k8s (Pod runs on create). Or: ensure Pod exists and is Running.
- **stop_pod:** Delete Pod (and optionally PVCs).
- **get_pod_port:** For k8s, return Service port (e.g., 4097) — connection is via Service DNS, not host port.
- **get_pod_auth_password:** Read from Pod annotation/label.
- **Image build:** In k8s, `build_image` may push to a registry and return the image ref. Or require pre-built images.

### 3.3 Where Bollard Stays Useful

- **Podman backend:** Bollard remains the primary client for container operations.
- **Kubernetes backend:** Bollard not used for pod/container lifecycle. Only if we support **Podman-in-Pod** (Section 5.5) would bollard be used inside a privileged pod running podman.

---

## 4. Migration Strategy

### Phase 1: Extract Backend Trait (Refactor Only)

**Goal:** Introduce `ContainerBackend` trait and implement it for Podman. No new features.

**Tasks:**
1. Define `ContainerBackend` trait in `src/backend/mod.rs`.
2. Implement `PodmanBackend` wrapping current `PodmanService` logic.
3. Replace direct `PodmanService` and `podman_command()` usage with backend calls.
4. Consolidate `main.rs` podman CLI calls into backend (e.g., `list_pods`, `get_pod_labels`, `cmd_start`, `cmd_stop`, `cmd_delete`).
5. Update web server to use backend for `get_pod_opencode_info` (instead of bollard + podman inspect).
6. Run integration tests; ensure no behavior change.

**Effort:** 2–3 weeks. **Risk:** Low.

### Phase 2: Implement Kubernetes Backend

**Goal:** `KubernetesBackend` that implements `ContainerBackend` using kube-rs.

**Tasks:**
1. Add `kube` and `k8s-openapi` dependencies.
2. Implement `KubernetesBackend`:
   - Pod creation from `DevaipodPod` config
   - PVC creation for volumes
   - Secret creation/reference for credentials
   - Service creation per pod for port 4097
   - Exec via kube-rs `ws` or `kubectl exec`
   - Label/annotation handling for service discovery
3. Image handling: assume images exist in registry; document build/push workflow. Optional: Kaniko/Buildah job for on-cluster build.
4. Namespace: use configurable namespace (e.g., `devaipod-workspaces`).

**Effort:** 4–6 weeks. **Risk:** Medium (networking, RBAC, storage).

### Phase 3: Runtime Selection

**Goal:** Choose backend at runtime via config or env.

**Tasks:**
1. Config: `[backend] type = "podman" | "kubernetes"`.
2. Env override: `DEVAIPOD_BACKEND=kubernetes`.
3. Kube config: `DEVAIPOD_KUBECONFIG` or in-cluster config.
4. Web server: use backend for all operations; remove or conditionalize podman proxy.
5. CLI: ensure all commands work with both backends.

**Effort:** 1–2 weeks. **Risk:** Low.

### Phase 4: Deploy devaipod in Kubernetes

**Goal:** Helm chart or manifests to run devaipod in K8s.

**Tasks:**
1. Create Deployment, Service, ConfigMap, Secret templates.
2. Optional: Ingress for web UI.
3. Document image registry, RBAC, storage requirements.
4. Test: devaipod in cluster creates workspace pods in same cluster.

**Effort:** 1–2 weeks. **Risk:** Low.

### Incremental Deliverables

| Milestone | Deliverable |
|-----------|-------------|
| Phase 1 | Podman backend behind trait; all tests pass |
| Phase 2a | k8s backend creates pods (no exec, minimal) |
| Phase 2b | k8s exec, logs, service discovery |
| Phase 2c | k8s volumes (PVCs), secrets |
| Phase 3 | Config-driven backend selection |
| Phase 4 | Helm chart for devaipod |

---

## 5. Key Technical Decisions and Open Questions

### 5.1 kube-rs vs kubectl

| Approach | Pros | Cons |
|----------|------|------|
| **kube-rs** | Native Rust, no subprocess, type-safe, good for controllers | Learning curve, API surface |
| **kubectl** | Simple, well-known | Subprocess overhead, parsing output, version skew |

**Recommendation:** **kube-rs**. Better integration, no parsing, and exec/port-forward via `ws` feature. kubectl as fallback for one-off debugging only.

### 5.2 RBAC Requirements

devaipod needs permissions in the target namespace:

| Resource | Verbs |
|----------|-------|
| Pods | create, get, list, watch, delete, patch |
| Services | create, get, list, delete |
| PersistentVolumeClaims | create, get, list, delete |
| Secrets | create, get, list, delete |
| ConfigMaps | get, list (optional) |
| Pods/exec | create (for exec) |
| Pods/log | get (for logs) |

Cluster-scoped: none if using namespaced resources. A Role + RoleBinding in the devaipod namespace (or a dedicated workspaces namespace) is sufficient.

### 5.3 Multi-Tenancy

- **Single namespace:** All workspace pods in one namespace (e.g., `devaipod-workspaces`). Simple.
- **Namespace per tenant:** One namespace per user/team. devaipod would need to create namespaces and manage RBAC. More complex, better isolation.

**Recommendation:** Start with single namespace. Add multi-tenancy later if needed.

### 5.4 Image Registry

- Workspace images must be pullable by the cluster (e.g., ghcr.io, quay.io).
- Private registries: use imagePullSecrets (K8s Secret of type dockerconfigjson).
- Building: devaipod today builds via bollard. In k8s, options:
  - Require pre-built images (push from CI).
  - Run Kaniko/Buildah job in cluster (adds complexity).
  - Use a registry with webhook-based builds (e.g., GitHub Container Registry on push).

### 5.5 Podman-in-Kubernetes as Stepping Stone?

**Idea:** Run a privileged pod with podman installed and the host's (or a VM's) podman socket. devaipod uses the existing Podman backend; no k8s backend needed.

**Pros:** Minimal code changes; reuse everything. **Cons:** Requires privileged pod, socket mounting, and a node with podman. Defeats the purpose of "native" k8s scheduling.

**Verdict:** Useful for migration or hybrid setups (e.g., dev on podman, prod on k8s). Not a substitute for a real k8s backend.

### 5.6 Dev vs Production

| Environment | Considerations |
|-------------|----------------|
| **Dev (minikube, kind, k3d)** | Local registry, small storage, single node |
| **Production** | HA, network policies, resource limits, image pull secrets |

The backend should be environment-agnostic. Config (namespace, storage class, resource limits) can vary.

---

## 6. Risks and Complexity Estimate

### Effort Summary

| Phase | Effort | Risk |
|-------|--------|------|
| Phase 1: Backend trait | 2–3 weeks | Low |
| Phase 2: K8s backend | 4–6 weeks | Medium |
| Phase 3: Runtime selection | 1–2 weeks | Low |
| Phase 4: Deploy in K8s | 1–2 weeks | Low |
| **Total** | **8–13 weeks** | |

### What Breaks

| Area | Impact |
|------|--------|
| Web UI podman proxy | Must be replaced or conditionalized |
| `podman_command()` in main.rs | Must go through backend |
| `get_pod_opencode_info` | Backend-specific (port discovery, password) |
| Image build | K8s may not support local build; need registry workflow |
| `host_for_pod_services()` | K8s: use Service DNS, not host gateway |

### What's Easy

| Area | Notes |
|------|-------|
| Pod structure | 1:1 mapping to K8s Pod |
| Labels | Same keys, different storage (metadata) |
| Container config | Env, mounts, etc. map directly |
| OCI images | Same images work in both |

### Dependencies and Prerequisites

- **Rust:** kube = "0.92", k8s-openapi = { version = "0.24", features = ["v1_28"] }
- **Kubernetes:** 1.28+ (for API compatibility)
- **Cluster:** Default storage class, optional Ingress controller
- **Registry:** Accessible from cluster (public or imagePullSecrets)

---

## References

- [kube-rs](https://kube.rs/) — Rust Kubernetes client
- [kube-rs features](https://kube.rs/features) — ws for exec, attach, port-forward
- [Podman REST API](https://docs.podman.io/en/latest/_static/api.html)
- [controlplane.md](./controlplane.md) — Control plane design
- [webui.md](./webui.md) — Web UI architecture
- [container-mode.md](../src/container-mode.md) — Current container deployment
