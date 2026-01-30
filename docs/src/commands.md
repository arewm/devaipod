# Commands

## First-time setup

```bash
devaipod init                     # Interactive setup wizard for API keys & tokens
```

## Workspace lifecycle

```bash
devaipod up .                     # Create pod with workspace + agent containers
devaipod up . -S                  # Create and SSH into workspace
devaipod up . "fix the bug"       # Create with task description for agent
devaipod list                     # List devaipod workspaces
devaipod status myworkspace       # Show detailed status of a pod
devaipod logs myworkspace         # View container logs (-c agent for agent logs)
devaipod stop myworkspace         # Stop a pod
devaipod delete myworkspace       # Delete a pod
devaipod up . --dry-run           # Show what would be created
```

## Connecting to workspaces

```bash
devaipod ssh myworkspace          # SSH into workspace (shows agent monitor)
devaipod ssh myworkspace bash     # SSH directly to shell
devaipod ssh-config myworkspace   # Generate SSH config for editor integration
```

## Running agents with tasks

```bash
devaipod run . 'fix typos'                          # Run on local repo
devaipod run https://github.com/org/repo            # Prompts for task interactively
devaipod run https://github.com/org/repo -c 'task'  # Task via flag
devaipod run https://github.com/org/repo/issues/42  # Issue URL: default task "Fix <url>"
```

## Shell completions

```bash
devaipod completions bash         # Generate bash completions
devaipod completions zsh          # Generate zsh completions
devaipod completions fish         # Generate fish completions
```

Note: The `devaipod-` prefix is optional for workspace names.

## Editor Integration (WIP)

The `ssh-config` command outputs an SSH config entry to stdout:

```bash
devaipod ssh-config my-pod >> ~/.ssh/config
```

**Note**: Full SSH support for VSCode/Zed Remote SSH requires an SSH server in the container (currently not implemented). For now, use VSCode's Dev Containers extension or the CLI workflow.
