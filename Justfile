# devaipod development tasks

# Default recipe: show available commands
default:
    @just --list

# Build in debug mode
build:
    cargo build

# Build in release mode
build-release:
    cargo build --release

# Run format and type checks, record tree hash for pre-commit hook
check: install-hooks
    cargo fmt -- --check
    cargo check
    @mkdir -p target
    @git write-tree > target/checks-run

# Run unit tests (no container runtime required)
test:
    cargo test

# Default test image (must have git installed)
default_test_image := "ghcr.io/bootc-dev/devenv-debian:latest"

# Go template for podman machine socket path (used in test-integration-container; literal braces)
_podman_socket_format := "{" + "{" + ".ConnectionInfo.PodmanSocket.Path" + "}" + "}"

# Run integration tests (requires podman)
# On macOS set DEVAIPOD_PODMAN_SOCKET so the binary finds the podman machine socket.
test-integration image=default_test_image:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build
    if [ -z "${XDG_RUNTIME_DIR:-}" ] && command -v podman &>/dev/null; then
        SOCKET=$(podman machine inspect --format '{{_podman_socket_format}}' 2>/dev/null || true)
        if [ -n "$SOCKET" ] && [ -S "$SOCKET" ]; then
            export DEVAIPOD_PODMAN_SOCKET="$SOCKET"
        fi
    fi
    DEVAIPOD_PATH=./target/debug/devaipod DEVAIPOD_TEST_IMAGE={{image}} cargo test -p integration-tests

# Shared implementation for container integration tests.
# Starts the devaipod container, then runs integration tests inside it.
# On macOS the container runs in the VM; use VM socket path /run/podman/podman.sock.
[private]
_run-integration-container image threads:
    #!/usr/bin/env bash
    set -euo pipefail
    SOCKET=""
    if [ -n "${XDG_RUNTIME_DIR:-}" ] && [ -S "${XDG_RUNTIME_DIR}/podman/podman.sock" ]; then
        SOCKET="${XDG_RUNTIME_DIR}/podman/podman.sock"
    elif command -v podman &>/dev/null; then
        SOCKET=$(podman machine inspect --format '{{_podman_socket_format}}' 2>/dev/null || true)
    fi
    if [ -z "$SOCKET" ] || [ ! -S "$SOCKET" ]; then
        echo "Could not find podman socket. Set XDG_RUNTIME_DIR (Linux) or start podman machine (macOS)."
        exit 1
    fi
    CONFIG=".ci/devaipod-test.toml"
    if [ ! -f "$CONFIG" ]; then
        echo "Missing $CONFIG (minimal config for container tests)"
        exit 1
    fi
    PWD_ABS=$(cd . && pwd)
    TMPDIR_ABS="${PWD_ABS}/tmp"
    mkdir -p "$TMPDIR_ABS"
    cleanup() { podman rm -f devaipod 2>/dev/null || true; }
    trap cleanup EXIT
    mkdir -p ~/.ssh/config.d/devaipod
    if [ -n "${XDG_RUNTIME_DIR:-}" ]; then
        VOL_MOUNT="-v $SOCKET:/run/podman/podman.sock"
    else
        VOL_MOUNT="-v /run/podman/podman.sock:/run/podman/podman.sock"
    fi
    echo "Starting devaipod container (socket, workspace at same path, test config)..."
    if ! podman volume exists devaipod-state 2>/dev/null; then
        podman volume create devaipod-state
    fi
    ADD_HOST=""
    if [ -n "${XDG_RUNTIME_DIR:-}" ]; then
        ADD_HOST="--add-host=host.containers.internal:host-gateway"
    fi
    podman run -d --name devaipod --privileged --replace \
        $ADD_HOST \
        $VOL_MOUNT \
        -v devaipod-state:/var/lib/devaipod \
        -v "$PWD_ABS:$PWD_ABS" \
        -v "$(pwd)/$CONFIG:/root/.config/devaipod.toml:ro" \
        -v ~/.ssh/config.d/devaipod:/run/devaipod-ssh:Z \
        -w "$PWD_ABS" \
        {{ CONTAINER_IMAGE }}:latest
    echo "Waiting for devaipod container to be running..."
    for i in $(seq 1 30); do
        if [ "$(podman inspect --format '{{ '{{' }}.State.Running{{ '}}' }}' devaipod 2>/dev/null)" = "true" ]; then
            break
        fi
        sleep 1
    done
    if [ "$(podman inspect --format '{{ '{{' }}.State.Running{{ '}}' }}' devaipod 2>/dev/null)" != "true" ]; then
        echo "devaipod container failed to reach running state"
        podman logs devaipod 2>&1 | tail -50
        exit 1
    fi
    echo "Running integration tests against built container ({{ CONTAINER_IMAGE }}:latest)..."
    cargo build -p integration-tests
    export DEVAIPOD_CONTAINER_IMAGE="{{ CONTAINER_IMAGE }}:latest"
    export DEVAIPOD_PODMAN_SOCKET="$SOCKET"
    RUST_TEST_THREADS="${RUST_TEST_THREADS:-{{threads}}}" TMPDIR="$TMPDIR_ABS" DEVAIPOD_TEST_IMAGE={{image}} cargo test -p integration-tests

