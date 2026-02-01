# Related Projects

The AI coding agent space is evolving rapidly. This page provides an overview of related projects and how devaipod relates to them. For some projects we have detailed comparison documents.

## Platforms and Orchestrators

### OpenHands

[OpenHands](https://github.com/All-Hands-AI/OpenHands) (formerly OpenDevin) is an open platform for AI software developers as generalist agents. It provides a web interface for managing agent sessions with Docker-based sandboxing.

**Relationship to devaipod**: OpenHands is a more complete platform with its own web UI, while devaipod focuses on CLI-first workflows and devcontainer.json compatibility. OpenHands targets teams wanting a managed experience; devaipod targets developers who want fine-grained control over sandboxing.

### Ambient Code Platform

[Ambient Code Platform](https://github.com/ambient-code/platform) is a Kubernetes-native platform for running AI coding agents ("virtual teams"). It provides orchestration, credential management, and issue-driven workflows.

**Relationship to devaipod**: Ambient Code targets team/organization deployment on Kubernetes with enterprise features. devaipod targets individual developer workstations with zero infrastructure beyond a container runtime. Both projects are solving credential scoping, and Ambient Code's broker architecture influenced devaipod's service-gator integration.

See [detailed comparison](relationship-ambient-code.md) for architecture analysis.

### SWE-agent

[SWE-agent](https://github.com/princeton-nlp/SWE-agent) from Princeton NLP provides an agent-computer interface designed for software engineering tasks. It focuses on the agent's ability to navigate and modify codebases.

**Relationship to devaipod**: SWE-agent is primarily a research project exploring how agents interact with code. devaipod could potentially use SWE-agent as an agent backend, though we currently focus on OpenCode.

## Agent Frameworks

### Continue

[Continue](https://github.com/continuedev/continue) is an open-source AI coding assistant with VS Code and JetBrains extensions, plus a CLI and cloud platform (Mission Control).

**Relationship to devaipod**: Continue's cloud agent features are proprietary (Mission Control). Continue's local CLI has no sandboxing. devaipod provides the sandboxing layer that Continue's local mode lacks.

See [detailed comparison](relationship-continue.md) for architecture analysis.

### Auto-Claude

[Auto-Claude](https://github.com/AndyMik90/Auto-Claude) is an autonomous multi-agent coding framework with a desktop UI, visual task management (Kanban), and parallel agent execution.

**Relationship to devaipod**: Auto-Claude has excellent UI/UX but runs agents directly on the host with full system access. devaipod could serve as a sandboxed backend for Auto-Claude's interface.

See [detailed comparison](relationship-auto-claude.md) for integration analysis.

### Ona

[Ona](https://github.com/synthetic-selves/ona) is an open source AI agent framework focused on autonomous task execution.

**Relationship to devaipod**: Ona is an agent framework; devaipod is agent-agnostic sandboxing infrastructure. devaipod could potentially run Ona agents in isolated containers.

### Goose

[Goose](https://github.com/block/goose) from Block is an extensible AI agent that can take actions on your behalf. It supports MCP (Model Context Protocol) for tool integration.

**Relationship to devaipod**: Goose is one of the agents devaipod can run (via `--agent goose`), though OpenCode is the primary tested agent. Goose is used in Aipproval-Forge.

## Commercial/Hybrid Products

### Cursor

[Cursor](https://cursor.sh/) is a commercial AI-first code editor based on VS Code. It provides deep IDE integration with AI assistance.

**Relationship to devaipod**: Cursor is a monolithic commercial product. devaipod is open infrastructure for composing your own workflows.

### Gastown

[Gastown](https://github.com/gastown-ai/gastown) provides sandboxed AI coding environments with a focus on isolation.

**Relationship to devaipod**: Similar goals around sandboxing, different implementation approaches.

## Key Differentiators

What makes devaipod unique:

1. **devcontainer.json first**: Uses the standard devcontainer spec rather than custom formats
2. **Fine-grained credential scoping**: service-gator MCP provides scoped access (e.g., draft PRs only)
3. **Fully open source**: No "open core" with proprietary cloud features
4. **Local-first**: Runs 100% on your machine with podman
5. **Mid-level infrastructure**: Provides primitives for building workflows, not a monolithic platform
