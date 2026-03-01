# Workspace Development Access from the Web UI

Today the workspace container is accessible only via `devaipod attach -W` (tmux
in a terminal). This document covers two related improvements: aligning the
devaipod web frontend with the opencode web UI, and adding a web-based terminal
for the workspace container.

These are tightly coupled — if we align the frontends we can reuse opencode's
UI components and potentially its terminal infrastructure for workspace access.

## Current State

The old `dist/index.html` control plane UI has been removed. The vendored
opencode SPA (`/usr/share/devaipod/opencode`) now handles all UI, including
pod management (`/pods` route) and the agent view (SolidJS + Tailwind).

The agent view is still embedded in an iframe (wrapper page with "Back to
Pods" bar). See [opencode-webui-fork.md](./opencode-webui-fork.md) for the
remaining iframe-removal plan.

**No web terminal exists.** There are no WebSocket endpoints in `web.rs`. The
podman proxy is HTTP-only and can't bridge podman's exec/attach streaming.
The opencode web UI has a chat interface for the agent — it is not a shell
terminal. Interactive container access is CLI-only (`devaipod attach`).

**The workspace container** (`devaipod-<name>-workspace`) runs `sleep infinity`
and holds the human's development environment. It has trusted credentials
(GH_TOKEN, SSH keys), full devcontainer privileges, and read-write access to
the workspace volume. The agent container is sandboxed separately with its own
workspace clone.

## Part 1: Frontend Alignment

### Problem

The devaipod control plane and the opencode agent UI look and feel different.
The control plane uses a warm dark theme with Inter/IBM Plex Mono fonts; the
opencode SPA uses its own design tokens and (by default) a light theme. Switching
between them is jarring, and the iframe wrapper adds visual overhead.

### Approach: Native Integration (per opencode-webui-fork.md)

Rather than trying to bridge themes across an iframe, extend the vendored
opencode SPA to include devaipod-specific pages as native SolidJS routes.
This is already planned in [opencode-webui-fork.md](./opencode-webui-fork.md):

1. Add a `/pods` route inside the opencode SPA for pod management (done)
2. Use per-pod API prefixes (`/pods/{name}/api/...`) instead of cookie routing
3. Reuse opencode's UI library (`@opencode-ai/ui`) for buttons, dialogs, etc.
4. Remove the iframe wrapper entirely

Benefits for workspace-dev specifically:

- Theme consistency comes for free — everything uses opencode's theme system
- Terminal components added to the opencode SPA can be reused for both agent
  chat and workspace shell access
- The SolidJS build system supports importing xterm.js as a dependency

### Incremental Path

The full fork-and-extend is significant work. An incremental approach:

1. **First**: Add a devaipod theme override to the opencode SPA build that
   switches its default to dark mode and adjusts accent colors. This is a
   small CSS/config change in the vendored build.
2. **Second**: Add the `/pods` route and pod management page as described in
   the fork doc (done — old `dist/index.html` has been removed).
3. **Third**: Add the workspace terminal (Part 2 below) as a component within
   the SPA, reachable from both the pod management page and the agent view.

## Part 2: Web Terminal for Workspace Container

### What We Need

A button on the control plane (or within the agent view) that opens an
interactive shell in the workspace container. This is the web equivalent of
`devaipod attach -W` — a full PTY with proper terminal emulation.

### Architecture

```
Browser                    devaipod web server              podman
┌──────────────┐          ┌──────────────────┐          ┌──────────┐
│ xterm.js     │◀── ws ──▶│ WebSocket        │── exec ─▶│workspace │
│ (in SPA)     │          │ handler          │  (PTY)   │container │
│              │          │                  │          │          │
│ Terminal tab │          │ /api/devaipod/   │          │ bash/zsh │
│ in pod view  │          │  pods/{name}/    │          │          │
└──────────────┘          │  terminal        │          └──────────┘
                          └──────────────────┘
```

Components:

1. **WebSocket endpoint** in `web.rs`:
   `GET /api/devaipod/pods/{name}/terminal` — upgrades to WebSocket, then
   creates a podman exec with PTY and bridges stdin/stdout between the
   WebSocket and the exec stream. This is similar to what VS Code's
   remote containers extension does.

2. **xterm.js frontend**: A terminal emulator component embedded in the
   pod management page (or accessible from the agent view sidebar).
   xterm.js handles rendering, input, resize events, and clipboard.

3. **Resize handling**: xterm.js sends resize events via a control message
   on the WebSocket. The server forwards these via bollard's `resize_exec`
   to podman.

### Implementation Options

**Option A: Podman exec via bollard (recommended)**

Use bollard's `create_exec` + `start_exec` with `tty: true` and
`attach_stdin/stdout/stderr: true`. Bollard returns a `StartExecResults::Attached`
stream that can be bridged to a WebSocket. This is the same mechanism the
existing `exec_in_container()` function uses, extended with TTY and
bidirectional streaming.

Advantages: uses our existing bollard connection, no extra dependencies,
full control over the exec parameters (user, env, working directory).

**Option B: Proxy podman's native WebSocket exec API**

Podman's REST API has `POST /exec/{id}/start` which supports WebSocket
upgrade for interactive sessions. We could extend the `/api/podman/*` proxy
to support WebSocket upgrades and let the frontend call the podman API
directly.

Advantages: less server-side code. Disadvantages: exposes podman API
details to the frontend, harder to add auth/validation, the current
proxy doesn't support upgrades.

**Option C: ttyd as sidecar**

Run [ttyd](https://github.com/tsl0922/ttyd) or a similar terminal
server as a sidecar in the pod, expose its WebSocket endpoint.

Advantages: battle-tested terminal serving. Disadvantages: another container
to manage, another port, doesn't reuse our existing infrastructure.

### Targeting the Right Container

The terminal should connect to the **workspace** container, not the agent
container. The workspace container:

- Has the human's trusted credentials
- Has full devcontainer capabilities (fuse, kvm, etc.)
- Is where humans do interactive development work
- Runs `sleep infinity` so it's always available

The exec command should default to the user's shell (read from the
devcontainer config's `remoteUser` and their `$SHELL`), falling back to
`/bin/bash`.

### Security

- The terminal endpoint must be behind the existing auth middleware
  (bearer token), same as all `/api/devaipod/*` routes.
- The exec runs as the devcontainer's configured user (not root), matching
  `devaipod attach -W` behavior.
- Consider adding a per-session nonce to the WebSocket URL to prevent
  replay/hijacking.

## Relationship to Other Work

- **[opencode-webui-fork.md](./opencode-webui-fork.md)**: The fork plan is a
  prerequisite for clean integration. The terminal should be added as a
  component within the opencode SPA.
- **[advisor.md](./advisor.md)**: The advisor pod is a workspace like any other.
  Web terminal access would let users interact with the advisor's workspace
  directly from the browser.
- **SSH access**: `devaipod ssh-config` generates SSH config entries for
  ProxyCommand-based access to workspace containers. The web terminal is
  complementary — useful when SSH isn't available (browser-only environments,
  Chromebooks, tablets).

## Open Questions

1. Should the terminal be a tab within the agent view (alongside chat), or a
   separate page? A tab feels more natural but requires the opencode SPA
   fork to be in place.

2. Should we support multiple terminal sessions per workspace? VS Code does
   this; it's useful but adds complexity (session management, tab UI).

3. File editing: once we have a web terminal, users will want a web editor
   too. Should we consider embedding Monaco or just rely on terminal-based
   editors (vim/nano)?

4. Should the terminal support connecting to the agent container as well
   (for debugging agent issues)? This would need a container selector in
   the UI.
