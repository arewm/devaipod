# Workspace v2: Rethinking Volumes, Worktrees, and the Devcontainer Model

🤖 Assisted-by: OpenCode (Claude Opus 4.6)

## Problem Statement

The current sandbox model creates **up to 5 named volumes per pod**: `{pod}-workspace`,
`{pod}-agent-home`, `{pod}-agent-workspace`, and (with orchestration) `{pod}-worker-home`,
`{pod}-worker-workspace`. Each new workspace clones the repository again (the agent clone
uses `--shared` for object sharing, but still creates a full working tree). More
importantly, each pod gets its own isolated devcontainer -- there is no way for a
single, long-lived devcontainer to serve multiple agent sessions working on the same
repository.

The key pain point:

1. **Local development flow**: When running devaipod from your own machine against a
   local repo, the current model bind-mounts your `.git` directory read-only and
   clones from it into a volume. You can't just "point the agent at your tree."
   There's no equivalent of Cursor's worktree mode where the agent works in a
   sibling worktree of your checkout.

## Prior Art

### Cursor Worktrees

Cursor's parallel agent model creates git worktrees under `~/.cursor/worktrees/<repo>/`.
Each agent gets its own worktree (and branch). When the agent finishes, the user clicks
"Apply" to merge changes back to the primary working tree. Key details:

- 1:1 mapping of agents to worktrees
- Worktrees are created from the user's current branch state
- `.cursor/worktrees.json` allows an initialization script (install deps, copy `.env`, etc.)
- Automatic cleanup: max 20 worktrees per workspace, oldest removed first
- "Apply" does a git merge from the worktree branch into the primary tree

### Gastown

Uses git worktrees for agent isolation but with no container sandboxing -- agents run in
tmux sessions with full host access. The worktree is the *only* isolation boundary.

### paude

