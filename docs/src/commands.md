# Commands

## First-time setup

```bash
devaipod init                     # Interactive setup wizard for API keys & tokens
```

## Workspace lifecycle

```bash
devaipod up .                     # Create pod with workspace + agent containers
devaipod up . -S                  # Create and SSH into workspace shell
devaipod up . "fix the bug"       # Create with task description for agent
devaipod list                     # List devaipod workspaces
devaipod status myworkspace       # Show detailed status of a pod
devaipod debug myworkspace        # Diagnose issues (mounts, connectivity, etc.)
devaipod logs myworkspace         # View agent logs (default container)
devaipod logs myworkspace -c workspace  # View workspace container logs
devaipod logs myworkspace -f      # Follow log output
devaipod stop myworkspace         # Stop a pod
devaipod delete myworkspace       # Delete a pod
devaipod delete myworkspace -f    # Force delete (stops first)
devaipod rebuild myworkspace      # Rebuild with new/updated image
devaipod rebuild myworkspace --image ghcr.io/org/dev:latest  # Use specific image
devaipod up . --dry-run           # Show what would be created
```

## Connecting to workspaces

```bash
devaipod attach myworkspace       # Connect to the AI agent (auto-continues session)
devaipod attach myworkspace -s ID # Connect to specific session
devaipod ssh myworkspace          # Open shell in workspace container
devaipod ssh myworkspace -- ls -la  # Run a specific command
devaipod ssh-config myworkspace   # Generate SSH config for editor integration
```

## Running agents with tasks

```bash
devaipod run . 'fix typos'                          # Run on local repo
devaipod run https://github.com/org/repo            # Prompts for task interactively
devaipod run https://github.com/org/repo -c 'task'  # Task via flag
devaipod run https://github.com/org/repo/issues/42  # Issue URL: default task "Fix <url>"
```

## Programmatic agent interaction

```bash
devaipod opencode myworkspace status              # Agent status and health
devaipod opencode myworkspace mcp list            # List MCP servers
devaipod opencode myworkspace mcp tools           # List available tools
devaipod opencode myworkspace session list        # List sessions
devaipod opencode myworkspace send "fix the bug"  # Send message to agent
```

The `send` command creates a session and returns the agent's response, useful for scripting:

```bash
# Send a task and get the response
devaipod opencode myworkspace send "list the files in src/" --json
```

## Shell completions

```bash
devaipod completions bash         # Generate bash completions
devaipod completions zsh          # Generate zsh completions
devaipod completions fish         # Generate fish completions
```

Note: The `devaipod-` prefix is optional for workspace names.

## Global flags

```bash
devaipod -v ...                   # Verbose output (debug logging)
devaipod -q ...                   # Quiet mode (warnings and errors only)
devaipod --config /path/to/config.toml ...  # Use custom config file
```

## Additional options

```bash
# Explicit pod naming (useful for CI/CD)
devaipod up . --name my-ci-run
devaipod run https://github.com/org/repo --name pr-123-fix

# JSON output for scripting
devaipod list --json
devaipod status myworkspace --json
devaipod debug myworkspace --json
```

## Editor Integration (WIP)

The `ssh-config` command outputs an SSH config entry to stdout:

```bash
devaipod ssh-config my-pod >> ~/.ssh/config
```

**Note**: Full SSH support for VSCode/Zed Remote SSH requires an SSH server in the container (currently not implemented). For now, use VSCode's Dev Containers extension or the CLI workflow.
