# Pod Architecture

Each devaipod workspace is a **podman pod** containing several containers:

| Container | Role |
|-----------|------|
| `workspace` | User's dev environment (from devcontainer image) |
| `agent` | Runs the AI agent (opencode); has its own workspace copy |
| `gator` | [service-gator](https://github.com/cgwalters/service-gator) — fine-grained MCP server for GitHub/GitLab/Forgejo |
| `api` | **pod-api** sidecar — HTTP server for git status, summary, completion status |
| `extra-<name>` | User-defined containers (see below) |

All containers in a pod share the network namespace (localhost communication).
The `api` container has a `/healthz` endpoint and a podman healthcheck configured.

## Extra containers

Users can inject additional containers into the pod via `[[extra-containers]]` in
`~/.config/devaipod.toml`. Each entry is a minimal container specification:

```toml
[[extra-containers]]
name = "goose"
image = "ghcr.io/block/goose-cli:latest"
command = ["goose", "session", "start"]

[extra-containers.env]
OPENAI_API_KEY = "sk-..."

[[extra-containers.mounts]]
host_path = "/home/user/.config/goose"
container_path = "/root/.config/goose"
read_only = true

[extra-containers.labels]
"app.managed-by" = "devaipod"
```

Extra containers are named `<pod-name>-extra-<name>` and created alongside the core
containers at pod creation time. They follow the same pod lifecycle (start/stop
together with the pod).

**Important:** devaipod does NOT automatically inject workspace volumes, agent home
volumes, YOLO mode (`OPENCODE_PERMISSION`), or MCP configuration into extra containers.
That is the user's responsibility. This is intentional — generic injection means
devaipod does not know what the container runs.

## Key source files

| File | Purpose |
|------|---------|
| `src/main.rs` | CLI entry point and all subcommand handlers |
| `src/pod.rs` | Pod creation, container configs, volume management |
| `src/pod_api.rs` | Pod-api sidecar HTTP server (axum) |
| `src/podman.rs` | Podman API abstraction, `ContainerConfig`, `PodmanService` |
| `src/web.rs` | Web UI server, proxy routes, auth |
| `src/config.rs` | Configuration types and loading |
| `src/ssh_server.rs` | SSH server for `exec --stdio` connections |

## Volumes

Each pod creates up to 5 named volumes (suffixed with the pod name):
`workspace`, `agent-home`, `agent-workspace`, `worker-home`, `worker-workspace`.
The worker volumes are only created when orchestration mode is enabled.

**Known issue:** `cmd_prune` and `prune_done_pods` do not clean up volumes
when removing pods. `cmd_delete` handles this correctly. This is a bug to fix.

## Tracing

All log output goes to **stderr** (via `tracing_subscriber` with
`.with_writer(std::io::stderr)`). This is important because some commands
(e.g. `exec --stdio`, `gator show --json`) use stdout for structured data.
