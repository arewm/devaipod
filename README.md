# devaipod

**Sandboxed AI coding agents in reproducible dev environments using podman pods**

Run AI agents with confidence: your code in a devcontainer, the agent in a separate container that only has limited access to the host system *and* limited network credentials (e.g. Github token).

## Quick Start

```bash
# Clone and build
git clone https://github.com/cgwalters/devaipod && cd devaipod
cargo install --path .

# First-time setup
devaipod init

# Start a workspace with agent
devaipod up /path/to/your/project -S

# Or run agent on a GitHub issue
devaipod run https://github.com/org/repo/issues/123
```

## Documentation

Full documentation is available at **[cgwalters.github.io/devaipod](https://cgwalters.github.io/devaipod)**

- [Quick Start](https://cgwalters.github.io/devaipod/quickstart.html)
- [Commands Reference](https://cgwalters.github.io/devaipod/commands.html)
- [Sandboxing Model](https://cgwalters.github.io/devaipod/sandboxing.html)
- [Configuration](https://cgwalters.github.io/devaipod/configuration.html)

## On the topic of AI

This tool is primarily authored by @cgwalters who would "un-invent" large language models if he could because he believes the long term negatives are likely to outweigh the gains. But since that's not possible, this project is about maximizing the positive aspects of LLMs with a focus on software production. We need to use LLMs safely and responsibly, with efficient human-in-the-loop controls and auditability.

If you want to use LLMs, but are terrified of e.g. [prompt injection](https://simonwillison.net/tags/prompt-injection/) attacks from un-sandboxed agent use especially with unbound access to your machine secrets (especially e.g. Github token): then devaipod can help you.

## How It Works

devaipod uses podman pods to create a multi-container environment:

```
┌───────────────────────────────────────────────────────────────────┐
│  Podman Pod                                                        │
│                                                                    │
│  ┌─────────────────────┐  ┌─────────────────────┐                 │
│  │ Workspace Container │  │ Agent Container     │                 │
│  │ • Full dev env      │  │ • opencode serve    │                 │
│  │ • Has GH_TOKEN      │  │ • Dropped caps      │                 │
│  │ • 'oc' shim         │  │ • No GH_TOKEN       │                 │
│  └─────────────────────┘  └─────────────────────┘                 │
│           │                         │                              │
│           └─────────────────────────┘                              │
│                   Shared workspace volume                          │
│                                                                    │
│  ┌─────────────────────┐                                          │
│  │ Gator Container     │  ← Scope-restricted GitHub/JIRA access   │
│  │ • service-gator MCP │                                          │
│  └─────────────────────┘                                          │
└───────────────────────────────────────────────────────────────────┘
```

The agent runs in an isolated container with dropped capabilities, no access to your credentials, and only communicates with external services through the scope-restricted [service-gator](https://github.com/cgwalters/service-gator) MCP server.

## Requirements

- **podman** (rootless works, including inside toolbox containers)
- An image with `opencode` installed (e.g., [devenv-debian](https://github.com/bootc-dev/devenv-debian))
- A `devcontainer.json` in your project

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and [AGENTS.md](AGENTS.md) for contribution guidelines.

## License

Apache-2.0 OR MIT
