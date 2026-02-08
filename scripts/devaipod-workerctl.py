#!/usr/bin/env python3
"""
Worker agent control tool for devaipod orchestration.

Provides commands to send tasks to the worker and monitor until completion.
This replaces the pattern of `opencode run --attach --format json` which
returns massive amounts of JSON including all tool calls.

Usage:
    # Send a task and wait for completion (most common)
    worker-ctl send-wait "Your task here. Commit when done."

    # Send a task without waiting (non-blocking)
    worker-ctl send "Your task here"

    # Monitor until idle or timeout
    worker-ctl monitor --timeout 1800

    # Continue a session with follow-up
    worker-ctl send --continue "Fix the typo and commit again"
    worker-ctl monitor

Environment variables:
    OPENCODE_WORKER_URL - Worker OpenCode server URL (default: http://localhost:4098)
"""

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from enum import Enum
from typing import Any


# Default worker URL
DEFAULT_WORKER_URL = "http://localhost:4098"


class AgentActivity(Enum):
    """Agent activity states, matching tui.rs AgentActivity."""
    IDLE = "idle"
    WORKING = "working"
    STOPPED = "stopped"
    UNKNOWN = "unknown"


@dataclass
class AgentState:
    """Rich agent state including activity and recent output."""
    activity: AgentActivity
    recent_output: list[str]
    current_tool: str | None
    status_line: str | None
    last_message_ts: int | None

    def to_dict(self) -> dict[str, Any]:
        return {
            "activity": self.activity.value,
            "recent_output": self.recent_output,
            "current_tool": self.current_tool,
            "status_line": self.status_line,
            "last_message_ts": self.last_message_ts,
        }


def extract_text_from_parts(parts: list[dict], max_lines: int = 5) -> list[str]:
    """Extract text content from message parts, truncating long lines."""
    lines = []

    for part in parts:
        part_type = part.get("type", "")

        if part_type == "text":
            text = part.get("text", "")
            # Take last few lines, truncate each
            for line in reversed(text.splitlines()):
                if len(line) > 120:
                    truncated = line[:117] + "..."
                else:
                    truncated = line
                if truncated.strip():
                    lines.append(truncated)
                if len(lines) >= max_lines:
                    break

        elif part_type == "tool":
            tool_name = part.get("name", "unknown")
            state = part.get("state", {})
            status = state.get("status", "running")
            lines.append(f"-> {tool_name}: {status}")

        if len(lines) >= max_lines:
            break

    lines.reverse()
    return lines


def derive_agent_state_from_messages(messages: list[dict]) -> AgentState:
    """
    Derive agent status (busy/idle) from session messages.

    This mirrors the logic from tui.rs derive_agent_state_from_messages().
    """
    if not messages:
        return AgentState(
            activity=AgentActivity.UNKNOWN,
            recent_output=[],
            current_tool=None,
            status_line=None,
            last_message_ts=None,
        )

    # Find the last assistant message
    last_assistant = None
    for msg in reversed(messages):
        info = msg.get("info", {})
        if info.get("role") == "assistant":
            last_assistant = msg
            break

    if not last_assistant:
        return AgentState(
            activity=AgentActivity.UNKNOWN,
            recent_output=[],
            current_tool=None,
            status_line=None,
            last_message_ts=None,
        )

    info = last_assistant.get("info", {})
    parts = last_assistant.get("parts", [])

    # Extract recent output from parts
    recent_output = extract_text_from_parts(parts)

    # Extract current tool if any is running
    current_tool = None
    for part in parts:
        if part.get("type") == "tool":
            state = part.get("state", {})
            status = state.get("status")
            if status not in ("completed", "error"):
                current_tool = part.get("name")
                break

    # Build status line from first text part
    status_line = None
    for part in parts:
        if part.get("type") == "text":
            text = part.get("text", "")
            first_line = text.split("\n")[0] if text else ""
            if len(first_line) > 80:
                status_line = first_line[:77] + "..."
            else:
                status_line = first_line
            break

    # Determine activity level
    time_info = info.get("time", {})
    if time_info.get("completed") is None:
        activity = AgentActivity.WORKING
    else:
        # Check for incomplete tool calls
        has_incomplete_tool = False
        for part in parts:
            if part.get("type") == "tool":
                state = part.get("state", {})
                status = state.get("status")
                if status not in ("completed", "error"):
                    has_incomplete_tool = True
                    break

        if has_incomplete_tool:
            activity = AgentActivity.WORKING
        else:
            finish = info.get("finish", "")
            if finish == "tool-calls":
                activity = AgentActivity.WORKING
            else:
                activity = AgentActivity.IDLE

    # Extract most recent message timestamp
    last_message_ts = None
    for msg in messages:
        msg_info = msg.get("info", {})
        msg_time = msg_info.get("time", {})
        ts = msg_time.get("completed") or msg_time.get("created")
        if ts and (last_message_ts is None or ts > last_message_ts):
            last_message_ts = ts

    return AgentState(
        activity=activity,
        recent_output=recent_output,
        current_tool=current_tool,
        status_line=status_line,
        last_message_ts=last_message_ts,
    )


