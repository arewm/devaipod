# Container Mode

Devaipod can run as a container image itself, rather than as a host binary. This is useful for:

- **Reproducible deployments** - same devaipod version everywhere
- **CI/CD integration** - run agents in pipelines without installing binaries
- **Server deployments** - run devaipod as a daemon managing multiple workspaces
- **Isolation** - keep devaipod itself sandboxed from the host

## Quick Start

```bash
# Pull the image
podman pull ghcr.io/cgwalters/devaipod:latest

# Run as a daemon (Linux rootless podman)
SOCKET=$XDG_RUNTIME_DIR/podman/podman.sock
podman run -d --name devaipod --privileged \
  --add-host=host.containers.internal:host-gateway \
  -v $SOCKET:/run/docker.sock -e DEVAIPOD_HOST_SOCKET=$SOCKET \
  -v ~/.config/devaipod.toml:/root/.config/devaipod.toml:ro \
  ghcr.io/cgwalters/devaipod

# Run an agent
podman exec devaipod devaipod run https://github.com/org/repo -c 'fix the bug'

# Attach to the TUI
podman exec -ti devaipod devaipod attach -l

# Use the dashboard
podman exec -ti devaipod devaipod tui

# List workspaces
podman exec devaipod devaipod list
```

## Requirements

### Privileged Mode and Host Gateway

The `--privileged` flag is required for:
1. Access to the mounted podman socket
2. Spawning privileged workspace containers (needed for nested podman in devcontainers)

We do **not** use `--network host`. Instead, the container uses the host gateway
(`--add-host=host.containers.internal:host-gateway`) to reach pod-published ports.
Each pod exposes an auth proxy (with Basic Auth) on a random host port; devaipod
connects to `host.containers.internal:&lt;port&gt;` to proxy to opencode. This keeps
port forwarding working on macOS (where `--network host` would use the VM's network
and break forwarding from the Mac). Override with `DEVAIPOD_HOST_GATEWAY` if needed
(e.g. `host.docker.internal`). All pod services that are exposed use auth.

A future option is a **per-pod gateway sidecar** (Rust): one small gateway container
per pod that proxies opencode (and optionally other services) with auth, exposing
a single port per pod. See [per-pod-gateway-sidecar.md](../todo/per-pod-gateway-sidecar.md).

### Container Runtime Socket

The container needs access to the host's container runtime socket to spawn sibling containers. Mount it at `/run/docker.sock` inside the container — this is the well-known path that devaipod checks by default, and it works with both Docker and Podman.

You must also set `DEVAIPOD_HOST_SOCKET` to the **host-side** path of the socket. Devaipod needs this because when it creates sibling containers, the bind mount source is resolved by the host's container daemon — not inside the devaipod container. On rootless Linux, the host path (e.g. `/run/user/1000/podman/podman.sock`) differs from the container-internal `/run/docker.sock`.

For **Podman** (Linux rootless):
```bash
SOCKET=$XDG_RUNTIME_DIR/podman/podman.sock
-v $SOCKET:/run/docker.sock -e DEVAIPOD_HOST_SOCKET=$SOCKET
```

For **Docker**:
```bash
-v /var/run/docker.sock:/run/docker.sock -e DEVAIPOD_HOST_SOCKET=/var/run/docker.sock
```

Alternatively, set the `DOCKER_HOST` environment variable instead of mounting a socket (e.g. `-e DOCKER_HOST=unix:///path/to/socket`).

On systems using podman machine (macOS, Windows), use the appropriate socket path from `podman machine inspect`.

### macOS and Windows (podman machine)

On macOS and Windows, podman runs containers inside a Linux VM. The **volume source for the socket must be the path inside the VM**, not the host path. The Mac path (e.g. from `podman machine inspect`) is for the Mac client; the container runs in the VM, so we mount the VM's podman socket. For a rootful machine that is `/run/podman/podman.sock` in the VM.

The `just container-run` recipe does this automatically: on Linux it mounts `$XDG_RUNTIME_DIR/podman/podman.sock` and sets `DEVAIPOD_HOST_SOCKET` to match; when that is unset (macOS/Windows) it uses the VM path `/run/podman/podman.sock` for both.

To run manually on macOS:

```bash
podman run -d --name devaipod --privileged --replace \
  --add-host=host.containers.internal:host-gateway \
  -v /run/podman/podman.sock:/run/docker.sock \
  -e DEVAIPOD_HOST_SOCKET=/run/podman/podman.sock \
  -v ~/.config/devaipod.toml:/root/.config/devaipod.toml:ro \
  ghcr.io/cgwalters/devaipod
```

