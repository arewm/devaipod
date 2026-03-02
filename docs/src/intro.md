# devaipod

**Sandboxed AI coding agents in reproducible dev environments using podman pods**

Run AI agents with confidence: your code in a devcontainer, the agent in a separate container that only has limited access to the host system *and* limited network credentials (e.g. Github token).

Combines in an opinionated way:

- [OpenCode](https://github.com/anomalyco/opencode/) as agent framework
- [Podman](https://github.com/containers/podman/) for container isolation
- [Devcontainers](https://containers.dev/) as a specification mechanism
- [service-gator](https://github.com/cgwalters/service-gator) for fine-grained MCP access to GitHub/GitLab/Forgejo

## On the topic of AI

This tool is primarily authored by @cgwalters who would "un-invent" large language models if he could because he believes the long term negatives for society as a whole are likely to outweigh the gains. But since that's not possible, this project is about maximizing the positive aspects of LLMs with a focus on software production (but not exclusively). We need to use LLMs safely and responsibly, with efficient human-in-the-loop controls and auditability.

If you want to use LLMs, but have concerns about e.g. [prompt injection](https://simonwillison.net/tags/prompt-injection/) attacks from un-sandboxed agent use especially with unbound access to your machine secrets (especially e.g. Github token): then devaipod can help you.

To be clear, this project is itself extensively built with AI (mostly Claude Opus), but
the author reviews the output (to varying degrees) - it's not "vibe coded". The emphasis
of this project is more on making it easier to use AI in a sandboxed way, but of course
there's a spectrum here, and nothing stops one from using it for closer-to-vibe-coding
cases.

## How It Works

devaipod implements a subset of the devcontainer specification, and launches multiple containers
in a single pod when a task is created. At the current time, each task must have at least
one git repository.

0. `devaipod launch <git repository> <task>` is started (via web UI, TUI or CLI)
1. Creates a workspace volume and clones that repository into it
2. Creates a podman pod with multiple components (unsandboxed workspace, sandboxed agent, API pod)

Each devcontainer pod is isolated from each other by default, and from the host. pods only
have what you explictly provide via environment variables, bind mounts etc.
At the current time networking is unrestricted by default, but we aim to support restricting
it further.

## Requirements

- **podman** (rootless works, including inside toolbox containers)
- A devcontainer image with `opencode` and `git` installed (e.g., [devenv-debian](https://github.com/bootc-dev/devenv-debian))

## License

Apache-2.0 OR MIT
