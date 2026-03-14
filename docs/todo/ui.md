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

## Migrate agent iframe wrapper to SolidJS

The agent iframe wrapper page is currently server-rendered HTML with an
external JS file (`src/static/agent-wrapper.js`), served by
`agent_iframe_wrapper()` in `web.rs`. It should be migrated into the
SolidJS app (`opencode-ui/`) as a proper routed page.

### Why

The server-rendered wrapper was expedient but is the wrong architecture:

- The SolidJS app already has `DevaipodProvider` which polls for pod
  state, agent status, and launches. The wrapper JS duplicates this
  with its own `fetchPodList`/`setInterval` calls.
- Template-based JS generation (even with the extracted `.js` file)
  is fragile -- the doubled-brace / JSON-escaping / double-prefix
  bugs we hit are evidence of this.
- The pod switcher, done button, and title display are UI components
  that belong in the component tree, not in imperative DOM manipulation.
- Client-side pod switching via the SolidJS router would be a smooth
  route change instead of a full page reload.

### Design

Add a new SolidJS route `/agent/:name` that renders the same visual
structure (nav bar + iframe) as a reactive component:

```
<DevaipodProvider>
  <div id="dbar">
    <A href="/pods">← Pods</A>
    <DoneButton pod={name()} />
    <spacer />
    <PodSwitcher pods={runningPods()} current={name()} />
  </div>
  <iframe src={iframeSrc()} />
</DevaipodProvider>
```

Key decisions:

- **Pod list from context**: `PodSwitcher` reads `ctx.pods` from
  `DevaipodProvider` (already polling every 5s). No separate fetch.
- **Iframe src from API**: The component calls
  `/api/devaipod/pods/{name}/opencode-info` on mount to discover the
  pod-api port, same data `openPod()` already fetches.
- **Navigation**: Pod switching uses `navigate()` from the SolidJS
  router -- client-side route change, component re-renders, iframe
  src updates. No full page reload.
- **Loading state**: Nav bar renders immediately; iframe shows a
  placeholder until the opencode-info response arrives.
- **Error state**: If the pod-api port can't be discovered, show an
  error message inline instead of a blank iframe.
- **Auth**: Same cookie as now. The SPA is already authenticated.

### Files to change

| File | Change |
|---|---|
| `opencode-ui/packages/app/src/pages/agent.tsx` | New: agent page component |
| `opencode-ui/packages/app/src/app.tsx` | Add `/agent/:name` route |
| `opencode-ui/packages/app/src/context/devaipod.tsx` | Refactor `openPod` to use `navigate()` |
| `opencode-ui/packages/app/src/pages/pods.tsx` | Update PodCard to use router navigation |
| `src/web.rs` | Add `/agent/{name}` to SPA-served routes |

### Migration path

1. Add the SolidJS agent page alongside the existing wrapper.
   The old `/_devaipod/agent/{name}/` route stays as-is.
2. Switch `openPod` to navigate to `/agent/{name}` instead.
3. Add a redirect from `/_devaipod/agent/{name}/` to `/agent/{name}`
   for bookmarks/external links.
4. Remove `agent_iframe_wrapper()`, `agent-wrapper.js`,
   `serve_agent_wrapper_js`, and the `AGENT_WRAPPER_JS` constant.
5. Update Playwright tests to target `/agent/{name}` route.

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
