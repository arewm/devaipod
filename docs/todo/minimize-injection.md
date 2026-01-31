# Minimize Code Injection into Containers

This document describes the plan to reduce/eliminate code injection into containers, moving logic into the host `devaipod` binary instead.

## Current State

We currently inject:

1. **`opencode-connect`** - Shell script in workspace container
   - Finds root session via curl + Python
   - Runs `opencode attach`
   
2. **`workspace_monitor.py`** - Python script (339 lines)
   - Polls opencode API for session status
   - Displays live status in terminal
   - Handles Ctrl-C to drop to shell
   - Sends initial task to agent

3. **Clone scripts** - Shell scripts for git operations
   - `clone_from_local_script()`
   - `clone_remote_script()`
   - `clone_pr_script()`
   - `clone_dotfiles_script()`

## Problems with Injection

1. **Dependency on container environment**: Requires Python, curl, specific shell features
2. **Version skew**: Scripts embedded in Rust binary vs actual execution environment
3. **Testing difficulty**: Can't easily test scripts in isolation
4. **Maintenance burden**: Two languages (Rust + Python/shell) to maintain
5. **Security surface**: Injected code runs with container privileges

## Proposed Changes

### 1. Replace `opencode-connect` with host-side session detection

The host `devaipod` binary queries the API directly (using reqwest), then execs into the agent container to run `opencode attach`. No opencode needed on the host.

```rust
// In devaipod binary - query API from host
async fn find_root_session(pod_name: &str) -> Result<Option<String>> {
    let port = get_published_port(pod_name)?;
    let password = get_pod_api_password(pod_name)?;
    
    let client = reqwest::Client::new();
    let sessions: Vec<Session> = client
        .get(&format!("http://127.0.0.1:{}/session", port))
        .basic_auth("opencode", Some(&password))
        .send().await?
        .json().await?;
    
    // Find root session (no parent)
    let root = sessions.iter()
        .filter(|s| s.parent_id.is_none())
        .min_by_key(|s| s.time.created);
    
    Ok(root.map(|s| s.id.clone()))
}

async fn cmd_attach(pod_name: &str, session: Option<&str>) -> Result<()> {
    // Detect session from host (API call via reqwest)
    let session_id = match session {
        Some(s) => s.to_string(),
        None => find_root_session(pod_name).await?.unwrap_or_default(),
    };
    
    // Exec into AGENT container - opencode is installed there, not on host
    let mut cmd = podman_command();
    cmd.args(["exec", "-it", &format!("{}-agent", pod_name)]);
    
    // opencode inside container talks to localhost:4096 (container's localhost)
    if session_id.is_empty() {
        cmd.args(["opencode", "attach", "http://localhost:4096"]);
    } else {
        cmd.args(["opencode", "attach", "http://localhost:4096", "-s", &session_id]);
    }
    
    cmd.status()?;
    Ok(())
}
```

