# Related Projects

The AI coding agent space is evolving rapidly. This page compares devaipod to related projects, with emphasis on licensing and cloud dependencies.

For broader context on the state of agentic AI coding tools, see [Thoughts on agentic AI coding as of Oct 2025](https://blog.verbum.org/2025/10/27/thoughts-on-agentic-ai-coding-as-of-oct-2025/).

## Comparison Table

| Project | License | Local-only? | Notes |
|---------|---------|-------------|-------|
| **devaipod** | Apache-2.0/MIT | Yes | No cloud services required |
| [Docker AI Sandboxes](https://docs.docker.com/ai/sandboxes/) | Proprietary | Yes | MicroVM isolation, Docker Desktop required |
| [NVIDIA OpenShell](https://github.com/NVIDIA/OpenShell) | Apache-2.0 | Yes | Docker-based sandboxing with gateway control plane, Landlock/seccomp, policy-driven egress |
| [nono](https://nono.sh/) | Apache-2.0 | Yes | OS-level sandboxing (Landlock/Seatbelt), agent-agnostic |
| [OpenHands](https://github.com/All-Hands-AI/OpenHands) | MIT | Yes | Self-hostable, Docker-based |
| [Ambient Code](https://github.com/ambient-code/platform) | MIT | Yes | Kubernetes-native, self-hosted |
| [paude](https://github.com/bbrowning/paude) | MIT | Yes | Podman + OpenShift backends, agent-agnostic |
| [Kortex](https://github.com/kortex-hub/kortex) | Apache-2.0 | Yes | Desktop GUI, AI + container/K8s management, Goose integration |
| [Gastown](https://github.com/steveyegge/gastown) | MIT | Yes | Multi-agent orchestration, no sandboxing |
| [krunai](https://github.com/slp/krunai) | Apache-2.0 | Yes | MicroVM, but not container oriented |
| [Auto-Claude](https://github.com/AndyMik90/Auto-Claude) | AGPL-3.0 | Yes | Desktop app, no sandboxing |
| [Continue](https://github.com/continuedev/continue) | Apache-2.0 | Partial | CLI is local; "Mission Control" cloud is proprietary |
| [SWE-agent](https://github.com/princeton-nlp/SWE-agent) | MIT | Partial | Core is open; [depends on Daytona cloud](https://www.daytona.io/dotfiles/langchain-s-open-swe-runs-on-daytona-here-s-why) for some features |
| [Ona](https://ona.com/) | Proprietary | No | Cloud service, not open source |
| [Cursor](https://cursor.sh/) | Proprietary | No | Commercial product |
| Claude Code Web | Proprietary | No | Anthropic-hosted, sandboxed but not open source |

## Basic Agent Frameworks

These are the "raw" agent tools that devaipod can wrap with sandboxing. They run directly on your machine with full access to your filesystem and credentials.

### OpenCode

[OpenCode](https://github.com/anomalyco/opencode/) is the primary agent framework used by devaipod. Apache-2.0 licensed. It provides a TUI and a server mode that devaipod uses for sandboxed execution.

### Claude Code

[Claude Code](https://docs.anthropic.com/en/docs/claude-code) is Anthropic's official CLI agent. Proprietary, closed source.
Claude Code recently added [builtin sandboxing](https://code.claude.com/docs/en/sandboxing), but container-based isolation is stronger and provides a reproducible environment.

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

[Goose](https://github.com/block/goose) from Block is an extensible AI agent with MCP (Model Context Protocol) support. Apache-2.0 licensed, fully open source, runs locally without builtin sandboxing.

## Orchestration Platforms

### OpenHands

[OpenHands](https://github.com/All-Hands-AI/OpenHands) (formerly OpenDevin) is an open platform for AI software developers. It provides a web interface for managing agent sessions with Docker-based sandboxing. MIT licensed.

OpenHands is a more complete platform with its own web UI. devaipod focuses on CLI-first workflows, devcontainer.json compatibility, and fine-grained credential scoping via service-gator.

### Ambient Code Platform

[Ambient Code Platform](https://github.com/ambient-code/platform) is a Kubernetes-native platform for running AI coding agents. MIT licensed (except for Claude Code), self-hostable.

Ambient Code targets team/organization deployment on Kubernetes. devaipod targets individual developer workstations with zero infrastructure beyond podman. Both projects solve credential scoping—Ambient Code's broker architecture influenced devaipod's service-gator integration.

The devaipod project would like to align more with Ambient Code. A few things:

- [Podman support](https://github.com/ambient-code/platform/issues/431)
- [Image needs to be pluggable](https://github.com/ambient-code/platform/pull/364)
- It's possible to run locally with [minikube](https://minikube.sigs.k8s.io/) or [minc](https://github.com/minc-org/minc) in theory, but this adds some friction

### paude

Following is Assisted-by: OpenCode (Opus 4.5)

[paude](https://github.com/bbrowning/paude) is a Python CLI that runs AI coding agents (Claude Code, Cursor CLI, Gemini CLI) inside secure containers. MIT licensed. It has a pluggable backend architecture with both Podman and OpenShift implementations, making it the closest existing project to what devaipod is trying to do with Kubernetes support.

The OpenShift backend is particularly interesting as prior art for devaipod's [Kubernetes plans](../todo/kubernetes.md). paude's approach:

- Uses `oc` CLI (subprocess) rather than a native Kubernetes client library. devaipod plans to use kube-rs instead, avoiding subprocess overhead and output parsing.
- Creates StatefulSets (not bare Pods) for workspace lifecycle, with scale-to-zero for stop/start. devaipod's pod model maps more naturally to bare Pods since each workspace is a multi-container pod with a specific lifecycle.
- Uses `oc exec` stdin/stdout tunneling with git's `ext::` protocol for code sync -- the agent makes commits inside the pod, and `git pull` tunnels through `oc exec`. This sidesteps the port-forward fragility problem entirely. devaipod should consider this pattern for Model 3 (hybrid local/remote).
- Credentials go into a tmpfs `emptyDir` volume (RAM-only, never persisted), synced via `oc cp`. This is a stronger security posture than writing credentials to a PVC.
- Network egress filtering uses a squid proxy container for Podman and Kubernetes NetworkPolicy for OpenShift, similar in spirit to how devaipod isolates agent network access via service-gator -- though service-gator operates at the API level rather than the network level.

Key differences from devaipod: paude is agent-agnostic (wraps Claude Code, Cursor, Gemini CLI) while devaipod integrates deeply with OpenCode. paude has no devcontainer.json support and uses a single container per session rather than devaipod's multi-container pod (workspace + agent + gator + api). paude has no credential scoping equivalent to service-gator -- network-level filtering is a blunter instrument than API-level scoping.

The git-over-exec-tunnel pattern is worth stealing for devaipod's hybrid model. And paude's tmpfs credential storage is a good security practice that devaipod should adopt when running in Kubernetes.

### Kortex

(This section is 85% Opus 4.6+OpenCode research, only superficial human review)

[Kortex](https://github.com/kortex-hub/kortex) is an Electron/Svelte desktop application for AI-powered container and Kubernetes management. Apache-2.0 licensed, evolved from [Podman Desktop](https://github.com/podman-desktop/podman-desktop).

Kortex occupies a different niche than devaipod: rather than sandboxing AI agents, it provides a desktop GUI that integrates AI with container and Kubernetes management. It has a pluggable "flow provider" abstraction, with [Goose](https://github.com/block/goose) as the current implementation. Goose is downloaded and spawned as a CLI subprocess (`goose run --recipe <path>`); the flow provider interface is generic enough that other agents could be plugged in via extensions.

Interesting aspects of the Goose integration:

- **MCP passthrough**: When creating a flow, users select from MCP servers registered in Kortex. Credentials are retrieved from secure storage and embedded into the Goose recipe YAML as `extensions` with `streamable_http` URIs and auth headers. This is a form of credential management, though not scoped per-operation like service-gator.
- **GUI on top of Goose**: Kortex adds a full web UI for flow creation (with AI-assisted parameter extraction from prompts), execution (xterm.js terminal streaming Goose stdout/stderr), and Kubernetes deployment (generates Job + Secret + ConfigMap YAML).
- **K8s deployment**: Flows can be deployed as Kubernetes Jobs running a hardcoded `quay.io/kortex/goose` container image (built externally in [packit/ai-workflows](https://github.com/packit/ai-workflows/tree/main/goose-container)) with the recipe mounted via ConfigMap. The image is not user-configurable. The Job is minimal: single container, no sidecars, no resource limits, no security context.
- **Chat-to-flow export**: Users can export chat conversations (powered by inference providers like Gemini) into Goose recipes, bridging interactive AI chat with automated workflows.

Key differences from devaipod:

- **No agent sandboxing**: Goose runs locally as a bare `child_process.spawn()` with full host access. No container wrapping for local execution at all.
- **No devcontainer/devfile support**: Kortex has no concept of devcontainer.json or devfiles. The execution environment is either the host (local) or a hardcoded container image (K8s). Users cannot define or customize the runtime environment.
- **Hardcoded image**: The K8s deployment image (`quay.io/kortex/goose:2025-09-03`) is a compile-time constant with no user override. The image just contains the goose binary; there's nothing else special in it.
- **GUI-first vs CLI-first**: Desktop application vs terminal tool.
- **AI manages infrastructure**: Kortex uses AI to help manage containers/K8s; devaipod uses containers to sandbox AI that writes code.

The projects could be complementary: Kortex could manage the container/K8s infrastructure that devaipod pods run on. More concretely, Kortex's MCP integration means it could consume service-gator as a tool provider, which would add the credential scoping that Kortex currently lacks for its Goose integration.

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

### krunai

As far as I can see [krunai](https://github.com/slp/krunai) is really another virtual machine launcher, it doesn't truly do much special for AI workloads - or even arguably anything at all other than having an example init script that downloads a particular CLI tool.

I think what devaipod is doing using devcontainers make sense as a mechanism to allow users to control their workload environment, and there's already good tooling to optionally launch podman/kube containers wrapped in VMs if desired.

I also think in the general case one really wants good affordance for git integration, output review etc.

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

### NVIDIA OpenShell

(This section is Assisted-by: OpenCode (Claude Opus 4.6) research, but has been refined and edited)

On [NVIDIA OpenShell](https://github.com/NVIDIA/OpenShell) there's a lot of overlap. One obvious thing here is that it does a pretty wild thing in running k3s inside docker (which would probably also work with podman), whereas devaipod leans into the native support for podman pods. However there are also clear advantages to k3s-in-container, among them it makes it much easier to have symmetric support for a real remote Kuberentes cluster.

I think service-gator as MCP is a stonger/better solution than the generic REST proxy. We're coming at these things from a very similar space, but a key thing here with service-gator is that the tokens are not accessible to the agent at all.
OpenShell is the closest project to devaipod in goals: both sandbox AI agents with fine-grained controls rather than just filesystem isolation. Key similarities and differences:

- **Sandboxing approach**: OpenShell uses Landlock (kernel LSM) for filesystem restrictions plus seccomp for syscall filtering, layered inside Docker containers. devaipod uses OCI containers via podman with rootless execution. The author of devaipod thinks LandLock was not intended for what OpenShell or nono.sh are doing with it and it's mostly unnecessary.
- **Network control**: OpenShell intercepts all outbound connections via an HTTP CONNECT proxy that matches destination + calling binary against a declarative YAML policy. devaipod does not isolate network access by default (although one could configure some of that at the container networking level). service-gator is used by devaipod for safe credential-based access to specific services, but it could also be used as an MCP server in OpenShell.
- **Credential management**: OpenShell uses "providers" — named credential bundles injected as environment variables at sandbox creation. Credentials are injected at runtime and never written to the sandbox filesystem. devaipod uses service-gator to avoid passing credentials to the agent at all — the agent never sees the GitHub token, it only gets scoped MCP tool access. This is a stronger isolation model for the services service-gator supports.
- **Architecture**: OpenShell runs a K3s cluster inside Docker and uses a gateway/sandbox control-plane model. This is heavier than devaipod's podman pod approach (no Kubernetes layer), but positions OpenShell better for multi-tenant and remote deployment (it already supports local, remote via SSH, and cloud gateway modes).
- **Agent support**: OpenShell is agent-agnostic — it wraps Claude Code, OpenCode, Codex, OpenClaw, and Ollama. devaipod integrates deeply with OpenCode at the moment, but supporting other agent types is a possibility.
- **Inference routing**: OpenShell has a built-in privacy router that intercepts LLM API calls and can redirect them to local or self-hosted backends, stripping/replacing credentials. devaipod has no equivalent — inference routing is handled by the agent's own configuration.
- **devcontainer.json**: devaipod uses the devcontainer.json standard for defining the agent environment. OpenShell uses community sandbox images and supports BYOC (bring your own container) but has no devcontainer.json integration.
- **git support**: Devaipod aims to have strong, native support for git, but I don't see this in OpenShell
- **Platform**: OpenShell requires Docker. devaipod uses podman (but could also pretty easily use docker). It is also a goal to support targeting Kubernetes.

The projects share the same fundamental insight that sandboxing AI agents requires more than filesystem isolation — you need network egress control, credential scoping, and defense-in-depth.

In a nutshell, I am considering:

- Rebasing devaipod on OpenShell
- Trying to contribute service-gator to that project

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
