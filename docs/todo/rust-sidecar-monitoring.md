# Alternative: Rust Sidecar for Worker Monitoring

> **Status: Superseded.** The pod-api sidecar (`{pod}-api` container) now exists in each pod and handles monitoring, status, and agent lifecycle. The Python `worker_monitor.py` has been removed. The approach below (host-side nsenter into pod netns) was not pursued; instead, a container-based sidecar (pod-api) was implemented, which is simpler and more portable. This document is retained for historical context.

This documents a potential future enhancement to replace the Python-based `worker_monitor.py` with a native Rust binary running on the host.

## Current Approach

A Python script (`worker_monitor.py`) is injected into the agent container. The task owner calls it via bash to poll the worker's OpenCode API until idle or timeout.

## Alternative Design

Instead of injecting a script, run a Rust binary on the host that:
1. Runs via `systemd-run` with `BindsTo=` linking its lifecycle to the podman unit
2. Enters the pod's network namespace to be reachable from within containers
3. Provides an HTTP API for monitoring

## Technical Considerations

### Network Namespace Access

All containers in a pod share a network namespace. A host sidecar would need to enter this namespace to be reachable at `localhost` from containers.

Options:
- **nsenter with netns file**: `nsenter --net=/run/user/$UID/netns/netns-$UUID /path/to/binary`
  - Works with rootless podman (user owns the netns file)
  - Need to discover netns UUID via podman inspection
- **Container reaches host**: Use `host.containers.internal` from within container
  - Simpler, no namespace entering
  - May not work reliably in all network modes

### Systemd Lifecycle Binding

Podman creates transient scopes like `podman-$PID.scope`. To bind a sidecar:

```bash
systemd-run --user \
  --unit=devaipod-monitor-$POD_NAME \
  --property=BindsTo=podman-$INFRA_PID.scope \
  --property=After=podman-$INFRA_PID.scope \
  /path/to/worker-monitor
```

Challenge: The scope name is PID-based and changes on pod restart, requiring dynamic discovery.

### Implementation Sketch

```rust
// After pod starts, get network namespace
let infra_id = format!("{}-infra", pod_name);
let netns_path = podman.get_netns_path(&infra_id)?;

// Start monitor in that namespace
let monitor_proc = Command::new("nsenter")
    .args(["--net", &netns_path, "--", "/path/to/worker-monitor"])
    .spawn()?;
```

## Trade-offs

| Aspect | Python Script | Rust Sidecar |
|--------|--------------|--------------|
| Deployment | Injected at create time | Separate binary on host |
| Network | Already in pod namespace | Must enter namespace |
| Lifecycle | Container lifecycle | Needs systemd binding |
| Dependencies | Python 3 (usually present) | None (static binary) |
| Security | Sandboxed in container | Runs on host |
| Complexity | Low | Medium-High |
| Portability | Works everywhere | Linux-specific (namespaces) |

## Recommendation

~~The Python approach is simpler and sufficient for now.~~ **Update:** Neither the Python approach nor the host-side nsenter approach was used. Instead, monitoring was implemented as part of the pod-api sidecar container, which runs in the pod network namespace naturally and provides a `/summary` endpoint for status queries. This is the "middle-ground option" mentioned below, realized as a proper container rather than a host process.
