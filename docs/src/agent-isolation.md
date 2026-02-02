# Agent Workspace Isolation

## Overview

The AI agent operates in an **isolated workspace** that is completely separate from the human's working tree. The agent cannot modify files in the human's workspace directly—changes must be explicitly pulled by the human after review.

This isolation prevents:

- Accidentally running AI-generated code before review
- Prompt injection attacks that could modify your working files
- Unintentional changes to your development environment

The human always has full control over when and how agent changes are incorporated.

## Architecture

The pod contains three containers that share git objects but maintain isolated working trees:

```
┌─────────────────────────────────────────────────────────────┐
│                        devaipod Pod                         │
├─────────────────────────┬─────────────────────────┬─────────┤
│   Workspace Container   │    Agent Container      │  Gator  │
│                         │                         │         │
│  /workspaces/<project>  │  /workspaces/<project>  │  MCP    │
│  (human's working tree) │  (agent's isolated      │  Server │
│                         │   working tree)         │         │
│  /mnt/agent-workspace   │  /mnt/main-workspace    │         │
│  (readonly, agent's     │  (readonly, for git     │         │
│   changes to pull)      │   object sharing)       │         │
└─────────────────────────┴─────────────────────────┴─────────┘
```

### Volume mounts

| Container | Path | Source | Access |
|-----------|------|--------|--------|
| Workspace | `/workspaces` | main workspace volume | read-write |
| Workspace | `/mnt/agent-workspace` | agent workspace volume | **read-only** |
| Agent | `/workspaces` | agent workspace volume | read-write |
| Agent | `/mnt/main-workspace` | main workspace volume | **read-only** |

The cross-mounts are read-only, so neither container can modify the other's working tree.

## Git object sharing

To avoid duplicating repository data, the agent's workspace is cloned using `git clone --shared`. This creates a `.git/objects/info/alternates` file that references the main workspace's git objects.

Benefits:

- Near-instant clone time (no network fetch needed)
- Minimal disk space overhead (objects shared, not copied)
- Full git functionality (the agent can commit, branch, etc.)

The agent's clone shares objects from `/mnt/main-workspace`, which contains the human's repository.

## Commands

Connect to the agent (default):

```bash
devaipod attach <name>
```

Connect to workspace container for manual work:

```bash
devaipod attach <name> -W
```

Create a pod and auto-start the agent on a task:

```bash
devaipod run <repo> "fix the bug in auth.rs"
```

SSH into the workspace container:

```bash
devaipod ssh <name>
```

## Bidirectional git remotes

Devaipod automatically sets up git remotes for collaboration in both directions:

| Container | Remote | Points to |
|-----------|--------|-----------|
| Workspace | `agent` | `/mnt/agent-workspace/<project>` |
| Agent | `workspace` | `/mnt/main-workspace/<project>` |

These remotes are set up automatically when the pod starts—no manual configuration needed.

## Workflow: Reviewing agent changes

The agent commits changes to its isolated workspace. To incorporate those changes into your working tree, use standard git operations from the workspace container.

First, connect to the workspace container:

```bash
devaipod attach <name> -W
# or
devaipod ssh <name>
```

The `agent` remote is already configured. Review and pull changes:

```bash
# Fetch agent's commits
git fetch agent

# See what the agent committed
git log agent/HEAD

# Review the diff
git diff HEAD..agent/HEAD

# Apply specific commits
git cherry-pick <commit>

# Or merge all agent changes
git merge agent/HEAD
```

## Workflow: Agent continues from human changes

When the human makes changes and wants the agent to continue from that point:

1. Human makes commits in the workspace container
2. Agent fetches from the pre-configured `workspace` remote:

```bash
# In the agent container (or via opencode)
git fetch workspace
git rebase workspace/HEAD
# or
git merge workspace/HEAD
```

This enables iterative collaboration loops:

1. Agent works on task, makes commits
2. Human reviews via `git fetch agent`, cherry-picks or edits
3. Agent fetches human's changes via `git fetch workspace`, continues
4. Repeat

## Security properties

This isolation model provides defense-in-depth:

1. **Write isolation**: The agent cannot modify your working tree. Any file changes require explicit `git fetch` + merge/cherry-pick.

2. **Commit review**: You see exactly what the agent changed before incorporating it. Use `git diff` and `git log` to review.

3. **Selective adoption**: Cherry-pick individual commits or reject changes entirely. You're not forced to accept everything.

4. **Credential isolation**: Combined with [sandboxing](sandboxing.md), the agent also lacks access to your GH_TOKEN and other credentials.

## Comparison with direct access

Without workspace isolation, the agent would have direct read-write access to your files. This means:

| Scenario | With isolation | Without isolation |
|----------|----------------|-------------------|
| Agent writes buggy code | Review before merge | Code already in your tree |
| Prompt injection attack | Cannot modify your files | Could delete/corrupt files |
| Agent makes unexpected changes | Visible in `git diff` | May not notice immediately |
| Reverting agent work | Don't merge it | Manual `git reset` required |

Workspace isolation means you always opt-in to agent changes rather than having to opt-out.