# Run integration tests with a full --no-cache container rebuild.
# Use for CI or when web routes / Containerfile have changed.
# Override parallelism with RUST_TEST_THREADS env var (default: serial).
test-integration-container image=default_test_image: container-build-for-integration (_run-integration-container image "1")

# Fast variant for local iteration: uses cached container build.
# Defaults to 4 test threads (web tests are parallel-safe).
# Override with RUST_TEST_THREADS env var.
test-integration-container-quick image=default_test_image: container-build (_run-integration-container image "4")

# Run all tests (unit + integration)
test-all: test test-integration

# Format code
fmt:
    cargo fmt

# Clean build artifacts
clean:
    cargo clean

# Run devaipod with arguments (builds release first)
run *ARGS: build-release
    ./target/release/devaipod {{ARGS}}

# Build and install to ~/.cargo/bin
install:
    cargo install --path .

# Quick smoke test: start workspace, check agent
smoke-test:
    cargo build
    ./target/debug/devaipod up . --no-agent
    ./target/debug/devaipod list
    ./target/debug/devaipod delete devc --force

# Run devaipod against our own local git tree for self-hosting development.
# This tears down any existing devcontainer completely and starts a fresh workspace
# using the devcontainer feature. Run this from outside the devcontainer (e.g. toolbox)
# to iterate on changes.
self-devenv:
    #!/usr/bin/env bash
    set -euo pipefail
    # Build the binary first
    cargo build --release
    # Stop and remove existing devpod workspace to force fresh container
    devpod stop devaipod 2>/dev/null || true
    devpod delete devaipod --force 2>/dev/null || true
    # Start fresh workspace with our local tree (uses devcontainer.json with feature)
    ./target/release/devaipod up .

# Alias for self-devenv (used by devenv-self convention)
devenv-self: self-devenv

# Default test repository for e2e GitHub tests
default_test_repo := "cgwalters/playground"

# Default workspace for e2e tests
default_test_workspace := "playground"

# Run end-to-end GitHub integration tests
# Requires a running devpod workspace with devaipod installed and gh authenticated
test-e2e-gh repo=default_test_repo workspace=default_test_workspace:
    DEVAIPOD_TEST_REPO={{repo}} DEVAIPOD_TEST_WORKSPACE={{workspace}} \
        cargo test test_e2e_gh -- --ignored --test-threads=1

# Run a specific e2e test (e.g., just test-e2e-gh-one test_e2e_gh_read_operations)
test-e2e-gh-one test repo=default_test_repo workspace=default_test_workspace:
    DEVAIPOD_TEST_REPO={{repo}} DEVAIPOD_TEST_WORKSPACE={{workspace}} \
        cargo test {{test}} -- --ignored

