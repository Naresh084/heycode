#!/usr/bin/env python3
"""Verify real Claude SDK host tools and approvals in a temporary workspace.

Run explicitly with --live and an existing Claude login. This uses the real
Claude subscription; --fake only supplies the otherwise unused native provider
while the app-server selects Claude. No saved heycode settings are changed.

The test requests only `ls`, a generated file read, and a session task update.
It checks executed host-tool results, not merely a successful model response.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
from pathlib import Path
import tempfile

MARKER = "cobalt-pine-42"
TASK = "Verify sample.txt"


def allowed_permission(event: dict, workspace: Path) -> bool:
    """Approve only the exact operations created by this smoke test."""
    action = event.get("action", "")
    detail = event.get("detail", "")
    try:
        arguments = json.loads(detail.split("input: ", 1)[1])
    except (IndexError, ValueError):
        arguments = {}
    # Agent approval uses its human-readable argument preview; Claude's own
    # callback uses JSON. Both must match the same bounded test operations.
    if not arguments and action == "todo_write" and detail.startswith("todos: "):
        try:
            arguments = {"todos": json.loads(detail.removeprefix("todos: "))}
        except ValueError:
            return False
    if not arguments and action == "read" and detail.startswith("path: "):
        arguments = {"path": detail.removeprefix("path: ")}
    if "todo_write" in action:
        return arguments.get("todos") in (
            [{"content": TASK, "status": "in_progress"}],
            [{"content": TASK, "status": "completed"}],
        )
    if "bash" in action:
        return arguments.get("command") == "ls" or detail.strip() == "command: ls"
    if "read" in action:
        path = arguments.get("path")
        return path in ("sample.txt", str(workspace / "sample.txt"))
    return False


async def verify(binary: Path) -> dict:
    with tempfile.TemporaryDirectory(prefix="heycode-claude-smoke-") as temporary:
        root = Path(temporary).resolve()
        home, workspace = root / "home", root / "workspace"
        home.mkdir()
        workspace.mkdir()
        (workspace / "sample.txt").write_text(f"heycode tool verification: {MARKER}\n")
        environment = dict(os.environ, HEYCODE_HOME=str(home))
        process = await asyncio.create_subprocess_exec(
            str(binary), "--fake", "--approval", "ask", "app-server", "--stdio-v1",
            "--workspace", str(workspace), cwd=workspace, env=environment,
            stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        stderr = asyncio.create_task(process.stderr.read())
        operation = 0
        session_id = None
        events: list[dict] = []
        approvals = 0
        denied: list[str] = []

        async def send(method: str, params: dict) -> int:
            nonlocal operation
            operation += 1
            frame = {"kind": "request", "operation": operation, "request": {
                "jsonrpc": "2.0", "id": operation, "method": method, "params": params,
            }}
            process.stdin.write((json.dumps(frame) + "\n").encode())
            await process.stdin.drain()
            return operation

        async def request(method: str, params: dict):
            nonlocal approvals
            expected = await send(method, params)
            while True:
                raw = await asyncio.wait_for(process.stdout.readline(), 90)
                if not raw:
                    raise RuntimeError(f"app-server closed during {method}")
                frame = json.loads(raw)
                if frame.get("kind") == "notification":
                    event = frame["notification"].get("params", {}).get("event", {})
                    events.append(event)
                    if event.get("type") == "permission_requested":
                        allowed = allowed_permission(event, workspace)
                        if allowed:
                            approvals += 1
                        else:
                            denied.append(event.get("action", "unknown"))
                        await send("session/permission/respond", {
                            "sessionId": session_id, "requestId": event["request_id"],
                            "decision": "allow_once" if allowed else "deny",
                        })
                if frame.get("kind") == "response":
                    response = frame["response"]
                    if "error" in response:
                        raise RuntimeError(f"app-server error: {response['error']}")
                    if frame.get("operation") == expected:
                        return response.get("result")

        try:
            await request("initialize", {})
            await request("runtime/select", {"runtime": "claude"})
            opened = await request("session/open", {
                "configuration": {"model": "opus", "reasoningEffort": "low"},
            })
            session_id = opened["sessionId"]
            if opened["runtimeId"] != "claude":
                raise AssertionError("Claude runtime was not selected")
            result = await request("turn/start", {
                "sessionId": session_id, "attachments": [],
                "text": (
                    "Verify this workspace using heycode host tools, not guesses. First use "
                    f"todo_write with exactly one task '{TASK}' as in_progress. Use bash "
                    "with exactly the command ls. Use read with path sample.txt. Finally "
                    "use todo_write to mark that same task completed. Report the exact "
                    "marker in sample.txt. Do not edit filesystem files or use network."
                ),
            })
            finished = [e for e in events if e.get("type") == "tool_finished"]
            successful = {e["name"] for e in finished if e.get("ok")}
            if result.get("reason") != "stop" or not {"todo_write", "bash", "read"} <= successful:
                raise AssertionError(f"host tools did not complete: {successful}; {result}")
            if any(not e.get("ok") for e in finished) or denied:
                raise AssertionError(f"tool failure or unexpected permission: {denied}")
            read_results = [e["result"] for e in finished if e["name"] == "read"]
            if not any(MARKER in json.dumps(value) for value in read_results):
                raise AssertionError("host read did not return the generated marker")
            shell_results = [json.dumps(e["result"]) for e in finished if e["name"] == "bash"]
            if not any("sample.txt" in value and "[exit code: 0]" in value for value in shell_results):
                raise AssertionError("ls did not list sample.txt with exit code zero")
            todos = [e["result"] for e in finished if e["name"] == "todo_write"]
            if not todos or "completed" not in json.dumps(todos[-1]):
                raise AssertionError("task did not reach completed")
            if approvals != 4:
                raise AssertionError(f"expected four approval callbacks, received {approvals}")
            await request("session/close", {"sessionId": session_id})
            return {"runtime": "claude", "host_tools": sorted(successful),
                    "approval_responses": approvals, "shell_exit_zero": True, "marker_verified": True,
                    "task_completed": True}
        finally:
            process.stdin.close()
            try:
                await asyncio.wait_for(process.wait(), 5)
            except asyncio.TimeoutError:
                process.terminate()
                await asyncio.wait_for(process.wait(), 5)
            await stderr


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--live", action="store_true", help="run against the real Claude SDK")
    parser.add_argument("--binary", type=Path, default=Path("target/debug/heycode"))
    args = parser.parse_args()
    if not args.live:
        parser.error("--live is required")
    print(json.dumps(asyncio.run(verify(args.binary.resolve())), indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
