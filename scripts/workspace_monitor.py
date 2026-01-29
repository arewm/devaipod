#!/usr/bin/env python3
"""Workspace monitor for devaipod - shows agent status and enables handoff.

This script runs in the workspace container and:
1. Polls the opencode API for session status
2. Displays live status updates
3. Handles Ctrl-C to drop to an interactive shell

Designed for testability: state fetching, processing, and display are separate.
Requires only Python stdlib.
"""
import json
import os
import signal
import sys
import time
import urllib.request
import urllib.error
from datetime import datetime, timedelta

AGENT_URL = os.environ.get("DEVAIPOD_AGENT_URL", "http://localhost:4096")
POLL_INTERVAL = 2  # seconds

# ANSI colors
GREEN = "\033[32m"
YELLOW = "\033[33m"
RED = "\033[31m"
BLUE = "\033[34m"
BOLD = "\033[1m"
RESET = "\033[0m"


def fetch_sessions():
    """Fetch all sessions from opencode API."""
    try:
        req = urllib.request.urlopen(f"{AGENT_URL}/session", timeout=5)
        return json.loads(req.read())
    except (urllib.error.URLError, OSError, json.JSONDecodeError):
        return None


def fetch_session_status():
    """Fetch session status (busy/idle) from opencode API.
    
    Note: The /session/status endpoint currently returns empty {}.
    We derive status from messages instead - see derive_status_from_messages().
    """
    try:
        req = urllib.request.urlopen(f"{AGENT_URL}/session/status", timeout=5)
        return json.loads(req.read())
    except (urllib.error.URLError, OSError, json.JSONDecodeError):
        return None


def derive_status_from_messages(messages):
    """Derive agent status (busy/idle) from message data.
    
    This is the primary status detection method since /session/status returns {}.
    
    We check the last assistant message for:
    - time.completed: if absent, agent is still processing
    - finish: if "tool-calls", agent will continue (but may be between calls)
    - parts with type="tool" and state.status != "completed": tool in progress
    
    Returns: "busy", "idle", or "unknown"
    """
    if not messages:
        return "unknown"
    
    # Find the last assistant message
    last_assistant = None
    for msg in reversed(messages):
        if msg.get("info", {}).get("role") == "assistant":
            last_assistant = msg
            break
    
    if not last_assistant:
        return "unknown"
    
    info = last_assistant.get("info", {})
    
    # Check if message is still being processed (no completed time)
    if "completed" not in info.get("time", {}):
        return "busy"
    
    # Check if there are any incomplete tool calls in parts
    for part in last_assistant.get("parts", []):
        if part.get("type") == "tool":
            state = part.get("state", {})
            if state.get("status") not in ("completed", "error"):
                return "busy"
    
    # Message completed - check finish reason
    finish = info.get("finish", "")
    if finish == "stop":
        return "idle"
    elif finish == "tool-calls":
        # Agent made tool calls but those are done; waiting for next turn
        # This is a brief transitional state
        return "busy"
    
    return "idle"


def fetch_recent_messages(session_id, limit=3):
    """Fetch recent messages from a session."""
    try:
        req = urllib.request.urlopen(
            f"{AGENT_URL}/session/{session_id}/message?limit={limit}", timeout=5
        )
        return json.loads(req.read())
    except (urllib.error.URLError, OSError, json.JSONDecodeError):
        return None


def create_session_with_task(task):
    """Create a new session and send the initial task.
    
    Uses 'opencode run --attach' to send the task to the agent server.
    Returns True if successful, False otherwise.
    """
    import subprocess
    
    print(f"{BOLD}devaipod{RESET}: Sending initial task to agent...")
    
    # Use opencode run --attach to send the task
    # This creates a session and sends the message
    try:
        result = subprocess.run(
            ["opencode", "run", "--attach", AGENT_URL, task],
            capture_output=True,
            text=True,
            timeout=30,  # Give it time to connect and send
        )
        if result.returncode == 0:
            print(f"{GREEN}Task sent successfully{RESET}")
            return True
        else:
            print(f"{YELLOW}Warning: opencode run exited with code {result.returncode}{RESET}")
            if result.stderr:
                print(f"  {result.stderr[:200]}")
            return False
    except subprocess.TimeoutExpired:
        print(f"{YELLOW}Warning: Timed out sending task (agent may still process it){RESET}")
        return False
    except FileNotFoundError:
        print(f"{RED}Error: opencode not found in PATH{RESET}")
        return False
    except Exception as e:
        print(f"{YELLOW}Warning: Failed to send task: {e}{RESET}")
        return False


def wait_for_agent_ready(timeout=60):
    """Wait for the agent server to be ready.
    
    Returns True when agent is responding, False on timeout.
    """
    start = time.time()
    while time.time() - start < timeout:
        try:
            req = urllib.request.urlopen(f"{AGENT_URL}/session", timeout=2)
            # If we get a response (even empty list), agent is ready
            json.loads(req.read())
            return True
        except (urllib.error.URLError, OSError, json.JSONDecodeError):
            time.sleep(1)
    return False