# Set up the test workspace for e2e tests
# Deploys the current devaipod binary and configures gh auth if GH_TOKEN is set
setup-e2e-gh workspace=default_test_workspace:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "Building devaipod..."
    cargo build --release
    echo "Deploying to {{workspace}}.devpod..."
    scp target/release/devaipod {{workspace}}.devpod:/tmp/
    ssh {{workspace}}.devpod 'sudo cp /tmp/devaipod /usr/local/bin/devaipod && sudo chmod +x /usr/local/bin/devaipod'
    echo "Verifying installation..."
    ssh {{workspace}}.devpod 'devaipod --help | head -5'
    
    # Configure gh auth if GH_TOKEN is available
    if [ -n "${GH_TOKEN:-}" ]; then
        echo "Configuring gh auth with GH_TOKEN..."
        ssh {{workspace}}.devpod "echo '${GH_TOKEN}' | gh auth login --with-token" || {
            echo "Warning: gh auth failed (gh may not be installed)"
        }
        ssh {{workspace}}.devpod 'gh auth status' || true
    else
        echo "Note: GH_TOKEN not set, skipping gh auth configuration"
    fi
    
    echo "Done! Run 'just test-e2e-gh' to run the tests."

# Configure allowed repo for e2e PR creation test
# This adds the repo to the state file inside the devcontainer
allow-repo-e2e repo=default_test_repo workspace=default_test_workspace:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "Adding {{repo}} to allowed repos in {{workspace}}.devpod..."
    ssh {{workspace}}.devpod 'sudo mkdir -p /run/devaipod && sudo chown $(id -u):$(id -g) /run/devaipod'
    ssh {{workspace}}.devpod "echo '{\"allowed_repos\":[\"{{repo}}\"],\"allowed_prs\":[]}' > /run/devaipod/state.json"
    echo "Done! The repo {{repo}} is now allowed for PR creation."

# ============================================================================
# Container builds
# ============================================================================

# Container image name (use localhost/ for local builds to avoid confusion with registry)
CONTAINER_IMAGE := "localhost/devaipod"

# Build the container image.
# no_cache: set to "--no-cache" to force full rebuild (used by test-integration-container so image has latest routes).
[group('container')]
container-build no_cache="":
    podman build {{ no_cache }} -t {{ CONTAINER_IMAGE }}:latest -f Containerfile .

# Force full rebuild (no cache); ensures latest code changes are in the image, but slow.
# Used by test-integration-container so the image has current web routes; use for CI or when web routes change.
[group('container')]
container-build-for-integration:
    podman build --no-cache -t {{ CONTAINER_IMAGE }}:latest -f Containerfile .

# Test the container image
[group('container')]
container-test: container-build
    #!/usr/bin/env bash
    set -euo pipefail
    echo "Testing container image..."
    
    # Verify the binary runs
    podman run --rm {{ CONTAINER_IMAGE }}:latest devaipod --help
    
    # Verify runtime dependencies are present
    podman run --rm {{ CONTAINER_IMAGE }}:latest git --version
    podman run --rm {{ CONTAINER_IMAGE }}:latest tmux -V
    podman run --rm {{ CONTAINER_IMAGE }}:latest podman --version
    
    echo "Container tests passed!"

# Build and push container image (for CI)
[group('container')]
container-push tag="latest": container-build
    podman push {{ CONTAINER_IMAGE }}:{{ tag }}