def fetch_agent_state(base_url: str, timeout: float = 10) -> AgentState:
    """Fetch agent state by querying the OpenCode API."""
    unknown = AgentState(
        activity=AgentActivity.UNKNOWN,
        recent_output=[],
        current_tool=None,
        status_line=None,
        last_message_ts=None,
    )

    try:
        # Get list of sessions
        sessions_url = f"{base_url}/session"
        req = urllib.request.Request(sessions_url)
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            sessions = json.loads(resp.read().decode("utf-8"))

        if not sessions:
            # No sessions yet - agent is idle/waiting for input
            return AgentState(
                activity=AgentActivity.IDLE,
                recent_output=["Waiting for input..."],
                current_tool=None,
                status_line="Waiting for input...",
                last_message_ts=None,
            )

        # Find the root session (no parent)
        root_session = None
        for s in sessions:
            parent_id = s.get("parentID")
            if parent_id is None or parent_id == "":
                root_session = s
                break

        if not root_session:
            return unknown

        session_id = root_session.get("id")
        if not session_id:
            return unknown

        # Fetch recent messages from the session
        messages_url = f"{base_url}/session/{session_id}/message?limit=10"
        req = urllib.request.Request(messages_url)
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            messages = json.loads(resp.read().decode("utf-8"))

        return derive_agent_state_from_messages(messages)

    except urllib.error.URLError as e:
        # Worker not reachable
        return AgentState(
            activity=AgentActivity.STOPPED,
            recent_output=[f"Worker unreachable: {e.reason}"],
            current_tool=None,
            status_line=f"Error: {e.reason}",
            last_message_ts=None,
        )
    except Exception as e:
        return AgentState(
            activity=AgentActivity.UNKNOWN,
            recent_output=[f"Error: {e}"],
            current_tool=None,
            status_line=f"Error: {e}",
            last_message_ts=None,
        )


@dataclass
class MonitorResult:
    """Result from monitoring the worker."""
    success: bool
    reason: str  # "idle", "timeout", "stopped", "error"
    state: AgentState
    elapsed_seconds: float
    poll_count: int

    def to_dict(self) -> dict[str, Any]:
        return {
            "success": self.success,
            "reason": self.reason,
            "state": self.state.to_dict(),
            "elapsed_seconds": self.elapsed_seconds,
            "poll_count": self.poll_count,
        }


def monitor_until_idle(
    base_url: str,
    timeout_seconds: float,
    poll_interval: float = 3.0,
    verbose: bool = False,
) -> MonitorResult:
    """
    Poll the worker until it becomes idle or timeout is reached.

    Returns a MonitorResult with the final state and reason for stopping.
    """
    start_time = time.time()
    poll_count = 0
    last_state = None

    while True:
        elapsed = time.time() - start_time
        if elapsed >= timeout_seconds:
            final_state = last_state or fetch_agent_state(base_url)
            return MonitorResult(
                success=False,
                reason="timeout",
                state=final_state,
                elapsed_seconds=elapsed,
                poll_count=poll_count,
            )

        state = fetch_agent_state(base_url)
        poll_count += 1
        last_state = state

        if verbose:
            print(
                f"[{elapsed:.1f}s] Activity: {state.activity.value}, "
                f"Tool: {state.current_tool or 'none'}",
                file=sys.stderr,
            )

        if state.activity == AgentActivity.IDLE:
            return MonitorResult(
                success=True,
                reason="idle",
                state=state,
                elapsed_seconds=elapsed,
                poll_count=poll_count,
            )

        if state.activity == AgentActivity.STOPPED:
            return MonitorResult(
                success=False,
                reason="stopped",
                state=state,
                elapsed_seconds=elapsed,
                poll_count=poll_count,
            )

        # Still working, wait before next poll
        time.sleep(poll_interval)