[paude](https://github.com/bbrowning/paude) has the most polished CLI workflow for
getting code in and out of a sandboxed container. Worth studying in detail.

**Git transport via `ext::` protocol**: paude uses git's `ext::` transport to tunnel
the git protocol over `podman exec` (or `oc exec` for OpenShift) stdin/stdout. The
local git remote URL looks like:

```
ext::podman exec -i <container> %S /pvc/workspace
```

The container-side workspace is initialized with `receive.denyCurrentBranch updateInstead`,
which tells git to accept pushes to the checked-out branch and update the working tree
immediately. This is elegant: no SSH server, no published ports, no network exposure --
just `git push paude-<name> main` and the code appears inside the container.

**Harvest + reset loop**: paude separates the lifecycle into distinct commands that
compose into a fire-and-forget workflow:

```bash
paude create --yolo --git myproject     # Build image, create container, push code
paude connect myproject                 # Attach to tmux session running agent
# ... agent works, detach with Ctrl+b d ...
paude harvest myproject -b feat/xyz     # Fetch agent commits, checkout local branch
paude harvest myproject -b feat/xyz --pr  # Or: fetch + push + create GitHub PR
paude reset myproject                   # Hard-reset workspace, clear history
# ... repeat with new task ...
```

The `harvest` command fetches from the container, checks out a local branch at
the container's HEAD, and optionally pushes + creates a PR. The `reset` command
does `git reset --hard origin/main && git clean -fdx` inside the container,
clears conversation history, and updates `refs/paude/base` to track the new
starting point. Together they form a clean "task loop" without recreating
containers.

**`refs/paude/base` for diffing**: paude writes a special ref inside the container
marking "where the agent started." This lets `paude status` show a meaningful diff
of what the agent has added beyond the starting point.

**Credential injection at connect-time only**: `GH_TOKEN` is passed via
`podman exec -e GH_TOKEN=...` when the user attaches -- never stored in the
container definition, never written to disk. On OpenShift, credentials go to a
tmpfs volume and a watchdog process wipes them after an inactivity timeout
(default 60 min). This is stronger than devaipod's current model where credentials
are set as container env vars at creation time.

**What devaipod should learn from paude:**

1. The `ext::podman exec` git transport could replace devaipod's current
   cross-container git remotes (which use volume path-based remotes like
   `/mnt/agent-workspace`). It would work across host/container boundaries too,
   which is exactly what `--worktree` mode needs for getting agent commits back
   to the host.

2. The `harvest` + `reset` loop is a workflow devaipod should support. Currently
   devaipod has no equivalent of "fetch agent changes into my local branch and
   optionally open a PR" as a single command.

3. Connect-time credential injection is less relevant for devaipod since we
   use service-gator for scoped access to external services (GitHub, JIRA, etc.)
   rather than injecting raw credentials into agent containers. LLM API keys
   are the only secrets the agent receives, and those are already
   credential-isolated from trusted tokens like `GH_TOKEN`.

### gjoll

gjoll solves the same code-transfer problem via `git push` over SSH with
`receive.denyCurrentBranch=updateInstead` -- similar idea to paude's `ext::`
approach but requires an SSH server in the sandbox.

## Proposed Models

### Model A: Local Worktree Mode (`devaipod launch --worktree`)

For local development, add a CLI flow inspired by Cursor:

```bash
# From inside a git repo:
devaipod launch --worktree
devaipod launch --worktree "fix the auth bug"  # with a task
```

This would:

1. Verify the current directory is a git repo
2. Create a host directory at `../<repo>-devaipod-<short-id>/` for the agent's
   working tree
3. **Bind mount that directory read-write** into the agent container at
   `/workspaces/<repo>`
4. **Bind mount the original repo read-only** at `/mnt/source-repo`
5. Inside the container, run `git clone --reference /mnt/source-repo` into
   `/workspaces/<repo>` -- this creates a full independent clone whose object
   store borrows from the RO mount via alternates
6. The devcontainer is built/run as usual, but the workspace is a host bind mount
   rather than a podman volume

`--reference` is the right tool here: the clone gets its own `.git` directory
with its own refs, HEAD, and object store. Existing objects are read from the
RO-mounted source repo via alternates; new objects (from `git commit`,
`git fetch`) are written to the clone's own store. No patching of gitdir
pointers needed -- `git clone --reference` handles all of this.

The working tree lives on the host filesystem, so the human can see agent
changes in real time from outside the container.

**Verified**: tested with rootless podman, RO bind mount, `git clone --reference`.
All operations work: `commit`, `fetch`, `gc`, `diff` across alternates. The RO
source repo is never written to.

**Alternates path caveat**: inside the container, alternates points to
`/mnt/source-repo/.git/objects`. From the host, that path doesn't exist, so
`git log` on the host copy of the working tree fails. Fix: after the container
creates the clone, rewrite `.git/objects/info/alternates` to the host-side path
(e.g., `../../<original-repo>/.git/objects`). This is a one-line file.

**Mount layout:**

```
Host                                  Container
────────────────────────────────────  ────────────────────────────
../<repo>-devaipod-<id>/         →   /workspaces/<repo>     (RW)
  ├── .git/  (independent clone)      ├── .git/
  │   └── objects/info/alternates     │   └── objects/info/alternates
  │        → /mnt/source-repo/...     │        → /mnt/source-repo/...
  ├── src/                            ├── src/
  └── ...                             └── ...

./ (original repo)               →   /mnt/source-repo       (RO)
  ├── .git/                           ├── .git/
  │   └── objects/                    │   └── objects/
  ├── src/                            ├── src/
  └── ...                             └── ...
```

**Advantages:**
- Familiar to Cursor users
- No volume creation overhead for workspace content
- Agent changes are immediately visible on the host via git
- Natural "apply" flow: `cd ../<worktree> && git diff` or cherry-pick from the worktree
- Plays well with existing editor integrations (VS Code multi-root workspaces, etc.)

**Trade-offs:**
- Bind mounts with rootless podman have UID mapping considerations
  (`userns=keep-id`, etc.). SELinux labeling (`:Z`/`:z`) is less of a concern
  since devaipod already uses `label=disable` for nested container support.
- The worktree is writable from the host -- no container-enforced isolation of the
  working tree (the agent can still only access what's bind-mounted)
- Need a cleanup mechanism (Cursor caps at 20 worktrees per repo)
- **Dirty tree semantics**: `git worktree add` creates from HEAD (or a specified
  commit), not from the dirty working tree. Uncommitted changes in the user's
  checkout won't be visible to the agent. This differs from the current `LocalRepo`
  flow which mounts `.git` (including staged index state).

**Implementation sketch:**

```rust
// In main.rs, new flag on UpOptions:
/// Create a git worktree instead of cloning into a volume
#[arg(long = "worktree")]
worktree: bool,

// In the create flow:
if opts.worktree {
    let worktree_path = create_host_worktree(&source_repo, &pod_name)?;
    // Use bind mount instead of volume for workspace
    // Read-only bind mount source .git for alternates
}
```

**Flag conflict**: `-W` currently means `--workspace` on `attach` and `exec`. We should
use `--worktree` (long only, no short flag) to avoid confusion across subcommands.

## Implementation Plan

Implement `devaipod launch --worktree` for the local development case. This gives
Cursor-like ergonomics without architectural changes to the pod model.

Key implementation tasks:
- Add `--worktree` flag to `up`/`run` commands
- Implement `create_host_worktree()` that runs `git worktree add`
- Modify `WorkspaceSource::LocalRepo` path to use bind mount instead of volume
  when worktree mode is active
- Handle UID mapping for rootless podman (`--userns=keep-id`)
- Add `devaipod worktree list` and `devaipod worktree clean` subcommands
- Respect `.devaipod/worktrees.json` for initialization scripts (Cursor compat)
- Add a `harvest`-like command (inspired by paude) to fetch agent commits into
  a local branch and optionally open a PR

## Open Questions

1. **Worktree location**: Cursor uses `~/.cursor/worktrees/<repo>/`, Gastown uses
   sibling directories. What's the right default for devaipod? Sibling dirs
   (`../<repo>-devaipod-<id>/`) feel more natural for CLI usage.

2. **UID mapping**: Rootless podman with bind mounts requires careful UID handling.
   `--userns=keep-id` maps the container user to the host user, but this may
   conflict with devcontainer images that expect a specific UID. Need to test
   with common devcontainer images.

3. **Cleanup policy**: How aggressively should worktrees be cleaned up? Cursor's
   "max 20, remove oldest" is reasonable. Should we clean up on `devaipod delete`?

4. **`git gc` safety**: With the alternates-based approach, `git gc` on the host
   could theoretically prune objects that the agent's alternates still reference.
   In practice this is low-risk: `git gc` runs infrequently and the agent session
   is short-lived. If it becomes a problem, the agent's init can run
   `git repack -a -d` to copy referenced objects locally (like the current worker
   clone does with `--dissociate`).

5. **Devcontainer lifecycle commands**: The current flow runs `onCreateCommand`,
   `postCreateCommand` etc. in both workspace and agent containers. In worktree
   mode, there's no volume to initialize -- the worktree already exists on disk.
   The lifecycle flow would need rethinking (perhaps only run on first use of a
   devcontainer image, not per-worktree).
