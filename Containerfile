# Containerfile for devaipod
#
# Builds devaipod as a container image for orchestrating AI agent workspaces.
# The container requires access to the host's podman socket to spawn sibling
# containers (workspace pods).
#
# Build:
#   podman build --tag ghcr.io/cgwalters/devaipod -f Containerfile .
#
# Run (web UI mode - default):
#   podman volume create devaipod-state   # optional; just container-run creates it
#   podman run -d --name devaipod -p 8080:8080 --privileged \
#     -v devaipod-state:/var/lib/devaipod \
#     -v $XDG_RUNTIME_DIR/podman/podman.sock:/run/podman/podman.sock \
#     -v ~/.config/devaipod.toml:/root/.config/devaipod.toml:ro \
#     ghcr.io/cgwalters/devaipod
#
#   The devaipod-state volume stores the web auth token by default (at
#   /var/lib/devaipod/web-token) so it persists across container restarts.
#
#   # Get the web UI URL with auth token from logs (first run only; later use same URL):
#   podman logs devaipod | grep "Web UI"
#
# Run with stable auth token (via podman secret) instead of state volume:
#   openssl rand -base64 32 | podman secret create devaipod-web-token -
#   podman run -d --name devaipod -p 8080:8080 --privileged \
#     --secret devaipod-web-token \
#     -v devaipod-state:/var/lib/devaipod \
#     -v $XDG_RUNTIME_DIR/podman/podman.sock:/run/podman/podman.sock \
#     -v ~/.config/devaipod.toml:/root/.config/devaipod.toml:ro \
#     ghcr.io/cgwalters/devaipod
#
# Interact via CLI:
#   podman exec devaipod devaipod run https://github.com/org/repo -c 'fix bug'
#   podman exec -ti devaipod devaipod attach -l
#   podman exec -ti devaipod devaipod tui
#
# Note: --privileged is required for socket access and for spawning privileged
# workspace containers (needed for nested podman in devcontainers).
#
# Uses BuildKit-style cache mounts for fast incremental Rust builds.

# -- source snapshot (keeps layer graph clean) --
FROM scratch AS src
COPY . /src

# -- opencode web UI build stage --
# Build the opencode web UI from source for vendoring
# This eliminates dependency on external app.opencode.ai
# Uses bootc-dev/devenv-debian (same as integration tests) with bun installed.
ARG OPENCODE_VERSION=v1.1.65
FROM ghcr.io/bootc-dev/devenv-debian:latest AS opencode-web
ARG OPENCODE_VERSION=v1.1.65
USER root

# Install git, curl, unzip for cloning and bun installer
RUN apt-get update && apt-get install -y --no-install-recommends \
    git \
    curl \
    unzip \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Install bun (required for opencode frontend build)
RUN curl -fsSL https://bun.sh/install | bash && \
    ln -sf /root/.bun/bin/bun /usr/local/bin/bun

WORKDIR /build
RUN git clone --depth 1 --branch ${OPENCODE_VERSION} \
    https://github.com/anomalyco/opencode.git opencode

# Install dependencies
WORKDIR /build/opencode
RUN bun install --frozen-lockfile

# Build the web app
# The app uses window.location.origin for API calls when not on opencode.ai
WORKDIR /build/opencode/packages/app
RUN bun run build

# Output is in /build/opencode/packages/app/dist

# -- build stage --
FROM quay.io/centos/centos:stream10 AS build

RUN dnf install -y \
        rust cargo \
        openssl-devel \
        gcc \
    && dnf clean all

COPY --from=src /src /src
WORKDIR /src

# Fetch dependencies (network-intensive, cached separately)
RUN --mount=type=cache,target=/src/target \
    --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    cargo fetch

# Build devaipod binary
RUN --network=none \
    --mount=type=cache,target=/src/target \
    --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    cargo build --release -p devaipod && \
    cp /src/target/release/devaipod /usr/bin/devaipod

# -- final minimal image --
FROM quay.io/centos/centos:stream10

# Install runtime dependencies:
# - podman-remote: for CLI fallback operations (exec, etc.)
# - git: for repository operations
# - openssh-clients: for SSH integration
# - tmux: for attach command's split-pane UI
RUN dnf install -y \
        podman-remote \
        git \
        openssh-clients \
        tmux \
    && dnf clean all \
    && ln -sf /usr/bin/podman-remote /usr/bin/podman

# Create config directory
RUN mkdir -p /root/.config

COPY --from=build /usr/bin/devaipod /usr/bin/devaipod

# Mark that we're running inside the official devaipod container
# This is checked by `devaipod` to require running in container mode by default
ENV DEVAIPOD_CONTAINER=1

# Copy devaipod web UI static files directly from build context
# (not from src stage, as that's a snapshot that may not include untracked files)
COPY dist /usr/share/devaipod/dist

# Copy vendored opencode web UI
# This is served at /opencode/ and proxies API calls to the agent's opencode server
COPY --from=opencode-web /build/opencode/packages/app/dist /usr/share/devaipod/opencode
WORKDIR /usr/share/devaipod

# Default: run web UI server
# The web server prints a URL with auth token to stdout on startup.
# Alternative entrypoints:
#   - `devaipod tui` for interactive TUI dashboard
#   - `sleep infinity` then use `podman exec` for commands
#   - Custom command for one-shot operations
#
# Example:
#   podman run -d -p 8080:8080 ... ghcr.io/cgwalters/devaipod
#   # Copy the URL with token from logs:
#   podman logs <container> | grep "Web UI"
EXPOSE 8080
CMD ["devaipod", "web", "--port", "8080"]
