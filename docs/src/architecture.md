# Pod Architecture

Each devaipod workspace is a **podman pod** containing several containers:

| Container | Role |
|-----------|------|
| `workspace` | User's dev environment (from devcontainer image) |
| `agent` | Runs the AI agent (opencode by default, or an alternative framework); has its own workspace copy |
| `gator` | [service-gator](https://github.com/cgwalters/service-gator) — fine-grained MCP server for GitHub/GitLab/Forgejo |
| `api` | **pod-api** sidecar — HTTP server for git status, summary, completion status |

## Agent frameworks

The `agent` container supports multiple AI agent frameworks, configured via `[agent]` in `devaipod.toml`:

| Framework | Config value | Default image | YOLO mechanism |
|-----------|-------------|---------------|----------------|
| OpenCode (default) | `opencode` | (workspace image) | `OPENCODE_PERMISSION={"*":"allow"}` env var |
| Goose by Block | `goose` | `ghcr.io/block/goose:latest` | `GOOSE_MODE=auto` env var + config file |
| Claude Code | `claude-code` | (user must supply) | `--dangerously-skip-permissions` CLI flag |

YOLO mode (auto-approve of all tool permissions) is always injected by devaipod when `auto_approve` is true — it is never sourced from the user's dotfiles. This ensures sandboxed agents run autonomously regardless of how the user's personal config is set up for local interactive use.

Example configuration for goose:
```toml
[agent]
framework = "goose"
# image defaults to ghcr.io/block/goose:latest
```

Example for claude-code (no official headless image; user must supply one):
```toml
[agent]
framework = "claude-code"
image = "my-registry/claude-devenv:latest"
```

All containers in a pod share the network namespace (localhost communication).
The `api` container has a `/healthz` endpoint and a podman healthcheck configured.

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
