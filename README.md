# devaipod

A fine-grained sandboxing tool for agentic AI that can run 100% locally. No "open core" here, no cloud services except those you configure.

Combines in an opinionated way:

- [OpenCode](https://github.com/anomalyco/opencode/) as agent framework
- [Podman](https://github.com/containers/podman/) as container isolation
- [Devcontainers](https://containers.dev/) as a specification mechanism
- [service-gator](https://github.com/cgwalters/service-gator) as fine-grained MCP server for Github/Gitlab/Forgejo/etc

This tool is primarily designed by @cgwalters who would "un-invent" large language models if he could because he believes the long term negatives are likely to outweigh the gains. But since that's not possible, this project is about maximizing the positive aspects of LLMs with a focus on software production. We need to use LLMs safely and responsibly, with efficient human-in-the-loop controls and auditability.

If you want to use LLMs, but are terrified of e.g. [prompt injection](https://simonwillison.net/tags/prompt-injection/) attacks from un-sandboxed agent use especially with unbound access to your machine secrets (especially e.g. Github token): then devaipod can help you.

## Quick Start

```bash
# Clone and build
git clone https://github.com/cgwalters/devaipod && cd devaipod
cargo install --path .

# First-time setup
devaipod init

# Run agent on a GitHub issue
devaipod run https://github.com/org/repo/issues/123
```

When you do this, the default service-gator configuration *only* allows editing that issue and creation of a draft pull request.

## Documentation

Full documentation is available at **[cgwalters.github.io/devaipod](https://cgwalters.github.io/devaipod)**

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and [AGENTS.md](AGENTS.md) for contribution guidelines.

## License

Apache-2.0 OR MIT
