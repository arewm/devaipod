# Service-gator Integration

## Overview

[service-gator](https://github.com/cgwalters/service-gator) is an MCP server that provides scope-restricted access to external services (GitHub, JIRA, GitLab) for AI agents. It runs in a **separate gator container** alongside the workspace and agent containers, providing a security boundary between the sandboxed AI agent and your external credentials.

## Architecture

```
┌───────────────────────────────────────────────────────────────────┐
│  Podman Pod                                                        │
│                                                                    │
│  ┌─────────────────────┐  ┌─────────────────────┐                 │
│  │ Workspace Container │  │ Gator Container     │                 │
│  │ • Full dev env      │  │ • service-gator     │                 │
│  │ • Has GH_TOKEN      │  │ • Has GH_TOKEN      │                 │
│  │ • (trusted)         │  │ • Scope-restricted  │                 │
│  └─────────────────────┘  └──────────┬──────────┘                 │
│                                      │ MCP (HTTP)                  │
│  ┌───────────────────────────────────┼──────────────────────────┐ │
│  │ Agent Container (restricted)      │                          │ │
│  │ • opencode serve                  │                          │ │
│  │ • NO GH_TOKEN (no direct access)  │                          │ │
│  │ • Connects to gator via MCP ──────┘                          │ │
│  │ • Dropped capabilities, no-new-privileges                    │ │
│  └──────────────────────────────────────────────────────────────┘ │
└───────────────────────────────────────────────────────────────────┘
```

## Recommended Setup: Global GitHub Read-Only

For most users, the recommended configuration is **global read-only access to all GitHub repos**. This allows the AI agent to browse code, read PRs/issues, and understand context across repositories while preventing any write operations.

First, create a podman secret for your GitHub token (one-time setup):
```bash
echo 'ghp_your_token_here' | podman secret create gh_token -
```

Then add this to `~/.config/devaipod.toml`:

```toml
# Use podman secrets to provide GH_TOKEN to service-gator (but NOT to the agent)
[trusted]
secrets = ["GH_TOKEN=gh_token"]

# Enable service-gator with global read-only GitHub access
[service-gator.gh]
read = true
```

With this configuration, every `devaipod up` or `devaipod run` will automatically have service-gator enabled with read access to all GitHub repos. The agent can:
- Read repository contents, PRs, issues, and comments via `gh api repos/OWNER/REPO/...`
- Understand cross-repo dependencies

But the agent **cannot**:
- Push code or create branches
- Create, merge, or close PRs
- Comment on issues or PRs
- Modify any repository state

This is the safest default for productive AI-assisted development.

The `read = true` setting enables:
- All `/repos/OWNER/REPO/...` endpoints (any owner/repo)
- Non-repo endpoints: `/search/...`, `/gists/...`, `/user/...`, `/orgs/...`
- GraphQL queries (implicitly enabled)

This is the most permissive read-only configuration. The agent can browse any public/accessible GitHub data but cannot modify anything.

### Adding Write Access for Specific Repos

You can layer additional permissions on top of the global readonly:

```toml
[service-gator.gh.repos]
# Global read-only baseline
"*/*" = { read = true }

# Allow draft PRs for your main projects
"myorg/frontend" = { read = true, create-draft = true }
"myorg/backend" = { read = true, create-draft = true }
```

## Quick Start (CLI)

For one-off usage or overriding the config, use command-line flags:

```bash
# Read-only access to all GitHub repos
devaipod up . --service-gator=github:readonly-all

# Read access to specific repos
devaipod up . --service-gator=github:myorg/myrepo

# Read access to all repos in an org
devaipod up . --service-gator=github:myorg/*

# Write access to a specific repo
devaipod up . --service-gator=github:myorg/myrepo:write

# Multiple scopes
devaipod up . \
  --service-gator=github:myorg/frontend \
  --service-gator=github:myorg/backend:write
```

### Using a Custom service-gator Image

By default, devaipod pulls `ghcr.io/cgwalters/service-gator:latest`. To use a locally-built or custom image:

```bash
# Use a local development build
devaipod up . --service-gator=github:myorg/myrepo --service-gator-image localhost/service-gator:dev

# Use a specific version
devaipod up . --service-gator=github:myorg/myrepo --service-gator-image ghcr.io/cgwalters/service-gator:v0.2.0
```

This is useful for testing local changes to service-gator or pinning to a specific version.

### CLI Scope Format

```
--service-gator=SERVICE:TARGET[:PERMISSIONS]
```

- **SERVICE**: `github` (or `gh`), `gitlab` (future), `jira` (future)
- **TARGET**: Repository pattern like `owner/repo` or `owner/*`, or special keyword like `readonly-all`
- **PERMISSIONS**: Comma-separated list (default: `read`)
  - `read` - Read-only access
  - `create-draft` - Create draft PRs
  - `pending-review` - Manage pending PR reviews
  - `write` - Full write access

## Configuration File

For persistent configuration, use `~/.config/devaipod.toml`. Here's a complete example:

```toml
# Podman secrets for credentials - forwarded to workspace and gator containers
# but NOT to the agent container. Format: "ENV_VAR=secret_name"
# Create secrets with: echo 'token' | podman secret create secret_name -
[trusted]
secrets = ["GH_TOKEN=gh_token", "GITLAB_TOKEN=gitlab_token", "JIRA_API_TOKEN=jira_token"]

# RECOMMENDED: Global read-only access to all GitHub data
# Enables: all repos, /search, /gists, /user, GraphQL
[service-gator.gh]
read = true

# Optional: Add write permissions for specific repos
[service-gator.gh.repos]
# Read + create draft PRs for specific repos you actively develop
"myorg/main-project" = { create-draft = true }

# Read + manage pending PR reviews (for AI code review workflows)
"myorg/reviewed-repo" = { pending-review = true }

# Full write access (use sparingly - only for highly trusted workflows)
# "myorg/trusted-repo" = { write = true }

# PR-specific grants (typically set dynamically via CLI)
# [service-gator.gh.prs]
# "myorg/repo#42" = { write = true }

# JIRA project permissions (if you use JIRA)
# [service-gator.jira.projects]
# "MYPROJ" = { read = true, create = true }
```

Note: The `[service-gator] enabled = true` setting is optional - service-gator is auto-enabled when any scopes are configured.

### Trusted Environment Variables

The `[trusted.env]` section is critical for service-gator to work:

```toml
[trusted.env]
# These env vars are forwarded ONLY to workspace and gator containers
# The AI agent container does NOT receive these - it must go through service-gator
allowlist = ["GH_TOKEN", "GITLAB_TOKEN", "JIRA_API_TOKEN"]

# You can also set explicit values
[trusted.env.vars]
GH_TOKEN = "ghp_xxxxxxxxxxxx"
```

This ensures credentials are available to service-gator but not directly accessible by the AI agent.

### Podman Secrets (Recommended)

For better security, use podman secrets instead of environment variables. Secrets don't appear in `podman inspect` or process listings, and podman's `type=env` feature sets them directly as environment variables.

1. Create a podman secret:
   ```bash
   echo -n "ghp_xxxxxxxxxxxx" | podman secret create gh_token -
   ```

2. Configure `~/.config/devaipod.toml`:
   ```toml
   [trusted]
   # Use podman secrets with type=env (secrets become env vars directly)
   # Format: "ENV_VAR_NAME=secret_name"
   secrets = ["GH_TOKEN=gh_token", "GITLAB_TOKEN=gitlab_token"]
   ```

3. devaipod passes `--secret gh_token,type=env,target=GH_TOKEN` to podman. The `GH_TOKEN` environment variable is set directly from the secret value.

See [Secret Management](secrets.md) for more details on this approach.

## Permission Levels

### GitHub

| Permission | Description |
|------------|-------------|
| `read` | View PRs, issues, code, run status, etc. |
| `create-draft` | Create draft PRs only (safer for review workflows) |
| `pending-review` | Create, update, and delete pending PR reviews |
| `write` | Full access (merge, close, create non-draft PRs, etc.) |

### JIRA

| Permission | Description |
|------------|-------------|
| `read` | View issues, projects, search |
| `create` | Create new issues |
| `write` | Full access (update, transition, comment, etc.) |

## Pattern Matching

Repository patterns support trailing wildcards:
- `owner/repo` - Exact match
- `owner/*` - All repos under `owner`
- More specific patterns take precedence over wildcards

## How It Works

When you run `devaipod up`:

1. **devaipod parses** CLI `--service-gator` flags and merges with `~/.config/devaipod.toml`
2. **If service-gator is enabled**, devaipod creates a pod with:
   - **workspace container**: Full dev environment with trusted env vars (GH_TOKEN, etc.)
   - **gator container**: Runs `service-gator` with scopes and trusted env vars
   - **agent container**: Runs `opencode serve` with NO trusted env vars, configured to use gator MCP
3. **The agent** can use GitHub/JIRA tools via MCP, but only with the configured scopes
4. **Credentials never reach the agent** - they stay in the trusted containers

## Requirements

- `GH_TOKEN` must be configured via `[trusted.env]` in devaipod.toml or set in your environment
- For JIRA, `JIRA_API_TOKEN` should be in `[trusted.env]`

The service-gator container image (`ghcr.io/cgwalters/service-gator`) is automatically pulled.

## Security Benefits

1. **Credential Isolation**: API tokens are in workspace/gator containers only; the agent never sees them
2. **Container Separation**: Agent runs in a separate container with dropped capabilities
3. **Fine-grained Scoping**: Grant exactly the permissions needed via CLI or config
4. **MCP Protocol**: Agent communicates with external services only through the MCP interface

## See Also

- [Sandboxing Model](sandboxing.md) - Security model and container isolation
- [Secret Management](secrets.md) - Handling API keys and credentials
- [service-gator README](https://github.com/cgwalters/service-gator) - Full documentation
