# devaipod Roadmap

Priorities may shift based on user feedback and practical experience.

## In Progress / Near-term

- **Agent completion detection**: For `run` mode, detect when agent reaches idle state and has completed its task
- **Git state awareness**: Detect and warn about unpushed commits in the workspace
- **SSH server for editor connections**: VSCode/Zed Remote SSH needs an actual SSH server in the container
- **Agent readiness probes**: Detect when agent container is actually ready to accept connections
- **Agent container image strategy**: Options for opencode installation (dedicated image, runtime install, sidecar)

## Future / Ideas

Larger features under consideration:

- **Network isolation**: Configure podman-level network settings to restrict agent network access
- **LLM credential isolation**: Proxy container (possibly service-gator) that holds LLM API keys, so the agent doesn't have direct credential access
- **Kubernetes support**: Use kube-rs to create pods on real Kubernetes clusters for remote dev environments
- **Quadlet/systemd integration**: Generate Quadlet units for proper lifecycle management
- **Local Forgejo instance**: Git caching, local CI/CD, and code review UI (see [forgejo-integration.md](../todo/forgejo-integration.md))
- **Nested devaipods**: MCP tool allowing agents to spawn additional sandboxed environments
- **Worker orchestration API**: MCP tools or OpenCode skill for task owner to programmatically assign subtasks to worker (see [worker-orchestration-api.md](../todo/worker-orchestration-api.md))
- **Devcontainer features support**: Install devcontainer features into the workspace image
- **Multi-project workspaces**: Support for monorepos or multi-repo setups
- **Persistent agent state**: Named volumes for agent home so context persists across pod restarts
- **Bot/assistant accounts**: OAuth2 apps with "on behalf of" authentication instead of PATs

## Known Limitations

- **Agent requires opencode in the image**: The agent container runs `opencode serve`, so opencode must be installed in the devcontainer image
- **Lifecycle commands only run in workspace**: onCreateCommand etc. run in the workspace container, not the agent container
- **Single agent type**: Only opencode is currently tested
