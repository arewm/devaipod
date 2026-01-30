# devaipod

**Sandboxed AI coding agents in reproducible dev environments using podman pods**

Run AI agents with confidence: your code in a devcontainer, the agent in a separate container that only
has limited access to the host system *and* limited network credentials (e.g. Github token).

## On the topic of AI

Note: This tool is primarily authored by @cgwalters who would "un-invent" large language models if he could because
he believes the long term negatives are likely to outweigh the gains. But since that's not possible, this project
is about maximizing the positive aspects of LLMs with a focus on software production (but not exclusively).
We need use LLMs safely and responsibly, with efficient human-in-the-loop controls and auditability.

If you want to use LLMs, but are terrified of e.g. [prompt injection](https://simonwillison.net/tags/prompt-injection/)
attacks from un-sandboxed agent use especially with unbound access to your machine secrets (espcially e.g. Github token): then devaipod can help you.

## How It Works

devaipod uses podman pods to create a multi-container environment:

1. Parses your project's `devcontainer.json` to determine the image
2. Creates a podman pod with shared network namespace
3. Starts containers:
   - **workspace**: Your development environment with `oc` and `opencode-agent` shims
   - **agent**: Runs `opencode serve` with security restrictions (dropped capabilities, no-new-privileges)
   - **gator**: The [service-gator](https://github.com/cgwalters/service-gator) MCP server for controlled access to GitHub/JIRA

All containers share the same network namespace, allowing localhost communication between the agent and workspace.

## Requirements

- **podman** (rootless works, including inside toolbox containers)
- An image with `opencode` installed (e.g., [devenv-debian](https://github.com/cgwalters/devenv-debian))
- A `devcontainer.json` in your project (`.devcontainer/devcontainer.json` or `.devcontainer.json`)

## Quick Start

```bash
# Clone and build
git clone https://github.com/cgwalters/devaipod && cd devaipod
cargo build --release

# First-time setup (configures API keys, GitHub token, etc.)
devaipod init
```

### Example 1: Interactive workspace with agent on standby

Start a pod for a local project. An agent will be available but waits for your instructions:

```bash
devaipod up /path/to/your/project
# After pod is ready, SSH in:
devaipod ssh myproject-abc123        # Shows agent monitor
# Or go straight to shell:
devaipod ssh myproject-abc123 bash
opencode-connect                     # Connect to agent from inside the pod
```

This allows you complete control over when and how the agent works.

### Example 2: Automated task execution

For more automation, use `run` which prompts for a task and starts the agent immediately.
Service-gator is auto-configured with read + draft PR permissions.

```bash
# Interactive prompt for task:
devaipod run https://github.com/org/repo

# Or pass task inline:
devaipod run https://github.com/org/repo -c 'fix typos in README.md'

# From an issue URL (extracts repo, default task is "Fix <issue_url>"):
devaipod run https://github.com/org/repo/issues/123
```

Monitor progress with `devaipod ssh <workspace>` which shows the agent monitor.
Press Ctrl-C to drop to an interactive shell where you can interrupt or guide the agent.

### Automatic Service-gator for Remote URLs

When you start a pod from a remote URL (GitHub repo or PR), devaipod automatically
enables [service-gator](https://github.com/cgwalters/service-gator) with **read + draft PR** permissions
for that repository. This means the agent can:

- Read repository contents, issues, and PRs
- Create **draft** pull requests (not regular PRs)

This is a safe default: the agent can propose changes via draft PRs, but a human must
review and mark them ready before they can be merged. No additional configuration needed.

```bash
# This automatically grants read + draft PR access to org/repo
devaipod up https://github.com/org/repo 'implement feature X'
```

To grant additional permissions or configure for local repos, see the [Security](#security) section.

## Commands

```bash
# First-time setup
devaipod init                     # Interactive setup wizard for API keys & tokens

# Workspace lifecycle
devaipod up .                     # Create pod with workspace + agent containers
devaipod up . -S                  # Create and SSH into workspace
devaipod up . "fix the bug"       # Create with task description for agent
devaipod list                     # List devaipod workspaces
devaipod status myworkspace       # Show detailed status of a pod
devaipod logs myworkspace         # View container logs (-c agent for agent logs)
devaipod stop myworkspace         # Stop a pod
devaipod delete myworkspace       # Delete a pod
devaipod up . --dry-run           # Show what would be created

# Connecting to workspaces
devaipod ssh myworkspace          # SSH into workspace (shows agent monitor)
devaipod ssh myworkspace bash     # SSH directly to shell
devaipod ssh-config myworkspace   # Generate SSH config for editor integration

# Running agents with tasks
devaipod run . 'fix typos'                          # Run on local repo
devaipod run https://github.com/org/repo            # Prompts for task interactively
devaipod run https://github.com/org/repo -c 'task'  # Task via flag
devaipod run https://github.com/org/repo/issues/42  # Issue URL: default task "Fix <url>"

# Shell completions
devaipod completions bash         # Generate bash completions
```

Note: The `devaipod-` prefix is optional for workspace names.

### Editor Integration (WIP)

The `ssh-config` command outputs an SSH config entry to stdout:
```bash
devaipod ssh-config my-pod >> ~/.ssh/config
```

**Note**: Full SSH support for VSCode/Zed Remote SSH requires an SSH server in
the container (currently not implemented). For now, use VSCode's Dev Containers
extension or the CLI workflow.

## Key Features

- **Native podman** - no devpod dependency for core workflow
- **Sandboxed agent** - agent container runs with dropped capabilities, no-new-privileges
- **Task kickoff** - give the agent a task and it starts working immediately
- **Auto service-gator** - remote URLs automatically get read + draft PR permissions
- **Workspace shims** - `oc` and `opencode-agent` commands run `opencode attach http://localhost:4096`
- **API keys from environment** - agent receives `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, etc.
- **Network isolation** - optionally restrict agent to allowed LLM API domains via proxy
- **Env allowlist** - per-project env vars in devcontainer.json customizations
- **Toolbox compatible** - works inside toolbox containers
- **macOS support** - works with podman machine on macOS

## Security

The agent container runs with restricted privileges:
- Drops all capabilities except `NET_BIND_SERVICE`
- Sets `no-new-privileges`
- Uses an isolated home directory (`/tmp/agent-home`)
- Has read/write access only to the workspace

The workspace container retains normal privileges for development tasks.

### Service-gator for GitHub Access

When you start a pod from a **remote URL** (GitHub repo or PR), service-gator is
automatically enabled with **read + draft PR** permissions for that repository.
This is the safe default: the agent can read code and create draft PRs, but cannot
merge or create regular PRs.

For local repositories or to customize permissions:

```bash
# Grant the agent read-only access to all GitHub repos
devaipod up . --service-gator=github:readonly-all

# Grant read access to specific repos only
devaipod up . --service-gator=github:myorg/myrepo

# Grant draft PR permissions (recommended for agent-created PRs)
devaipod up . --service-gator=github:myorg/myrepo:read,create-draft
```

Credentials like `GH_TOKEN` are forwarded only to trusted containers (workspace, gator), never to the agent. For better security, use [podman secrets](docs/secrets.md#podman-secrets-recommended) instead of environment variables—secrets are mounted as files and don't appear in `podman inspect` or process listings.

See [Service-gator Integration](docs/service-gator.md) for full details.

### Network Isolation

When enabled, agent network access is restricted to allowed LLM API endpoints via an HTTPS proxy:

```toml
# ~/.config/devaipod.toml
[network-isolation]
enabled = true
allowed_domains = ["api.custom.com"]  # Additional domains (LLM APIs allowed by default)
```

### Global Environment Variables

Configure environment variables to inject into all containers (workspace + agent) in `~/.config/devaipod.toml`:

```toml
[env]
# Forward these from host environment (if they exist)
allowlist = ["GOOGLE_CLOUD_PROJECT", "SSH_AUTH_SOCK", "VERTEX_LOCATION"]

# Set these explicitly
[env.vars]
VERTEX_LOCATION = "global"
EDITOR = "vim"
```

This is useful for cloud provider credentials, editor preferences, and other env vars needed in both containers.

### Per-Project Environment Variables

Projects can specify additional env vars to pass to the agent in devcontainer.json:

```json
{
  "customizations": {
    "devaipod": {
      "envAllowlist": ["MY_API_KEY", "CUSTOM_TOKEN"]
    }
  }
}
```

## Status

| Feature | Status |
|---------|--------|
| Native podman commands | ✅ Working |
| Agent container isolation | ✅ Working |
| Task kickoff (`up URL 'task'`) | ✅ Working |
| Auto service-gator (draft PRs) | ✅ Working |
| devcontainer.json parsing | ✅ Working |
| Dockerfile builds | ✅ Working |
| Lifecycle commands | ✅ Working |
| service-gator integration | ✅ Auto for remote URLs |
| Network isolation | ✅ Optional (proxy-based) |
| Env allowlist | ✅ Working |
| GPU passthrough | ✅ Optional (NVIDIA/AMD) |
| PR/MR URL support | ✅ Working |
| Remote git URLs | ✅ Working |
| macOS (podman machine) | ✅ Working |

## Documentation

- [Sandboxing Model](docs/sandboxing.md) - Security model details
- [Secret Management](docs/secrets.md) - Handling API keys and credentials
- [OpenCode Agent](docs/opencode.md) - Configuring the AI agent
- [Service-gator Integration](docs/service-gator.md) - Scope-restricted access to GitHub/JIRA

## License

Apache-2.0 OR MIT
