# Configuration

devaipod is configured via `~/.config/devaipod.toml` and per-project `devcontainer.json` files.

## Global Configuration

Create `~/.config/devaipod.toml`:

```toml
# Default image for repositories without devcontainer.json (optional)
default-image = "ghcr.io/bootc-dev/devenv-debian"

# Global environment variables for all containers
[env]
# Forward these from host environment (if they exist)
allowlist = ["GOOGLE_CLOUD_PROJECT", "SSH_AUTH_SOCK", "VERTEX_LOCATION"]

# Set these explicitly
[env.vars]
VERTEX_LOCATION = "global"
EDITOR = "vim"

# Trusted environment variables (workspace + gator only, NOT agent)
[trusted.env]
allowlist = ["GH_TOKEN", "GITLAB_TOKEN", "JIRA_API_TOKEN"]

# Or use podman secrets (recommended)
[trusted]
secrets = ["GH_TOKEN=gh_token", "GITLAB_TOKEN=gitlab_token"]

# GPU passthrough (optional)
[gpu]
enabled = true  # or "auto" to detect
target = "all"  # or "workspace", "agent"

# Service-gator default configuration (optional)
[service-gator]
enabled = true
port = 8765

[service-gator.gh.repos]
"myorg/*" = { read = true }
"myorg/main-project" = { read = true, create-draft = true }
```

## Using Without devcontainer.json

Not all repositories include a `devcontainer.json`. There are two ways to use devaipod with these repos:

**Option 1: Set a default image globally** (recommended)

Add to `~/.config/devaipod.toml`:

```toml
default-image = "ghcr.io/bootc-dev/devenv-debian"
```

This image will be used automatically for any repository that lacks a devcontainer.json.

**Option 2: Specify an image per-invocation**

```bash
devaipod up https://github.com/org/repo --image ghcr.io/bootc-dev/devenv-debian
devaipod run https://github.com/org/repo --image ghcr.io/bootc-dev/devenv-debian -c 'fix typos'
```

The `--image` flag takes precedence over both `default-image` and any devcontainer.json.

## Per-Project Configuration

Projects use standard `devcontainer.json` with optional devaipod customizations:

```json
{
  "name": "my-project",
  "image": "ghcr.io/bootc-dev/devenv-debian:latest",
  "customizations": {
    "devaipod": {
      "envAllowlist": ["MY_API_KEY", "CUSTOM_TOKEN"]
    }
  }
}
```

### Secrets in devcontainer.json

Declare secrets that should be injected from podman:

```json
{
  "secrets": {
    "GEMINI_API_KEY": {
      "description": "API key for Google Gemini"
    },
    "ANTHROPIC_API_KEY": {
      "description": "API key for Claude"
    }
  }
}
```

Then create matching podman secrets:

```bash
echo "your-api-key" | podman secret create GEMINI_API_KEY -
```

## Environment Variable Priority

Environment variables are merged in this order (later wins):

1. Global `[env]` section in devaipod.toml
2. Per-project `containerEnv` in devcontainer.json
3. Per-project `customizations.devaipod.envAllowlist`
4. Command-line `--env` flags

## Service-gator CLI Flags

Override configuration with CLI flags:

```bash
# Read-only access to all GitHub repos
devaipod up . --service-gator=github:readonly-all

# Read + draft PR access to specific repo
devaipod up . --service-gator=github:myorg/myrepo:read,create-draft

# Custom image
devaipod up . --service-gator=github:myorg/myrepo --service-gator-image localhost/service-gator:dev
```

See [Service-gator Integration](service-gator.md) for full details.

## Multi-Agent Orchestration

Every devaipod workspace includes a worker container alongside the task owner agent. The task owner delegates subtasks to the worker and reviews its commits before merging.

Configure worker behavior in `~/.config/devaipod.toml`:

```toml
[orchestration]
worker_timeout = "30m"   # Timeout for worker subtasks

[orchestration.worker]
# How the worker accesses service-gator
# Options: "readonly" (default), "inherit", "none"
gator = "readonly"
```

**Worker gator options:**

- `"readonly"`: Worker can only read from forge (no PRs, no pushes) — **default**
- `"inherit"`: Worker gets same gator scopes as task owner
- `"none"`: Worker has no gator access

The worker is one step further from human review, so it has restricted access by default.