@dataclass
class SendResult:
    """Result from sending a message to the worker."""
    success: bool
    session_id: str | None
    message_id: str | None
    error: str | None

    def to_dict(self) -> dict[str, Any]:
        return {
            "success": self.success,
            "session_id": self.session_id,
            "message_id": self.message_id,
            "error": self.error,
        }


def send_message(base_url: str, message: str, timeout: float = 30) -> SendResult:
    """
    Send a message to the worker via the OpenCode API.

    This posts to /session/message which creates a session if needed.
    Returns immediately (non-blocking) after the message is accepted.
    """
    try:
        url = f"{base_url}/session/message"
        payload = {
            "parts": [{"type": "text", "text": message}]
        }
        data = json.dumps(payload).encode("utf-8")

        req = urllib.request.Request(
            url,
            data=data,
            headers={"Content-Type": "application/json"},
            method="POST",
        )

        with urllib.request.urlopen(req, timeout=timeout) as resp:
            result = json.loads(resp.read().decode("utf-8"))
            return SendResult(
                success=True,
                session_id=result.get("sessionID") or result.get("session_id"),
                message_id=result.get("id"),
                error=None,
            )

    except urllib.error.HTTPError as e:
        error_body = ""
        try:
            error_body = e.read().decode("utf-8")
        except Exception:
            pass
        return SendResult(
            success=False,
            session_id=None,
            message_id=None,
            error=f"HTTP {e.code}: {error_body or e.reason}",
        )
    except urllib.error.URLError as e:
        return SendResult(
            success=False,
            session_id=None,
            message_id=None,
            error=f"Connection error: {e.reason}",
        )
    except Exception as e:
        return SendResult(
            success=False,
            session_id=None,
            message_id=None,
            error=str(e),
        )


def cmd_send(args) -> int:
    """Handle the 'send' command."""
    result = send_message(args.url, args.message)

    if args.json:
        print(json.dumps(result.to_dict(), indent=2))
    else:
        if result.success:
            print(f"Message sent successfully")
            if result.session_id:
                print(f"Session: {result.session_id}")
        else:
            print(f"Failed to send message: {result.error}", file=sys.stderr)

    return 0 if result.success else 1


def cmd_monitor(args) -> int:
    """Handle the 'monitor' command."""
    if args.verbose:
        print(f"Monitoring {args.url} (timeout: {args.timeout}s)", file=sys.stderr)

    result = monitor_until_idle(
        base_url=args.url,
        timeout_seconds=args.timeout,
        poll_interval=args.poll_interval,
        verbose=args.verbose,
    )

    if args.json:
        print(json.dumps(result.to_dict(), indent=2))
    else:
        print(f"Result: {result.reason}")
        print(f"Activity: {result.state.activity.value}")
        print(f"Elapsed: {result.elapsed_seconds:.1f}s ({result.poll_count} polls)")
        if result.state.current_tool:
            print(f"Last tool: {result.state.current_tool}")
        if result.state.status_line:
            print(f"Status: {result.state.status_line}")
        if result.state.recent_output:
            print("Recent output:")
            for line in result.state.recent_output[-5:]:
                print(f"  {line}")

    return 0 if result.success else 1