**Key points:**
- **Host** uses reqwest to call API (no opencode binary needed on host)
- **Exec** runs `opencode attach` inside the agent container (where it's installed)
- Inside the container, `localhost:4096` reaches the opencode server (same container)
- No injected scripts, no Python dependency

### 2. Replace `workspace_monitor.py` with `devaipod monitor`

The monitor functionality moves to the host:

```rust
async fn cmd_monitor(pod_name: &str) -> Result<()> {
    let agent_url = "http://localhost:4096";
    
    loop {
        // Fetch status from host
        let sessions = fetch_sessions(agent_url).await?;
        let root_session = find_root_session(&sessions);
        
        if let Some(session) = root_session {
            let messages = fetch_messages(agent_url, &session.id).await?;
            let status = derive_status(&messages);
            display_status(&session, &status);
        } else {
            println!("Waiting for agent...");
        }
        
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
```

**For Ctrl-C → shell behavior:**

```rust
// Handle SIGINT specially
ctrlc::set_handler(move || {
    // Drop to shell via podman exec
    Command::new("podman")
        .args(["exec", "-it", &workspace_container, "bash"])
        .status()
        .ok();
})?;
```

### 3. Simplify workspace container startup

Currently the workspace container runs `workspace_monitor.py`. Change to:

```rust
// Workspace container just runs bash (or sleeps)
ContainerConfig {
    command: Some(vec!["sleep", "infinity"]),
    // Or just let devcontainer's default CMD run
}
```

The user interacts via:
- `devaipod attach` - connect to agent (with auto-session detection)
- `devaipod ssh` - shell in workspace
- `devaipod monitor` - live status display (optional)

### 4. Send initial task from host

Currently `workspace_monitor.py` sends the initial task. Move this to pod creation:

```rust
// After pod starts, send task if provided
if let Some(task) = &opts.task {
    send_task_to_agent("http://localhost:4096", task).await?;
}
```

### 5. Keep clone scripts (for now)

The git clone scripts are harder to replace because they run during container creation (before the container is fully started). Options:

**Option A: Keep as shell scripts** (short-term)
- They're simple and stable
- Run once at creation, not ongoing

**Option B: Use init container pattern** (medium-term)
- Separate init container clones the repo
- Shares volume with workspace
- More podman-native

**Option C: Clone on host, copy in** (alternative)
- Clone to temp dir on host
- `podman cp` into volume
- Works but slower for large repos

**Recommendation**: Keep clone scripts for now, they're a different category (one-shot setup vs runtime).

## Migration Plan

### Phase 1: Random port + Basic Auth infrastructure
- [ ] Generate random password at pod creation
- [ ] Set `OPENCODE_SERVER_PASSWORD` env var on agent container
- [ ] Publish agent port to random host port: `-p 127.0.0.1::4096`
- [ ] Store password in pod label: `io.devaipod.api-password`
- [ ] Add helper functions: `get_published_port()`, `get_pod_api_password()`
- [ ] Add `call_agent_api()` function using reqwest with Basic Auth

### Phase 2: Host-side session detection
- [ ] Add `find_root_session()` function using the new API access
- [ ] Update `cmd_attach()` to query API from host, then exec `opencode attach`
- [ ] Remove `opencode-connect` script injection

### Phase 3: Host-side monitoring  
- [ ] Add `devaipod monitor <workspace>` command
- [ ] Port status display logic from Python to Rust
- [ ] Handle Ctrl-C → shell handoff
- [ ] Remove `workspace_monitor.py` injection

### Phase 4: Simplify workspace container
- [ ] Change workspace container startup to simple sleep/bash
- [ ] Send initial task from host after pod starts
- [ ] Update attach/ssh to work with simplified container

### Phase 5: (Optional) Improve clone scripts
- [ ] Consider init container pattern for cloning
- [ ] Or keep scripts if they're working fine

## API Access Without Publishing Ports

**Problem:** Publishing the opencode API to a hardcoded host port is problematic:
- No authentication on the API
- Port conflicts with multiple pods
- Security exposure on localhost

**Solution:** Access the API without publishing ports.

### Option A: Unix Socket (Recommended Long-Term)

Have opencode listen on a unix socket mounted to the host:

```
Pod:
  agent container:
    opencode serve --socket /run/devaipod/api.sock
  
  volume: /run/devaipod/ (shared with host)

Host:
  curl --unix-socket ~/.local/share/devaipod/pods/X/api.sock http://localhost/session
```

**Requires:** opencode to support `--socket` option (feature request/PR needed)

### Option B: Random Port + Basic Auth (Recommended)

opencode already supports HTTP Basic Auth via environment variables:
- `OPENCODE_SERVER_PASSWORD` - enables auth when set
- `OPENCODE_SERVER_USERNAME` - defaults to "opencode"

Publish to random port with password in pod labels:

```rust
// Pod creation
let password = generate_random_password();  // e.g., 32 hex chars

// Agent container env
env.insert("OPENCODE_SERVER_PASSWORD", password.clone());

// Publish port (random host port -> container 4096)
args.push("-p");
args.push("127.0.0.1::4096");

// Store password in pod labels for later retrieval
labels.insert("io.devaipod.api-password", password);
```

```rust
// After pod starts, read assigned port from podman
fn get_published_port(pod_name: &str) -> Result<u16> {
    let output = Command::new("podman")
        .args(["port", &format!("{}-agent", pod_name), "4096"])
        .output()?;
    // Parse "127.0.0.1:54321" -> 54321
    let port_str = String::from_utf8_lossy(&output.stdout);
    let port = port_str.trim().split(':').last()
        .and_then(|p| p.parse().ok())
        .context("Failed to parse port")?;
    Ok(port)
}
```

```rust
// Host access with Basic Auth
fn call_agent_api(pod_name: &str, path: &str) -> Result<String> {
    let port = get_published_port(pod_name)?;
    let password = get_pod_label(pod_name, "io.devaipod.api-password")?;
    
    let client = reqwest::Client::new();
    let resp = client.get(&format!("http://127.0.0.1:{}{}", port, path))
        .basic_auth("opencode", Some(&password))
        .send().await?;
    
    Ok(resp.text().await?)
}
```

**This works today!** No changes to opencode needed.

**Note:** nsenter won't work on macOS (containers run in podman machine VM, not directly on host).

### Option C: podman exec (Recommended Short-Term)

Just exec curl into the container:

```rust
fn call_agent_api(pod_name: &str, path: &str) -> Result<String> {
    let output = Command::new("podman")
        .args(["exec", &format!("{}-agent", pod_name),
               "curl", "-sf", &format!("http://127.0.0.1:4096{}", path)])
        .output()?;
    
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
```

Slowest but requires no special setup. Works if curl is in the agent image.

### Recommendation

**Use Option B (Random Port + Basic Auth)** - it works today, is cross-platform (Mac/Linux), and is secure:
- Random port avoids conflicts between multiple pods
- Basic Auth prevents unauthorized access
- Password stored in pod labels, retrieved by devaipod CLI
- Works on macOS (unlike nsenter)

## Architecture Note: No opencode on Host

The `devaipod` binary does NOT require opencode installed on the host. The split is:

| Operation | Where it runs | How |
|-----------|---------------|-----|
| API queries (session list, status) | Host | reqwest HTTP calls with Basic Auth |
| `opencode attach` (interactive TUI) | Agent container | `podman exec ... opencode attach` |
| `opencode run` (send task) | Agent container | `podman exec ... opencode run` |

This means:
- devaipod binary uses reqwest (already a dependency) for API calls
- Interactive commands exec into the container where opencode is installed
- No opencode installation needed on Mac/Windows/Linux host

## API Endpoints Used

The host needs to call these opencode API endpoints (via reqwest + Basic Auth):

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/session` | GET | List all sessions (for finding root session) |
| `/session/{id}/message` | GET | Get messages for status display |
| `/session/status` | GET | Get busy/idle status (if available) |

For sending tasks, exec into the agent container:
```bash
podman exec -it pod-agent opencode run --attach http://localhost:4096 "the task"
```

## Benefits

1. **No Python dependency** in containers
2. **Single source of truth** - all logic in Rust binary
3. **Easier testing** - can unit test Rust code
4. **Cleaner containers** - just run the services they need
5. **Better error handling** - Rust's Result vs shell exit codes
6. **Consistent behavior** - no environment-dependent scripts

## Open Questions

1. **Label security**: Pod labels are visible to anyone who can run `podman pod inspect`. For multi-user systems, consider using podman secrets instead of labels for the password.

2. **Port discovery timing**: After `podman pod start`, there may be a brief delay before the port is assigned. Need to handle this gracefully.

3. **Password rotation**: If a pod is long-lived, should we support rotating the API password?
