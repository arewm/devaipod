# Dynamic subagent containers via MCP

## Background

The original orchestration design pre-created a worker container alongside
every agent container. This added significant complexity (extra volumes,
git clone chains, volume cross-mounts, workerctl script) even when the
agent never needed a subagent. We removed this as the default.

The better approach: let the agent spawn subagent containers on demand via
an MCP tool, as part of the same podman pod (shared network namespace).

## Design

### MCP tool: `spawn_subagent`

Exposed via service-gator (or a new devaipod MCP server). The agent calls
it when it decides to delegate work.

```json
{
  "name": "spawn_subagent",
  "arguments": {
    "task": "Implement the foo feature per the spec in docs/foo.md",
    "branch": "work/foo-feature"
  }
}
```

The tool:

1. Creates a new container in the existing pod (shared localhost)
2. Clones the agent's workspace into a new volume (or uses `git worktree`)
3. Creates a branch for the subagent's work
4. Starts `opencode serve` on an auto-assigned port
5. Sends the task as the initial message
6. Returns a handle (container name + port) the agent can poll

### MCP tool: `subagent_status`

```json
{
  "name": "subagent_status",
  "arguments": { "handle": "..." }
}
```

Returns whether the subagent is idle/working/completed, its recent
commits, and any errors.

### MCP tool: `subagent_merge`

```json
{
  "name": "subagent_merge",
  "arguments": {
    "handle": "...",
    "action": "merge"  // or "reject", "iterate"
  }
}
```

Merges/cherry-picks the subagent's commits into the agent's workspace,
then tears down the subagent container and volume.

## Advantages over the pre-created worker

- No upfront cost: pods start faster, use fewer resources
- Multiple subagents: the agent can spawn more than one for parallel work
- Clean lifecycle: containers are created and destroyed per-task
- Simpler git topology: no shared-object clones or dissociation steps;
  each subagent gets a plain `git clone` or `git worktree`

## Implementation plan

1. Implement `spawn_subagent` in service-gator (or a dedicated devaipod
   MCP server running in the agent container)
2. The tool calls the host's podman API (via the socket mounted in the
   agent container, or via the devaipod web API) to create a new container
   in the existing pod
3. Add `subagent_status` and `subagent_merge` tools
4. Write an opencode instruction/skill that teaches the agent when and
   how to use subagents effectively
5. Add cleanup logic: subagent containers are removed when the agent
   session ends or when explicitly torn down

## Open questions

- Should subagents share the agent's LLM credentials or have independent
  config? (Probably shared, copied from agent home like the old worker)
- Should the devaipod web UI show subagent containers? (Probably not
  initially — they're an implementation detail of the agent)
- Can we use `git worktree` instead of full clones to save disk space?
- Should there be a limit on concurrent subagents per pod?
- Should the MCP server run in the agent container or in the devaipod
  controller? (Agent container is simpler; controller has more authority)