def find_root_session(sessions):
    """Find the main task session (no parent)."""
    if not sessions:
        return None
    root_sessions = [s for s in sessions if s.get("parentID") is None]
    if not root_sessions:
        return None
    root_sessions.sort(key=lambda s: s.get("time", {}).get("created", 0))
    return root_sessions[0]


def format_duration(start_ms):
    """Format duration since start time."""
    if not start_ms:
        return "unknown"
    elapsed = datetime.now() - datetime.fromtimestamp(start_ms / 1000)
    if elapsed < timedelta(minutes=1):
        return f"{int(elapsed.total_seconds())}s"
    elif elapsed < timedelta(hours=1):
        return f"{int(elapsed.total_seconds() / 60)}m"
    else:
        hours = int(elapsed.total_seconds() / 3600)
        mins = int((elapsed.total_seconds() % 3600) / 60)
        return f"{hours}h {mins}m"


def get_status_display(status_type):
    """Get colored status display."""
    if status_type == "busy":
        return f"{GREEN}WORKING{RESET}"
    elif status_type == "idle":
        return f"{BLUE}IDLE{RESET}"
    else:
        return f"{YELLOW}{status_type.upper()}{RESET}"


def get_last_activity(messages):
    """Extract last activity summary from messages."""
    if not messages:
        return None
    for msg in messages:
        if msg.get("info", {}).get("role") == "assistant":
            parts = msg.get("parts", [])
            for part in parts:
                if part.get("type") == "text":
                    text = part.get("text", "")
                    # Truncate to first line or 80 chars
                    first_line = text.split("\n")[0][:80]
                    if len(first_line) < len(text.split("\n")[0]):
                        first_line += "..."
                    return first_line
    return None


def clear_line():
    """Clear current line."""
    sys.stdout.write("\r\033[K")
    sys.stdout.flush()


def display_status(session, status, messages, iteration):
    """Display current status. Returns the output string for testing."""
    lines = []

    # Spinner animation
    spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][iteration % 10]

    if session is None:
        lines.append(f"{YELLOW}{spinner} Waiting for agent...{RESET}")
    else:
        session_id = session.get("id", "unknown")
        title = session.get("title", "")[:50]
        created = session.get("time", {}).get("created")
        duration = format_duration(created)

        # Try /session/status first, fall back to deriving from messages
        status_type = (
            status.get(session_id, {}).get("type") if status else None
        )
        if not status_type:
            # Derive status from message data (primary method)
            status_type = derive_status_from_messages(messages)
        status_display = get_status_display(status_type)

        lines.append(f"{BOLD}devaipod agent monitor{RESET}")
        lines.append("")
        lines.append(f"  Status:   {spinner} {status_display} ({duration} elapsed)")
        lines.append(f"  Session:  {session_id[:20]}...")
        if title:
            lines.append(f"  Task:     {title}")

        activity = get_last_activity(messages)
        if activity:
            lines.append(f"  Activity: {activity}")

        lines.append("")
        lines.append(
            f"{BLUE}Press Ctrl-C for shell, or run 'opencode-connect' to interact{RESET}"
        )

    return "\n".join(lines)


def run_shell():
    """Drop to interactive shell."""
    shell = os.environ.get("SHELL", "/bin/bash")
    print(
        f"\n{GREEN}Dropping to shell. Run 'opencode-connect' to interact with agent.{RESET}\n"
    )
    os.execv(shell, [shell])


def main():
    # Handle Ctrl-C to drop to shell instead of exit
    def handle_sigint(sig, frame):
        run_shell()

    signal.signal(signal.SIGINT, handle_sigint)
    signal.signal(signal.SIGTERM, lambda s, f: (print("\nShutting down..."), sys.exit(0)))

    print(f"{BOLD}devaipod{RESET}: Starting workspace monitor...")
    print(f"{BOLD}devaipod{RESET}: Agent at {AGENT_URL}")
    print()

    # Check for initial task from environment
    initial_task = os.environ.get("DEVAIPOD_TASK")
    task_sent = False

    if initial_task:
        print(f"{BOLD}devaipod{RESET}: Waiting for agent to be ready...")
        if wait_for_agent_ready(timeout=60):
            # Check if any sessions already exist (task already sent in previous run)
            sessions = fetch_sessions()
            if sessions:
                print(f"{BOLD}devaipod{RESET}: Session already exists, skipping task")
                task_sent = True
            else:
                # No sessions yet, send the task
                task_sent = create_session_with_task(initial_task)
        else:
            print(f"{RED}Error: Agent not ready after 60s, skipping task{RESET}")
        print()

    iteration = 0
    last_output_lines = 0

    while True:
        sessions = fetch_sessions()
        session = find_root_session(sessions)
        status = fetch_session_status()
        messages = None
        if session:
            messages = fetch_recent_messages(session.get("id"), limit=3)

        # Move cursor up to overwrite previous output
        if last_output_lines > 0:
            sys.stdout.write(f"\033[{last_output_lines}A")

        output = display_status(session, status, messages, iteration)
        print(output)
        last_output_lines = output.count("\n") + 1

        iteration += 1
        time.sleep(POLL_INTERVAL)


if __name__ == "__main__":
    main()
