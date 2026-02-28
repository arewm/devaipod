# Audit: Agent UI URL rewriting and proxy (web server)

**Date:** 2026-02  
**Scope:** `src/web.rs` — opencode UI serving, URL rewriting, root API proxy.

## 1. URL rewriting (serve_opencode_static)

### HTML (index.html only)

| Pattern | Rewritten to | Notes |
|---------|--------------|--------|
| ` src="/` | ` src="/agent/{name}/` | Script/source with space before attr |
| ` href="/` | ` href="/agent/{name}/` | Link with space before attr |
| ` src='/` | ` src='/agent/{name}/` | Single-quoted |
| ` href='/` | ` href='/agent/{name}/` | Single-quoted |

**Not rewritten:** Minified HTML with no space before `src`/`href` (e.g. `<script src="/`) would not match. Vite output typically includes a space. If 404s appear for assets referenced in HTML without a leading space, add `src="/`, `href="/`, `src='/`, `href='/` and apply after the space-prefixed replacements to avoid double-rewrite, or use a single pass that skips when the path already starts with `/agent/`.

### CSS (all .css files)

| Pattern | Rewritten to | Notes |
|---------|--------------|--------|
| `url(\"/` | `url(\"/agent/{name}/` | Double-quoted |
| `url('/` | `url('/agent/{name}/` | Single-quoted |
| `url(/` | `url(/agent/{name}/` | Unquoted (e.g. fonts) |
| `url( "/` | `url( "/agent/{name}/` | Space after parenthesis (double quote) |
| `url( '/` | `url( '/agent/{name}/` | Space after parenthesis (single quote) |

### Root /assets/* route (no JS rewriting)

The opencode bundle requests fonts and other assets at **origin root** (e.g. `GET /assets/inter-FIwubZjA.woff2`) because it uses absolute paths. Rewriting JS would be fragile. Instead we **route `/assets` and `/assets/{*path}`**:

- **When `DEVAIPOD_AGENT_POD` cookie is set** (user loaded an agent page): serve from the vendored opencode UI dir (`OPENCODE_UI_PATH/assets/...`). Fonts and other assets return 200.
- **When the cookie is not set**: serve from the control-plane static dir (`static_dir/assets/...`) so the main UI’s own assets still work.

No HTML/CSS/JS string rewriting is needed for `/assets/*`; the cookie selects the correct backend.

## 2. Root-level API proxy (opencode_root_proxy)

**Cookie:** `DEVAIPOD_AGENT_POD` (value = pod name, URL-encoded when set, decoded when read).

**Routes (must match agent_ui_handler api_paths):**

- `/session`, `/session/{*path}`
- `/rpc`, `/rpc/{*path}`
- `/event`, `/event/{*path}`
- `/global`, `/global/{*path}`
- `/path`, `/path/{*path}`
- `/project`, `/project/{*path}`
- `/provider`, `/provider/{*path}`
- `/auth`, `/auth/{*path}`

**Single source of truth:** `OPENCODE_API_SEGMENTS` in `web.rs` is used by `agent_ui_handler`. The root routes in `build_app()` must be kept in sync when adding segments (add both the segment and `/{*path}` variant).

## 3. Content types (serve_opencode_static)

Served with correct Content-Type: html, js, css, json, png, svg, ico, woff, woff2, ttf. Default: application/octet-stream.

## 4. Verification

- Unit tests: `cargo test -p devaipod web::` — `test_agent_ui_api_path_detection` (OPENCODE_API_SEGMENTS), `test_agent_ui_rewrite_patterns` (HTML/CSS rewrite patterns), `test_agent_redirect` (307 + cookie), `test_agent_back_button_injection`.
- Integration tests: `just test-integration` (full critical path, assets, root API with cookie, CSS rewrite when applicable). See `crates/integration-tests/src/tests/webui.rs` module doc and `test_web_opencode_ui_full_critical_path`.

## 5. Static file fallback for bare filenames (fonts)

When a request path is a bare filename (e.g. `BlexMonoNerdFontMono-Regular-DSJ7IWr2.woff2`) with no directory component, the file may not exist at the opencode dist root. `serve_opencode_static` tries `assets/<filename>` and then `assets/fonts/<filename>` so that relative `url(font.woff2)` in CSS (resolved against `/agent/{name}/`) still finds the file.

## 6. Changelog

- Added CSS `url(/` (unquoted) rewrite for font 404s.
- Added this audit doc and in-code comments for rewrite patterns.
- Added fallback for bare-filename requests to `assets/` and `assets/fonts/` for font 404s.
- **Root cause of font 404s:** Font paths are in the JS bundle and request `/assets/*` at origin. Fixed by routing `/assets/*`: with agent cookie serve opencode; else control-plane static dir. No JS rewriting.
