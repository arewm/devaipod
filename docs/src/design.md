# Design Philosophy

## Mid-Level Infrastructure

devaipod is designed as **mid-level infrastructure** for AI coding workflows.

**More opinionated than raw tools**: Unlike running opencode or Claude Code directly, devaipod provides structure around sandboxing, credential isolation, and workspace lifecycle. You don't have to figure out container security yourself.

**Less opinionated than full platforms**: Unlike monolithic solutions (OpenHands Cloud, Cursor), devaipod focuses on the primitives and leaves room for building different workflows on top. Want a web UI? Build one that talks to our pods. Prefer a TUI? That works too.

**Composable building blocks**: The pod abstraction, service-gator MCP, and network isolation are independent pieces. Use what you need, skip what you don't.

This design enables:

- Custom control planes (web UI, TUI, or API-driven)
- Integration with existing CI/CD and review workflows
- Different human-in-the-loop patterns for different teams
- Extension via MCP servers and external tooling

## Security First

The fundamental design principle is that AI agents should have minimal access to credentials and external services. Rather than trusting the agent with your GitHub token, devaipod:

1. Runs the agent in an isolated container with dropped capabilities
2. Routes external service access through [service-gator](service-gator.md), which enforces fine-grained scopes
3. By default, only allows the agent to read repositories and create *draft* pull requests

This means a prompt injection attack or misbehaving agent cannot:

- Push directly to your repositories
- Access other repositories you have access to
- Merge pull requests
- Create non-draft PRs (which could trigger CI in surprising ways)

## Human-in-the-Loop

devaipod is built for workflows where humans review AI-generated code before it becomes permanent. The default permissions (read + draft PR) reflect this: the agent can propose changes, but a human must mark them ready for merge.

This isn't about distrusting AI capabilities—it's about maintaining auditability and preventing automation failures from having outsized impact.
