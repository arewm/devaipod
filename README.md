# devaipod

A fine-grained sandboxing tool for agentic AI that can run 100% locally. No "open core" here, no cloud services except those you configure.

Combines in an opinionated way:

- [OpenCode](https://github.com/anomalyco/opencode/) as agent framework
- [Podman](https://github.com/containers/podman/) as container isolation
- [Devcontainers](https://containers.dev/) as a specification mechanism
- [service-gator](https://github.com/cgwalters/service-gator) as fine-grained MCP server for Github/Gitlab/Forgejo/etc

To be clear: this tool is primarily designed by @cgwalters who would "un-invent" large language models if he could because he believes the long term negatives for society are likely to outweigh the gains. But since that's not possible, this project is about maximizing the positive aspects of LLMs with a focus on software production. We need to use LLMs safely and responsibly, with efficient human-in-the-loop controls and auditability.

However, @cgwalters uses LLMs every day. If you use LLMs or want to, but have heard of e.g. [prompt injection](https://simonwillison.net/tags/prompt-injection/) attacks and share similar concerns from un-sandboxed agent use, then devaipod can help you, as it does the author.

## Documentation

Full documentation including quick start is available at **[cgwalters.github.io/devaipod](https://cgwalters.github.io/devaipod)**

## Related Projects

- [OpenHands](https://github.com/All-Hands-AI/OpenHands) - Open platform for AI software developers as generalist agents
- [SWE-agent](https://github.com/princeton-nlp/SWE-agent) - Agent-computer interface for software engineering tasks (from Princeton NLP)
- [Auto-Claude](https://github.com/siddicky/auto-claude) - Autonomous Claude with sandboxed execution
- [Ambient Code](https://ambient.run/) - AI coding assistant with local execution
- [Ona](https://github.com/synthetic-selves/ona) - Open source AI agent framework
- [Gastown](https://github.com/gastown-ai/gastown) - Sandboxed AI coding environments

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and [AGENTS.md](AGENTS.md) for contribution guidelines.

## License

Apache-2.0 OR MIT
