# Internals

## Crates

- [`devaipod`](internals/devaipod/index.html) - Main binary and library

To build the rustdoc documentation locally:

```bash
cargo doc --workspace --no-deps --document-private-items
```

## Key UI source files

For the core Rust source files, see [Architecture](architecture.md).

| File | Purpose |
|------|---------|
| `opencode-ui/packages/app/src/context/devaipod.tsx` | Pod management context |
| `opencode-ui/packages/app/src/pages/pods.tsx` | Pod management page |
| `opencode-ui/packages/app/src/context/workspace-terminal.tsx` | Workspace PTY client |
| `opencode-ui/packages/app/src/pages/session/git-review-tab.tsx` | Git diff review |
| `opencode-ui/packages/app/src/pages/session/terminal-panel.tsx` | Agent/Workspace terminal tabs |
| `opencode-ui/packages/app/src/utils/devaipod-api.ts` | `isDevaipod()`, `apiFetch`, error reporting |

## Testing

**Rust unit tests** (`cargo test`): ~274 tests covering web.rs routing,
proxy behavior, pod configuration, git operations. Run via `just test-container`.

**Bun unit tests** (`bun test` with happy-dom): 46 existing test files.
Covers devaipod-specific modules like `utils/devaipod-api.ts` (`apiFetch`,
error reporting), `context/workspace-terminal.tsx` (session lifecycle), and
`pages/session/terminal-label.ts` (`kind` prefix formatting).

**Rust integration tests** (`cargo test -p integration-tests`): verify HTTP
endpoints, auth, static files, and proxying using curl inside a running
devaipod container.

**Playwright E2E tests** (`bun test:e2e`): 33 existing specs. For devaipod
features, the SPA can be served directly from the pod-api sidecar — no
cookie injection needed since `VITE_DEVAIPOD=true` enables all devaipod
code paths at build time.

## Notable discoveries

- **`exec_in_container` has ~200-500ms overhead per call** through the podman
  VM on macOS — this motivated creating the pod-api sidecar.
- **SELinux is enforcing** on the podman machine VM; the api container needs
  `label=disable` for the podman socket.
- **GlobalSDKProvider does NOT react to URL changes** — it reads `server.url`
  once at init time. This is why iframe removal is deferred (see
  [the todo](../todo/opencode-webui-fork.md)).
- **SolidJS `createEffect` reactive tracking** — async functions reading
  store properties inside `createEffect` cause accidental tracking loops;
  must wrap in `untrack()`.
- **Each pod on its own origin** naturally isolates localStorage, eliminating
  the need for monkey-patching approaches.
