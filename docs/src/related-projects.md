# Related Projects

The AI coding agent space is evolving rapidly. This page compares devaipod to related projects, with emphasis on licensing and cloud dependencies.

For broader context on the state of agentic AI coding tools, see [Thoughts on agentic AI coding as of Oct 2025](https://blog.verbum.org/2025/10/27/thoughts-on-agentic-ai-coding-as-of-oct-2025/).

## Comparison Table

| Project | License | Local-only? | Notes |
|---------|---------|-------------|-------|
| **devaipod** | Apache-2.0/MIT | Yes | No cloud services required |
| [Docker AI Sandboxes](https://docs.docker.com/ai/sandboxes/) | Proprietary | Yes | MicroVM isolation, Docker Desktop required |
| [nono](https://nono.sh/) | Apache-2.0 | Yes | OS-level sandboxing (Landlock/Seatbelt), agent-agnostic |
| [OpenHands](https://github.com/All-Hands-AI/OpenHands) | MIT | Yes | Self-hostable, Docker-based |
| [Ambient Code](https://github.com/ambient-code/platform) | MIT | Yes | Kubernetes-native, self-hosted |
| [Gastown](https://github.com/steveyegge/gastown) | MIT | Yes | Multi-agent orchestration, no sandboxing |
| [Auto-Claude](https://github.com/AndyMik90/Auto-Claude) | AGPL-3.0 | Yes | Desktop app, no sandboxing |
| [Continue](https://github.com/continuedev/continue) | Apache-2.0 | Partial | CLI is local; "Mission Control" cloud is proprietary |
| [SWE-agent](https://github.com/princeton-nlp/SWE-agent) | MIT | Partial | Core is open; [depends on Daytona cloud](https://www.daytona.io/dotfiles/langchain-s-open-swe-runs-on-daytona-here-s-why) for some features |
| [Ona](https://ona.com/) | Proprietary | No | Cloud service, not open source |
| [Cursor](https://cursor.sh/) | Proprietary | No | Commercial product |
| Claude Code Web | Proprietary | No | Anthropic-hosted, sandboxed but not open source |

## Basic Agent Frameworks

These are the "raw" agent tools that devaipod can wrap with sandboxing. They run directly on your machine with full access to your filesystem and credentials.

### Claude Code

[Claude Code](https://docs.anthropic.com/en/docs/claude-code) is Anthropic's official CLI agent. Proprietary, closed source. Runs locally but with no sandboxing—the agent has full access to your machine and any credentials in your environment.

### Gemini CLI

[Gemini CLI](https://github.com/google-gemini/gemini-cli) is Google's agent CLI. Apache-2.0 licensed.

Gemini CLI has a "sandbox" mode using Docker, but **the sandboxing is insufficient for security-conscious use**:

- The sandbox isolates filesystem access, but credentials (API keys, tokens) are still passed into the container environment
- There is no credential scoping—if you give the agent a GitHub token, it has full access to all repos that token can reach
- No network isolation beyond what Docker provides by default
- No fine-grained control over what the agent can do with external services
- No devcontainer.json support—you can't use your project's existing dev environment spec

devaipod addresses these gaps: the agent container has no direct access to your GitHub token; instead, all GitHub operations go through service-gator which enforces scopes (e.g., only draft PRs to a specific repo).

### Goose

[Goose](https://github.com/block/goose) from Block is an extensible AI agent with MCP (Model Context Protocol) support. Apache-2.0 licensed, fully open source, runs locally.

devaipod can run Goose as an agent backend (via `--agent goose`), though OpenCode is the primary tested agent.

### OpenCode

[OpenCode](https://github.com/anomalyco/opencode/) is the primary agent framework used by devaipod. Apache-2.0 licensed. It provides a TUI and a server mode that devaipod uses for sandboxed execution.

## Orchestration Platforms

### OpenHands

[OpenHands](https://github.com/All-Hands-AI/OpenHands) (formerly OpenDevin) is an open platform for AI software developers. It provides a web interface for managing agent sessions with Docker-based sandboxing. MIT licensed.

OpenHands is a more complete platform with its own web UI. devaipod focuses on CLI-first workflows, devcontainer.json compatibility, and fine-grained credential scoping via service-gator.

### Ambient Code Platform

[Ambient Code Platform](https://github.com/ambient-code/platform) is a Kubernetes-native platform for running AI coding agents. MIT licensed, self-hostable.

Ambient Code targets team/organization deployment on Kubernetes. devaipod targets individual developer workstations with zero infrastructure beyond podman. Both projects solve credential scoping—Ambient Code's broker architecture influenced devaipod's service-gator integration.

### Auto-Claude

[Auto-Claude](https://github.com/AndyMik90/Auto-Claude) is an autonomous multi-agent coding framework with a desktop UI, Kanban board, and parallel agent execution. AGPL-3.0 licensed.

Auto-Claude has excellent UI/UX but runs agents directly on the host with full system access—no sandboxing. devaipod could serve as a sandboxed backend for Auto-Claude's interface.

### Gastown

[Gastown](https://github.com/steveyegge/gastown) (from Steve Yegge) is a multi-agent orchestration system for Claude Code. MIT licensed, written in Go. It provides workspace management, agent coordination via "convoys", and persistent work tracking through git-backed "hooks" (git worktrees).

Gastown focuses on **orchestration** rather than **sandboxing**:

- No container isolation—agents run in tmux sessions with full host filesystem access
- No credential scoping—agents receive your full GitHub token, API keys, etc.
- Claude Code runs with `--dangerously-skip-permissions` by default
- No devcontainer.json support
- Isolation is via git worktrees (separate working directories) and prompt-based instructions to "stay in your worktree"

Gastown and devaipod solve different problems and could be complementary: Gastown for orchestrating work distribution across many agents, devaipod for sandboxing individual agent execution with credential scoping.

## Open Core (Partial Cloud Dependencies)

### Continue

[Continue](https://github.com/continuedev/continue) provides VS Code and JetBrains extensions, plus a CLI. The extensions and CLI are Apache-2.0.

**Cloud dependency**: "Mission Control" (hub.continue.dev) is Continue's proprietary cloud platform for running cloud agents. The backend code is not open source. Local CLI execution has no sandboxing.

### SWE-agent

[SWE-agent](https://github.com/princeton-nlp/SWE-agent) from Princeton NLP provides an agent-computer interface for software engineering tasks. MIT licensed.

**Cloud dependency**: The "Open SWE" product [runs on Daytona](https://www.daytona.io/dotfiles/langchain-s-open-swe-runs-on-daytona-here-s-why), a commercial cloud service for dev environments.

## Proprietary / Cloud-Required

### Ona

[Ona](https://ona.com/) is a commercial AI agent platform. **Requires cloud services**—there is no open source version or self-hosted option.

### Cursor

[Cursor](https://cursor.sh/) is a commercial AI-first code editor based on VS Code. Proprietary, cloud-connected.

### Claude Code Web

Claude Code is also available as a hosted web service at claude.ai. Anthropic runs it in their own sandboxed infrastructure with a git proxy for credential scoping (described in their [sandboxing blog post](https://www.anthropic.com/engineering/claude-code-sandboxing)). However, **that sandbox code is not open source**—you cannot run it yourself. If you want similar sandboxing locally, you need something like devaipod.

## Other Sandboxing Tools

### Docker AI Sandboxes

[Docker AI Sandboxes](https://docs.docker.com/ai/sandboxes/) is Docker's solution for running AI coding agents in isolated environments. It uses lightweight microVMs with private Docker daemons for each sandbox.

devaipod is just a wrapper for podman and uses the **devcontainer.json** standard.

Note that the use case of running containers *inside* the sandbox is captured via nested containerization: VMs are not required.

- **Licensing**: Docker Sandboxes is part of Docker Desktop, which is [proprietary software](https://www.docker.com/legal/docker-subscription-service-agreement/) requiring paid subscriptions for commercial use in organizations with 250+ employees or $10M+ revenue; devaipod is fully open source (Apache-2.0/MIT)
- **Platform**: Docker Sandboxes requires Docker Desktop with microVM support (macOS, Windows experimental); devaipod uses podman and works on Linux natively
- **Credential scoping**: Docker Sandboxes provides isolation but does not mention fine-grained credential scoping like service-gator; devaipod can limit agent access to specific repos/operations

### nono

[nono](https://nono.sh/) ([GitHub](https://github.com/lukehinds/nono)) is an OS-level sandboxing tool for AI agents. Apache-2.0 licensed, created by Luke Hinds (creator of Sigstore).

nono defaults to Landlock on Linux and Seatbelt on macOS. I think OCI containers provide more security and are more flexible and well understood by tools.
Further, containers provide reproducible environments that are just a foundational piece.

Landlock is complementary to containerization, but how nono is doing it is conceptually against what the Landlock creators
want in my opinion: Landlock was supposed to primarily used by apps to sandbox *themselves*, not as a container-replacement framework.

## Why devaipod?

1. **Fully open source**: Apache-2.0/MIT, no "open core" trap
2. **100% local**: No cloud services required (you bring your own LLM API keys)
3. **devcontainer.json**: Uses the standard spec, not custom formats
4. **Fine-grained credential scoping**: service-gator MCP provides scoped access (e.g., draft PRs only to specific repos)—not just filesystem sandboxing
5. **Podman-native**: Rootless containers, works in toolbox, no Docker daemon required

## Reusable Components

A design goal for devaipod is that its core components should be reusable building blocks, not a monolithic system. Projects like OpenHands, Ona, and Ambient Code are building centralized platforms for corporate/team agentic AI usage. We hope that a fully open source version of such a platform emerges, and when it does, components from devaipod should be useful:

- **service-gator**: Fine-grained credential scoping for GitHub/GitLab/Forgejo could plug into any orchestration system
- **Container sandboxing patterns**: The podman pod architecture with separate workspace/agent/gator containers
- **devcontainer.json integration**: Parsing and applying the devcontainer spec for agent environments

devaipod is designed for individual developers today, but the primitives should scale to team/org deployment when composed with appropriate orchestration.
