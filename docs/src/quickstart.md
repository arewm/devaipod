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

Monitor progress with `devaipod attach <workspace>` which connects to the agent.

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

You can also use `devaipod` as a basic wrapper for a devcontainer with an attached
agent that is idle by default.

```bash
devaipod up https://github.com/org/repo
# Attach to the agent when ready
devaipod attach <workspace>
# Or get a shell in agent container
devaipod exec <workspace>
# Or get a shell in workspace container for manual work
devaipod exec <workspace> -W
```

## Next Steps

- [Commands Reference](commands.md) - Full list of available commands
- [Configuration](configuration.md) - Customize devaipod behavior
- [Sandboxing Model](sandboxing.md) - Understand the security model