(Rootless podman machine may use a different socket path in the VM; if `devaipod list` fails, check the VM's socket location.)

## State Volume and Web Auth Token

A **devaipod-state** volume can be used to persist the web UI auth token (and future state) across container restarts. Mount it at `/var/lib/devaipod`:

```bash
podman volume create devaipod-state   # once; just container-run does this automatically
podman run -d --name devaipod ... \
  -v devaipod-state:/var/lib/devaipod \
  ...
```

Token lookup order: (1) podman secret `/run/secrets/devaipod-web-token` if provided, (2) `/var/lib/devaipod/web-token` from the state volume. If no token exists, one is generated and written to the state path when the directory is present, so the same URL works after restarts. Override the state directory with `DEVAIPOD_STATE_DIR`.

## Credentials and Secrets

In container mode, devaipod cannot access the host's home directory. The `bind_home` configuration option is **not supported** and will error if configured. Instead, use podman secrets:

### Podman Secrets (Required)

Create podman secrets on the host and reference them in your config:

```bash
# Create secrets
echo "$ANTHROPIC_API_KEY" | podman secret create anthropic_api_key -
echo "$GH_TOKEN" | podman secret create gh_token -
```

Then in `~/.config/devaipod.toml`:

```toml
[trusted]
secrets = [
  "ANTHROPIC_API_KEY=anthropic_api_key",
  "GH_TOKEN=gh_token",
]

# IMPORTANT: Remove any bind_home configuration for container mode
# [bind_home]
# paths = [...]  # This will error in container mode
```

The secrets are passed through to workspace containers using podman's `--secret type=env` feature.

### Migrating from bind_home

If you have an existing config with `bind_home` for credentials, you'll need to:

1. Create podman secrets for each credential
2. Add them to `[trusted.secrets]`
3. Remove the `[bind_home]` section (or use a separate config for container mode)

## Building the Image

To build locally:

```bash
# Using just
just container-build

# Or directly
podman build -t ghcr.io/cgwalters/devaipod -f Containerfile .
```

The multi-stage Containerfile:
1. Builds devaipod from source using CentOS Stream 10
2. Creates a minimal runtime image with `podman-remote`, `git`, `tmux`, and `openssh-clients`

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│ Host                                                        │
│  ┌─────────────────────┐                                    │
│  │ podman.sock         │◄──────────────────┐               │
│  └─────────────────────┘                   │               │
│                                            │               │
│  ┌─────────────────────┐     ┌─────────────┴─────────────┐ │
│  │ devaipod container  │     │ Created workspace pods    │ │
│  │ (daemon mode)       │────►│ - workspace container     │ │
│  │                     │     │ - agent container         │ │
│  └─────────────────────┘     │ - gator container         │ │
│           ▲                  │ - worker container        │ │
│           │                  └───────────────────────────┘ │
│  podman exec -ti devaipod                                  │
│  (for TUI/CLI access)                                      │
└─────────────────────────────────────────────────────────────┘
```

The devaipod container uses podman-remote to communicate with the host's podman daemon via the mounted socket. This allows it to create "sibling" containers (workspace pods) that run alongside it on the host.

## SSH Config Export

To enable VSCode/Zed Remote SSH from the host to connect to workspaces created by the containerized devaipod, bind-mount a directory to `/run/devaipod-ssh`:

```bash
# On host: create the SSH config directory
mkdir -p ~/.ssh/config.d/devaipod

# Add to ~/.ssh/config (at the top):
# Include config.d/devaipod/*

# Run with the bind mount
SOCKET=$XDG_RUNTIME_DIR/podman/podman.sock
podman run -d --name devaipod --privileged \
  --add-host=host.containers.internal:host-gateway \
  -v $SOCKET:/run/docker.sock -e DEVAIPOD_HOST_SOCKET=$SOCKET \
  -v ~/.config/devaipod.toml:/root/.config/devaipod.toml:ro \
  -v ~/.ssh/config.d/devaipod:/run/devaipod-ssh:Z \
  ghcr.io/cgwalters/devaipod
```

When `/run/devaipod-ssh` exists, devaipod automatically writes SSH configs there instead of inside the container. No configuration needed.

## Limitations

- **No local repository support** - Container mode only works with remote URLs, not local directories
- **bind_home not supported** - The `[bind_home]` config section will error in container mode; use `[trusted.secrets]` instead

## Daemon Management

The container runs `sleep infinity` by default, acting as a daemon. To stop it:

```bash
podman stop devaipod
podman rm devaipod
```

Workspaces created by the daemon persist independently and continue running even if the devaipod container is stopped.
