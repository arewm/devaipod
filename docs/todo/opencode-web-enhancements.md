# OpenCode Web UI Enhancements

The opencode web UI (served via `devaipod web`) already provides basic
agent interaction — session management, chat, and file viewing. It also
has built-in support for viewing changes the agent has made and commenting
on them, which gives us a starting point for code review without building
a custom UI from scratch.

This document tracks enhancements to build on top of that foundation.

> **See also**: [opencode-webui-fork.md](./opencode-webui-fork.md) for the
> plan to extend the vendored opencode SPA with devaipod-specific pages
> (pod management, multi-pod switching).

## Improved Code Review

The opencode web UI shows file changes and supports commenting inline. This
is already useful for reviewing what an agent has done before approving it.
Areas to extend:

- **Commit-level review**: Group changes by commit rather than showing a flat
  diff of all uncommitted changes. Allow approving or rejecting individual
  commits.
- **Accept/reject actions**: Add explicit approve/reject buttons that mark
  commits as reviewed. Approved commits could be auto-pushed or queued for
  the human to push.
- **Review state persistence**: Track which commits have been reviewed and
  their status (pending, approved, rejected, needs-changes). This state
  should survive page reloads.
- **Feedback to agent**: When rejecting or requesting changes, route the
  comment back to the agent as a new message so it can iterate.

## Multi-Pod Awareness

Currently the web UI connects to a single opencode backend. With the
pod-prefixed API proxy from [opencode-webui-fork.md](./opencode-webui-fork.md),
the review UI should work across pods:

- See pending reviews across all running pods in one view
- Switch between pods without losing review context
- Notifications when a pod has new commits ready for review

## Integration with Advisor Proposals

The [advisor agent](./advisor.md) creates draft proposals for new agent pods.
The web UI is a natural place to surface these:

- Show pending advisor proposals alongside running pods
- Approve/dismiss proposals directly from the UI
- See the advisor's rationale and source links inline

## Open Questions

1. **Upstream contribution**: Should we try to upstream the review
   enhancements to opencode, or keep them in the devaipod fork? The
   commit-level review workflow is fairly generic and could benefit other
   opencode users.

2. **Diff viewer quality**: The built-in opencode diff view may need
   enhancement (syntax highlighting, side-by-side mode). Evaluate whether
   to improve it in-fork or integrate a dedicated component like Monaco.

3. **Push workflow**: After approving commits, should the UI offer a
   one-click push? Or should pushing remain a deliberate CLI action?