# Run devaipod as a container daemon
# Mounts podman socket, config, and SSH config export directory
# Uses host gateway (host.containers.internal) to reach pod-published ports; avoids --network host
# so port forwarding works on macOS. All pod services use auth (auth proxy).
# Socket: Linux uses XDG_RUNTIME_DIR; macOS/Windows use VM path /run/podman/podman.sock (container runs in VM).
[group('container')]
container-run: container-build
    #!/usr/bin/env bash
    set -euo pipefail
    SOCKET=""
    if [ -n "${XDG_RUNTIME_DIR:-}" ] && [ -S "${XDG_RUNTIME_DIR}/podman/podman.sock" ]; then
        SOCKET="${XDG_RUNTIME_DIR}/podman/podman.sock"
    elif command -v podman &>/dev/null; then
        SOCKET=$(podman machine inspect --format '{{_podman_socket_format}}' 2>/dev/null || true)
    fi
    if [ -z "$SOCKET" ] || [ ! -S "$SOCKET" ]; then
        echo "Could not find podman socket. Linux: set XDG_RUNTIME_DIR. macOS/Windows: run 'podman machine start' and ensure default machine exists."
        exit 1
    fi
    echo "Using podman socket: $SOCKET"
    mkdir -p ~/.ssh/config.d/devaipod
    if [ ! -f ~/.config/devaipod.toml ]; then
        echo "Warning: ~/.config/devaipod.toml not found; container may exit. Run 'devaipod init' on the host first."
    fi
    # Allocate devaipod-state volume if missing (auth token and other state stored there by default)
    if ! podman volume exists devaipod-state 2>/dev/null; then
        podman volume create devaipod-state
        echo "Created volume devaipod-state"
    fi
    # Linux: mount the host socket (path is on the host). macOS/podman machine: the container runs in the VM,
    # so the volume source must be the VM's path, not the Mac path. Use the VM's podman socket path so the
    # daemon (in the VM) bind-mounts its own socket into the container. Rootful VM uses /run/podman/podman.sock.
    if [ -n "${XDG_RUNTIME_DIR:-}" ]; then
        VOL_MOUNT="-v $SOCKET:/run/podman/podman.sock"
        ADD_HOST="--add-host=host.containers.internal:host-gateway"
    else
        VOL_MOUNT="-v /run/podman/podman.sock:/run/podman/podman.sock"
        ADD_HOST=""
    fi
    podman run -d --name devaipod --privileged --replace \
        -p 8080:8080 \
        $ADD_HOST \
        $VOL_MOUNT \
        -v devaipod-state:/var/lib/devaipod \
        -v ~/.config/devaipod.toml:/root/.config/devaipod.toml:ro \
        -v ~/.ssh/config.d/devaipod:/run/devaipod-ssh:Z \
        {{ CONTAINER_IMAGE }}:latest
    echo "devaipod container started"
    echo "Web UI: http://127.0.0.1:8080/"
    echo "SSH configs will be written to ~/.ssh/config.d/devaipod/"
    echo ""
    echo "Ensure your ~/.ssh/config has: Include config.d/devaipod/*"
    echo ""
    echo "If you cannot connect to 127.0.0.1:8080, run: just container-debug"

# Debug connection to devaipod container (run after container-run)
# Checks: container running, port mapping, recent logs, curl to /health
[group('container')]
container-debug:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "=== devaipod container connection debug ==="
    echo ""
    if ! podman container exists devaipod 2>/dev/null; then
        echo "FAIL: Container 'devaipod' does not exist. Run 'just container-run' first."
        exit 1
    fi
    echo "1. Container state:"
    podman inspect devaipod --format '   State: {{ '{{' }}.State.Status{{ '}}' }} (Running={{ '{{' }}.State.Running{{ '}}' }})'
    if [ "$(podman inspect --format '{{ '{{' }}.State.Running{{ '}}' }}' devaipod 2>/dev/null)" != "true" ]; then
        echo "   Container is not running. Last logs:"
        podman logs devaipod 2>&1 | tail -30
        exit 1
    fi
    echo ""
    echo "2. Port mapping (host -> container):"
    podman port devaipod 2>/dev/null || echo "   (no ports published)"
    echo ""
    echo "3. Process inside container (devaipod web):"
    podman top devaipod 2>/dev/null || true
    echo ""
    echo "4. Last 15 lines of container logs:"
    podman logs devaipod 2>&1 | tail -15
    echo ""
    echo "5. Curl from host to 127.0.0.1:8080/_devaipod/health:"
    if curl -sf --connect-timeout 3 http://127.0.0.1:8080/_devaipod/health 2>/dev/null; then
        echo ""
        echo "   OK: Connection succeeded."
    else
        echo "   FAIL: Connection refused or timeout."
        if [ -z "${XDG_RUNTIME_DIR:-}" ]; then
            echo ""
            echo "   On macOS with podman machine, port forwarding (-p 8080:8080) may not reach the host."
            echo "   Workaround: use Podman Desktop port forwarding, or run devaipod on the host:"
            echo "   cargo run -- web --port 8080"
        fi
        exit 1
    fi

# ============================================================================
# Documentation
# ============================================================================

# Build the documentation (mdbook) via container
build-mdbook:
    podman build -t localhost/devaipod-mdbook -f docs/Dockerfile.mdbook .

# Build docs and extract to DIR
build-mdbook-to dir: build-mdbook
    #!/usr/bin/env bash
    set -xeuo pipefail
    outdir="$(pwd)/{{dir}}"
    mkdir -p "${outdir}"
    cid=$(podman create localhost/devaipod-mdbook)
    podman cp ${cid}:/src/docs/book/. "${outdir}"
    podman rm -f ${cid}

