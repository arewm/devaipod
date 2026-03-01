# Agent UI: remaining work

Architecture and internals are documented in
[design.md](../src/design.md#web-ui-architecture) and
[internals.md](../src/internals.md).

## Vendored UI maintenance

- [ ] Write an `update-opencode-ui.sh` script for pulling new upstream releases

## Pod management navigation

- [ ] Add sidebar pod icon with navigation to `/pods`
- [ ] Add "Back to Pods" navigation from session view (currently via
      iframe wrapper bar)

## Git review

- [ ] Add push/sync button to GitReviewTab
- [ ] Wire inline comments from commit-range view back to agent prompt context
- [ ] Debug "expand" button in diff view
- [ ] Rename existing terminal label to "Agent Terminal" in devaipod mode
      (the `kind` parameter exists in `terminalTabLabel()` but is never passed)

## Review state and sync (Phase 3)

- [ ] Add review state endpoints
- [ ] Add sync endpoint — control plane runs `git push origin {branch}` in
      workspace container after verifying commits are in "approved" state
- [ ] Create review controls — approve/reject/sync buttons
- [ ] Review state persistence

See also [lightweight-review.md](./lightweight-review.md) for the detailed
review design (API endpoints, review state model, sync flow).

## Cleanup

- [ ] Drop `dist/index.html` (old control plane UI, still served at
      `/_devaipod/oldui` as fallback)

## Iframe removal (Phase 5)

Currently the agent view is embedded in an iframe (wrapper page with "Back
to Pods" bar). Removing the iframe requires:

- [ ] Navigate between `/pods` and agent sessions within the SPA router
      (no full page reload)
- [ ] The `ServerProvider` / `GlobalSDKProvider` must remount when switching
      pods (they read `server.url` once at init)
- [ ] Auth token must reach the pod-api sidecar (currently the SPA runs at
      the sidecar's origin, so no cross-origin issues — but navigating away
      from `/pods` to a different origin requires solving this)

This is a larger refactor. The iframe approach works well enough for now.

## Open questions

1. **Upstream contribution**: Should we try to upstream the review
   enhancements to opencode, or keep them in the devaipod fork? The
   commit-level review workflow is fairly generic and could benefit other
   opencode users.

2. **Diff viewer quality**: The built-in opencode diff view may need
   enhancement (syntax highlighting, side-by-side mode). Evaluate whether
   to improve it in-fork or integrate a dedicated component like Monaco.

3. **Push workflow**: After approving commits, should the UI offer a
   one-click push? Or should pushing remain a deliberate CLI action?
