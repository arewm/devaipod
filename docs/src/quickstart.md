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

## Automatic Service-gator for Remote URLs

When you start a pod from a remote URL (GitHub repo or PR), devaipod automatically enables [service-gator](https://github.com/cgwalters/service-gator) with **read + draft PR** permissions for that repository. This means the agent can:

- Read repository contents, issues, and PRs
- Create **draft** pull requests

This is a safe default: the agent can propose changes via draft PRs, but a human must review and mark them ready before they can be merged. No additional configuration needed.

```bash
# This automatically grants read + draft PR access to org/repo
devaipod up https://github.com/org/repo 'implement feature X'
```

To grant additional permissions or configure for local repos, see the [Security](sandboxing.md) section.

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