# Serve docs locally (prints URL)
mdbook-serve: build-mdbook
    #!/usr/bin/env bash
    set -xeuo pipefail
    podman run --init --replace -d --name devaipod-mdbook --rm --publish 127.0.0.1::8000 localhost/devaipod-mdbook
    echo http://$(podman port devaipod-mdbook 8000/tcp)

# Install git hooks for development
install-hooks:
    cp scripts/pre-commit .git/hooks/pre-commit
    chmod +x .git/hooks/pre-commit
    @echo "Installed pre-commit hook"

# Default repo for agent draft PR E2E test
e2e_test_repo := "cgwalters/playground"

# E2E test: verify agent can start a task and create a draft PR
# This is a real integration test that exercises the full flow:
# 1. devaipod run with a task message
# 2. Agent starts working autonomously
# 3. Agent creates a draft PR on GitHub
#
# Requirements:
# - GH_TOKEN in environment (for service-gator to push)
# - Network access to GitHub
#
# The test polls for PR creation with a timeout.
e2e-draft-pr repo=e2e_test_repo:
    #!/usr/bin/env bash
    set -euo pipefail
    
    REPO="{{repo}}"
    TASK="Create a draft PR that adds a single line to README.md with today's date ($(date +%Y-%m-%d)) and a random UUID. The commit message should mention this is an automated test."
    
    echo "Building devaipod..."
    cargo build --release
    
    # Generate unique identifier for this test run
    TEST_ID="e2e-$(date +%s)-$$"
    POD_NAME="e2e-test-${TEST_ID}"
    
    echo "Starting devaipod with task..."
    echo "  Repo: https://github.com/${REPO}"
    echo "  Task: ${TASK}"
    
    # Start the agent with the task
    ./target/release/devaipod run "https://github.com/${REPO}" "${TASK}" --name "${POD_NAME}" || {
        echo "Failed to start devaipod"
        exit 1
    }
    
    # Function to cleanup on exit
    cleanup() {
        echo "Cleaning up pod ${POD_NAME}..."
        ./target/release/devaipod delete "${POD_NAME}" --force 2>/dev/null || true
    }
    trap cleanup EXIT
    
    echo "Agent started. Polling for draft PR creation..."
    
    # Poll for draft PR creation (timeout: 5 minutes)
    MAX_ATTEMPTS=60
    POLL_INTERVAL=5
    
    for i in $(seq 1 $MAX_ATTEMPTS); do
        echo "  Checking for draft PRs (attempt $i/$MAX_ATTEMPTS)..."
        
        # Check for draft PRs created in the last 10 minutes
        DRAFT_PRS=$(gh pr list --repo "${REPO}" --state open --draft --json number,title,createdAt --jq '.[] | select(.createdAt > (now - 600 | strftime("%Y-%m-%dT%H:%M:%SZ"))) | "\(.number): \(.title)"' 2>/dev/null || echo "")
        
        if [ -n "$DRAFT_PRS" ]; then
            echo ""
            echo "SUCCESS: Found draft PR(s):"
            echo "$DRAFT_PRS"
            echo ""
            echo "E2E test passed!"
            exit 0
        fi
        
        # Also check if agent is still working by looking at session activity
        CONTAINER="devaipod-${POD_NAME}-workspace"
        LAST_UPDATE=$(podman exec "$CONTAINER" curl -sf http://localhost:4096/session 2>/dev/null | jq -r '.[0].time.updated // 0' || echo "0")
        NOW_MS=$(($(date +%s) * 1000))
        IDLE_MS=$((NOW_MS - LAST_UPDATE))
        
        if [ "$IDLE_MS" -gt 120000 ] && [ "$i" -gt 10 ]; then
            echo "  Agent appears idle (last activity: ${IDLE_MS}ms ago)"
        fi
        
        sleep $POLL_INTERVAL
    done
    
    echo ""
    echo "TIMEOUT: No draft PR found after $((MAX_ATTEMPTS * POLL_INTERVAL)) seconds"
    echo "Check agent logs: devaipod attach ${POD_NAME}"
    exit 1
