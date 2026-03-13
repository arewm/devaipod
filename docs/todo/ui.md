# UI improvements

## Session title

A human-editable "title" for each pod session, separate from the auto-generated
pod name.  The title provides a short, meaningful description of what the session
is about (e.g. "refactoring auth middleware", "PR #42 review").

The title is distinct from `task` (the agent's instruction) — it describes the
*session* from the human's perspective.

### Storage

The title is stored in the pod-api sidecar's state directory
(`$DEVAIPOD_STATE_DIR/title.txt`) so it survives TUI cache clears and is
accessible from both the TUI and web UI.

At pod creation time an initial title can be set via `--title` (stored as the
`io.devaipod.title` label for display before the pod-api is reachable).
Once the pod is running, the pod-api `GET/PUT /title` endpoints are authoritative.

### API

- `GET /title` — returns `{"title": "..."}` (falls back to the pod label if
  no file exists yet)
- `PUT /title` — accepts `{"title": "..."}`, writes to `title.txt`

### CLI

- `devaipod up --title "..."` / `devaipod run --title "..."`  — set at creation
- `devaipod title <pod> [new-title]` — get or set the title on a running pod

### TUI

- Display the title prominently on each instance card (line 1, after the name)
- `t` keybinding to edit the title of the selected instance (inline text input)

## Other planned UI work

Cross-references to existing TODO documents for UI-adjacent work:

- **Agent web UI**: see [opencode-webui-fork.md](./opencode-webui-fork.md)
  (sidebar nav, git review, iframe removal)
- **Pod management in web UI**: sidebar pod list, "Back to Pods" nav
  (tracked in opencode-webui-fork.md)
- **Review workflow**: see [lightweight-review.md](./lightweight-review.md)
  (commit-range review, approve/reject, push/sync)

### TUI card layout refinements

- Show title on line 1 alongside the pod name
- Consider showing the `task` (agent instruction) separately from the title
  when both are present
- Improve truncation of long repo URLs

### Session lifecycle visibility

Related to the "done vs active" item in [ideas.md](./ideas.md):
- Clearer visual distinction between active/idle/done sessions
- Consider a "done" badge or dimming for completed `run`-mode sessions

### Attach experience

Related to the "Login as bot" item in [ideas.md](./ideas.md):
- Three-pane attach: opencode UI, workspace terminal, agent terminal
- Distinct `CONTAINER_NAME` env vars for bash prompt disambiguation
