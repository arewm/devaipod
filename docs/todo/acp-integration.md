# ACP Integration Design

## Overview

Devaipod uses the Agent Client Protocol (ACP) as its sole agent
transport. Any ACP-compatible coding agent works by configuring a
profile in `devaipod.toml`. Devaipod does not impose a base image
or inject agent binaries — the user's devcontainer image must have
the agent installed.

## Architecture

Pod-api and the agent run in separate containers within the same pod.
Pod-api tunnels ACP over stdio via `podman exec -i` into the agent
container, matching the pattern used by `devaipod harvest` for git
transport:

```
pod-api (sidecar)  ──podman exec -i──►  agent container
   │  JSON-RPC 2.0 over stdin/stdout       │
   │                                        │ opencode acp
   ▼                                        │ goose acp
 WebSocket ◄── frontend (SolidJS)          │ claude ...
```

The agent container's entrypoint is a keep-alive loop. Pod-api
starts the ACP process on demand. Readiness is determined by the
ACP `initialize` handshake, not HTTP health checks.

## Agent Profiles

User config in `~/.config/devaipod.toml`:

```toml
[agent]
default = "opencode"

[agent.profiles.opencode]
command = ["opencode", "acp"]
env = { OPENCODE_CONFIG = "~/.config/devaipod/agents/opencode/config.json" }

[agent.profiles.goose]
command = ["goose", "acp"]
env = { GOOSE_MODE = "auto", GOOSE_CONFIG_DIR = "~/.config/devaipod/agents/goose" }

[agent.profiles.claude-code]
command = ["claude", "--dangerously-skip-permissions"]
env = { CLAUDE_CONFIG_DIR = "~/.config/devaipod/agents/claude" }
```

Resolution order: CLI `--agent` flag → config `[agent].default` →
hardcoded `["opencode", "acp"]`.

Agent-specific config (auto-approve, model, MCP servers) lives in
the user's dotfiles, pointed to by env vars. Devaipod never writes
agent config files. Tool permissions are managed by the agent's own
config, not the frontend.

## Protocol Support

Uses the `agent-client-protocol-schema` crate for ACP types. Custom
`Send`-compatible JSON-RPC client (`acp_client.rs`) handles the
transport since the upstream `ClientSideConnection` has `!Send`
futures incompatible with axum.

Implemented methods:
- `initialize` / `initialized`
- `session/new`, `session/list`, `session/load`
- `session/prompt` (fire-and-forget, streams events in real-time)
- `session/cancel`
- `session/request_permission` (auto-approve by default)

The `session/prompt` method returns immediately — a background task
handles the JSON-RPC response while `session/update` notifications
stream to the frontend as the agent works.

## Generic Framework

Tests validate agent-agnosticism with two mock agents that differ in
slash commands, tool kinds, and event patterns. Both use the same
client, WebSocket endpoint, and config with zero agent-specific code
outside the profile definition.

## Future Work

- **Git worktrees per session**: Separate worktree per ACP session
  so parallel sessions don't conflict. ACP's `session/new` accepts
  a `cwd` parameter for this.

- **MCP-over-ACP**: When the RFD stabilizes, inject service-gator
  through the ACP channel instead of per-agent MCP config.

- **Native agent UI**: Optional `native_ui` in profiles for agents
  with their own web UI (OpenCode, Goose), served via iframe.

## References

- [ACP specification](https://agentclientprotocol.com/protocol/overview)
- [ACP tool calls](https://agentclientprotocol.com/protocol/tool-calls)
- [MCP-over-ACP RFD](https://agentclientprotocol.com/rfds/mcp-over-acp)
- [agent-client-protocol crate](https://crates.io/crates/agent-client-protocol)
