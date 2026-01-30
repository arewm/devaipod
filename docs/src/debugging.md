# Debugging

This guide covers common debugging techniques for devaipod pods and containers.

## Quick Diagnostics with `devaipod debug`

The fastest way to diagnose issues is using the built-in debug command:

```bash
devaipod debug <workspace>
```

This checks:
- Pod state and project info
- Gator container: version, mount type, git accessibility
- Agent container: health, MCP configuration
- MCP connectivity between agent and gator

Example output showing a problem:

```
=== Pod Debug: devaipod-myproject-abc123 ===

State: Running
Project: myproject

--- Gator Container ---
  Present: yes
  Version: service-gator 0.2.0
  Workspace mount: none (read-write)
  Git accessible: NO - check mount!

--- Agent Container ---
  Health: healthy
  MCP configured: yes

--- MCP Connectivity ---
  Gator reachable from agent: yes
```

The "Git accessible: NO" indicates the gator can't see the workspace—likely a mount configuration issue.

Use `--json` for machine-readable output:

```bash
devaipod debug <workspace> --json
```

## Manual Inspection

For deeper investigation, you can inspect pods and containers directly.

### Inspecting Pods and Containers

List running pods:

```bash
podman pod ls
```

Check pod labels (useful for finding devaipod-managed pods):

```bash
podman pod inspect <pod> | jq '.[0].Labels'
```

Check a container's command:

```bash
podman inspect <container> | jq '.[0].Config.Cmd'
```

Check container mounts:

```bash
podman inspect <container> | jq '.[0].Mounts'
```

## Service-gator Issues

### Verifying Mounts

The gator container needs the workspace volume mounted correctly (as a named volume, not a bind mount from a temp directory). To check:

```bash
podman inspect <pod>-gator | jq '.[0].Mounts'
```

Look for the workspace mount—it should reference the pod's volume, not a host temp path.

### Checking Git Repository Access

If `git_push_local` fails with "Not a git repository", the gator can't see the workspace:

```bash
podman exec <pod>-gator ls -la /workspaces/<project>/.git
```

This should show the `.git` directory contents. If it fails, the volume mount is misconfigured.

### Testing Local Gator Builds

To test a locally-built service-gator image:

```bash
devaipod up . --service-gator=github:myorg/myrepo --service-gator-image localhost/service-gator:latest
```

## MCP Connection Debugging

The agent talks to service-gator via localhost (they share a pod network namespace).

Check MCP status from the agent container:

```bash
podman exec <pod>-agent opencode mcp list
```

Test basic connectivity to the gator:

```bash
podman exec <pod>-agent curl -s http://localhost:8765/
```

## Using opencode-connect

The workspace container includes `opencode-connect`, a script that connects to the agent. The agent listens on `localhost:4096`, and the gator listens on `localhost:8765`.

## Common Issues

| Symptom | Likely Cause | Fix |
|---------|--------------|-----|
| "Not a git repository" from `git_push_local` | Gator can't see workspace | Check volume mounts on gator container |
| "Permission denied" on workspace files | SELinux or wrong mount type | Ensure `:z` label on bind mounts, or use volumes |
| Old service-gator behavior | Cached old image | Use `--service-gator-image` to specify version |
| MCP tools not available | Gator not running or misconfigured | Check `podman ps` and verify gator container is up |

## See Also

- [Service-gator Integration](service-gator.md) - Architecture and configuration
- [Sandboxing Model](sandboxing.md) - Container security model
