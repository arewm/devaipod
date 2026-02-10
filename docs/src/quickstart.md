# Quick Start

## Installation

```bash
# Clone and build
git clone https://github.com/cgwalters/devaipod && cd devaipod
cargo build --release

# Install to ~/.cargo/bin
cargo install --path .

# First-time setup (configures API keys, GitHub token, etc.)
devaipod init
```

## Example 1: Automated task execution

The recommended default is to use `run` which prompts for a task and starts the agent immediately. Service-gator is auto-configured with read + draft PR permissions.

```bash
# Interactive prompt for task:
devaipod run https://github.com/org/repo

# Or pass task inline:
devaipod run https://github.com/org/repo -c 'fix typos in README.md'

# From an issue URL (extracts repo, default task is "Fix <issue_url>"):
devaipod run https://github.com/org/repo/issues/123
```

Monitor progress with `devaipod attach <workspace>` which connects to the task owner agent. Use `devaipod attach <workspace> --worker` to connect to the worker agent.

See below for other verbs.

## Service-gator: GitHub Access for the Agent

[service-gator](service-gator.md) provides scope-controlled GitHub access (read PRs/issues, create drafts, etc.) to the AI agent without exposing your `GH_TOKEN` directly.

**Automatic for GitHub URLs:** When you run `devaipod up https://github.com/...` or `devaipod run https://github.com/.../pull/123`, service-gator is auto-enabled with **read + draft PR** permissions for that repository.

**Recommended: Global read-only config.** For local repos (`devaipod up .`) and broader access, first create a podman secret (`echo 'ghp_...' | podman secret create gh_token -`), then add to `~/.config/devaipod.toml`:

```toml
[trusted]
secrets = ["GH_TOKEN=gh_token"]

[service-gator.gh]
read = true
```

This gives all pods read-only access to all GitHub data (repos, search, gists, GraphQL). See [Service-gator Integration](service-gator.md) for write permissions and advanced configuration.

## Example 2: Manual control

You can also use `devaipod` as a basic wrapper for a devcontainer with attached
agents (task owner and worker) that are idle by default.

```bash
devaipod up https://github.com/org/repo
# Attach to the task owner agent when ready
devaipod attach <workspace>
# Attach to the worker agent
devaipod attach <workspace> --worker
# Or get a shell in task owner container
devaipod exec <workspace>
# Or get a shell in workspace container for manual work
devaipod exec <workspace> -W
```

## Editor integration via SSH

Each devaipod workspace runs an embedded SSH server, allowing you to connect with editors that support SSH remoting (Zed, VSCode, Cursor, etc.). This lets you interrupt an autonomous task and take manual control of the codebase.

```bash
# Generate SSH config entries for your workspaces
devaipod ssh-config >> ~/.ssh/config

# Then open in your editor:
# Zed: zed ssh://devaipod-<workspace>
# VSCode: code --remote ssh-remote+devaipod-<workspace> /workspaces/<project>
```

The SSH connection goes to the workspace container, which has full access to credentials (GH_TOKEN, etc.) for manual development work. You can review agent changes, make edits, run tests, and push commits directly.

## Next Steps

- [Commands Reference](commands.md) - Full list of available commands
- [Configuration](configuration.md) - Customize devaipod behavior
- [Sandboxing Model](sandboxing.md) - Understand the security model
