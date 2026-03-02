# OpenCode Integration

## Overview

[OpenCode](https://github.com/anomalyco/opencode) is an open-source TUI for AI coding agents. devaipod runs OpenCode in a sandboxed agent container within a podman pod.

## Installation

OpenCode must be available in your devcontainer image. The `ghcr.io/bootc-dev/devenv-debian` base image comes with OpenCode pre-installed.

## Configuration

OpenCode is configured via `~/.config/opencode/opencode.json`. Set this up in your dotfiles:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "model": "google-vertex-anthropic/claude-sonnet-4-20250514"
}
```

### Supported Providers

| Provider | Model Example | Env Vars Needed |
|----------|---------------|-----------------|
| Vertex AI | `google-vertex-anthropic/claude-sonnet-4-20250514` | `GOOGLE_CLOUD_PROJECT` + gcloud ADC |
| Anthropic | `anthropic/claude-sonnet-4-20250514` | `ANTHROPIC_API_KEY` |
| Google Gemini | `google/gemini-2.0-flash` | `GEMINI_API_KEY` |
| OpenAI | `openai/gpt-4o` | `OPENAI_API_KEY` |

## Usage with devaipod

```bash
# Create workspace and get a shell
devaipod up https://github.com/org/repo -S
# Then run 'opencode-connect' inside the workspace to connect to the agent

# Create workspace with a task for the agent
devaipod up https://github.com/org/repo "fix the type errors in main.rs"

# Run agent on a GitHub issue (issue URL is parsed, default task: "Fix <url>")
devaipod run https://github.com/org/repo/issues/123

# Attach to the agent in a running workspace
devaipod attach myworkspace
```

## Architecture

devaipod uses a podman pod with multiple containers:

```
┌──────────────────────────────────────────────────────────────────────────┐
│  Podman Pod (shared network namespace)                                    │
│                                                                           │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐        │
│  │ Workspace         │  │ Agent             │  │ Pod-api Sidecar  │        │
│  │ • Full dev env    │  │ • opencode serve  │  │ • Serves opencode│        │
│  │ • opencode-connect│  │ • Port 4096       │  │   SPA (vendored) │        │
│  │ • Your dotfiles   │  │ • Isolated $HOME  │  │ • Proxies to     │        │
│  └──────────────────┘  └──────────────────┘  │   agent (4096)   │        │
│          │                       ▲             │ • Git/PTY APIs   │        │
│          │  attach (TUI) ────────┘             └────────┬─────────┘        │
│          │                                              │ port 8090        │
└──────────│──────────────────────────────────────────────│──────────────────┘
           │                                              │
    opencode-connect                          Control plane (:8080)
    (terminal attach)                         embeds via iframe
```

The primary interface is the **control plane web UI at :8080**, which manages pods and embeds each pod's agent view in an iframe. The pod-api sidecar serves the vendored opencode SPA and proxies API calls to `localhost:4096` within the pod. See [design.md](design.md) for details on the architecture.

The workspace container also has an `opencode-connect` shim that runs `opencode attach` for terminal-based access, automatically continuing any existing session. All containers share the same network namespace via the pod.

## Agent Support

Currently only OpenCode is supported as the AI agent. The agent container runs `opencode serve` and the workspace connects via `opencode attach`.