def cmd_send_wait(args) -> int:
    """Handle the 'send-wait' command - send then monitor."""
    # First send the message
    if args.verbose:
        print(f"Sending message to {args.url}...", file=sys.stderr)

    send_result = send_message(args.url, args.message)
    if not send_result.success:
        error_output = {
            "success": False,
            "reason": "send_failed",
            "error": send_result.error,
        }
        if args.json:
            print(json.dumps(error_output, indent=2))
        else:
            print(f"Failed to send message: {send_result.error}", file=sys.stderr)
        return 1

    if args.verbose:
        print(f"Message sent, monitoring...", file=sys.stderr)

    # Small delay to let the worker start processing
    time.sleep(0.5)

    # Now monitor until idle
    result = monitor_until_idle(
        base_url=args.url,
        timeout_seconds=args.timeout,
        poll_interval=args.poll_interval,
        verbose=args.verbose,
    )

    # Include send info in output
    output = result.to_dict()
    output["session_id"] = send_result.session_id

    if args.json:
        print(json.dumps(output, indent=2))
    else:
        print(f"Result: {result.reason}")
        print(f"Activity: {result.state.activity.value}")
        print(f"Elapsed: {result.elapsed_seconds:.1f}s ({result.poll_count} polls)")
        if result.state.current_tool:
            print(f"Last tool: {result.state.current_tool}")
        if result.state.status_line:
            print(f"Status: {result.state.status_line}")
        if result.state.recent_output:
            print("Recent output:")
            for line in result.state.recent_output[-5:]:
                print(f"  {line}")

    return 0 if result.success else 1


def cmd_status(args) -> int:
    """Handle the 'status' command - get current worker state."""
    state = fetch_agent_state(args.url)

    if args.json:
        print(json.dumps(state.to_dict(), indent=2))
    else:
        print(f"Activity: {state.activity.value}")
        if state.current_tool:
            print(f"Current tool: {state.current_tool}")
        if state.status_line:
            print(f"Status: {state.status_line}")
        if state.recent_output:
            print("Recent output:")
            for line in state.recent_output[-5:]:
                print(f"  {line}")

    return 0


def main():
    parser = argparse.ArgumentParser(
        prog="devaipod-workerctl",
        description="Worker agent control tool for devaipod orchestration",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    parser.add_argument(
        "--url",
        default=os.environ.get("OPENCODE_WORKER_URL", DEFAULT_WORKER_URL),
        help=f"Worker OpenCode server URL (default: $OPENCODE_WORKER_URL or {DEFAULT_WORKER_URL})",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Output as JSON",
    )
    parser.add_argument(
        "--verbose", "-v",
        action="store_true",
        help="Verbose output",
    )

    subparsers = parser.add_subparsers(dest="command", help="Command to run")

    # send command
    send_parser = subparsers.add_parser(
        "send",
        help="Send a message to the worker (non-blocking)",
    )
    send_parser.add_argument(
        "message",
        help="Message to send to the worker",
    )
    send_parser.set_defaults(func=cmd_send)

    # monitor command
    monitor_parser = subparsers.add_parser(
        "monitor",
        help="Monitor worker until idle or timeout",
    )
    monitor_parser.add_argument(
        "--timeout",
        type=float,
        default=1800,  # 30 minutes
        help="Maximum time to wait in seconds (default: 1800 = 30 minutes)",
    )
    monitor_parser.add_argument(
        "--poll-interval",
        type=float,
        default=3.0,
        help="Seconds between polls (default: 3)",
    )
    monitor_parser.set_defaults(func=cmd_monitor)

    # send-wait command (send + monitor)
    send_wait_parser = subparsers.add_parser(
        "send-wait",
        help="Send a message and wait for worker to complete",
    )
    send_wait_parser.add_argument(
        "message",
        help="Message to send to the worker",
    )
    send_wait_parser.add_argument(
        "--timeout",
        type=float,
        default=1800,  # 30 minutes
        help="Maximum time to wait in seconds (default: 1800 = 30 minutes)",
    )
    send_wait_parser.add_argument(
        "--poll-interval",
        type=float,
        default=3.0,
        help="Seconds between polls (default: 3)",
    )
    send_wait_parser.set_defaults(func=cmd_send_wait)

    # status command
    status_parser = subparsers.add_parser(
        "status",
        help="Get current worker status",
    )
    status_parser.set_defaults(func=cmd_status)

    args = parser.parse_args()

    if not args.command:
        parser.print_help()
        return 1

    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
