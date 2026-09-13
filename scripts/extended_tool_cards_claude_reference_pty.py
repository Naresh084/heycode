#!/usr/bin/env python3
"""Capture synthetic extended Claude tool cards with kernel-enforced loopback-only networking.

Every Claude process uses a disposable HOME/config/workspace, a minimal environment,
the pinned reference executable, and a sandbox profile allowing only the fixture port.
No credentials are inherited. Unsupported tools are recorded as unavailable; their
errors are not accepted as successful tool references. Imported Screen and hashing
match the core capture technique; terminal output is always from the installed CLI.
"""
from __future__ import annotations

import argparse
import fcntl
import http.server
import json
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
import uuid

from core_tool_cards_claude_reference_pty import Screen, _sha256
from terminal_screenshot import TerminalByteStream, render_screen

PIN = "c942e1228b93cb4d52183b3dfbc77f28264f35aa947acd9c0853d029164cf450"
MARKER = "SYNTHETIC_LOCAL_REFERENCE_DONE"


def run(output: Path, case: str, command: str | None, timeout: float) -> dict:
    output.mkdir(parents=True, exist_ok=False)
    executable = Path(shutil.which("claude") or "/Users/naresh/.local/bin/claude").resolve()
    if _sha256(executable) != PIN:
        raise RuntimeError("Installed Claude differs from pinned reference")
    requests, captures, raw = [], [], bytearray()
    state = {"steps": [], "work": None, "tools": [], "subrequests": 0}

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
            tools = body.get("tools", [])
            if not state["tools"]:
                state["tools"] = tools
            blocks = [block for message in body.get("messages", [])
                      for block in (message.get("content", []) if isinstance(message.get("content"), list) else [])
                      if isinstance(block, dict)]
            results = [block for block in blocks if block.get("type") == "tool_result"]
            # Agent child calls receive a plain synthetic response, never the parent sequence.
            main = any("controlled extended tool-UI capture" in str(message.get("content", ""))
                       for message in body.get("messages", []))
            index = len([r for r in results if str(r.get("tool_use_id", "")).startswith("toolu_extended_")])
            requests.append({"path": self.path, "main": main, "result_count": index,
                             "tool_names": [t.get("name") for t in tools], "tool_results": results})
            if "count_tokens" in self.path:
                data = b'{"input_tokens":0}'
                self.send_response(200); self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data))); self.end_headers(); self.wfile.write(data)
                return
            self.send_response(200); self.send_header("Content-Type", "text/event-stream"); self.end_headers()
            def event(kind, value):
                self.wfile.write(f"event: {kind}\ndata: {json.dumps(value)}\n\n".encode())
            event("message_start", {"type":"message_start", "message": {
                "id":f"msg_local_{len(requests)}", "type":"message", "role":"assistant",
                "model":"claude-opus-5", "content":[], "stop_reason":None, "stop_sequence":None,
                "usage":{"input_tokens":0,"output_tokens":0}}})
            if main and index < len(state["steps"]):
                name, arguments = state["steps"][index]
                event("content_block_start", {"type":"content_block_start","index":0,
                      "content_block":{"type":"tool_use","id":f"toolu_extended_{index}","name":name,"input":{}}})
                event("content_block_delta", {"type":"content_block_delta","index":0,
                      "delta":{"type":"input_json_delta","partial_json":json.dumps(arguments)}})
                stop = "tool_use"
            else:
                event("content_block_start", {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}})
                event("content_block_delta", {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":MARKER}})
                stop = "end_turn"
            event("content_block_stop", {"type":"content_block_stop","index":0})
            event("message_delta", {"type":"message_delta","delta":{"stop_reason":stop,"stop_sequence":None},"usage":{"output_tokens":0}})
            event("message_stop", {"type":"message_stop"}); self.wfile.flush()

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
    proc = None; master = None
    result = {"status":"blocked", "case":case, "command":command, "synthetic_fixture":True,
              "external_inference":False, "external_network":"kernel sandbox denies all except fixture loopback port",
              "credential_inheritance":False, "binary_sha256":PIN, "version":"2.1.269", "viewport":[110,42]}
    try:
        with tempfile.TemporaryDirectory(prefix="claude-extended-reference-") as folder:
            root = Path(folder)
            for name in ("home", "config", "work"): (root/name).mkdir()
            work = root/"work"; state["work"] = work
            (work/"sample.py").write_text("def sample():\n    return 42\n")
            (work/"sample.ipynb").write_text(json.dumps({"nbformat":4,"nbformat_minor":5,"metadata":{},"cells":[{"cell_type":"code","id":"fixture-cell","metadata":{},"source":["print('before')\n"],"outputs":[],"execution_count":None}]}))
            (root/"config/.claude.json").write_text(json.dumps({"hasCompletedOnboarding":True,"theme":"dark","lastOnboardingVersion":"2.1.269"}))
            cases = {
                "discover":[],
                "catalog":[],
                "tasks":[("TaskCreate",{"subject":"Inspect local fixture","description":"Synthetic task for UI capture","activeForm":"Inspecting local fixture"}), ("TaskUpdate",{"taskId":"1","status":"in_progress"}), ("TaskList",{}), ("TaskUpdate",{"taskId":"1","status":"completed"})],
                "agent":[("Agent",{"description":"Inspect synthetic marker","prompt":"Reply SYNTHETIC_CHILD_DONE only; do not call tools.","subagent_type":"general-purpose"})],
                "background":[("Bash",{"command":"sleep 3; printf 'SYNTHETIC_BACKGROUND_DONE\\n'","description":"Run local background fixture","run_in_background":True})],
                "notebook":[("Read",{"file_path":str(work/"sample.ipynb")}), ("NotebookEdit",{"notebook_path":str(work/"sample.ipynb"),"cell_id":"fixture-cell","new_source":"print('synthetic after')","cell_type":"code","edit_mode":"replace"})],
                "lsp":[("LSP",{"operation":"documentSymbol","filePath":str(work/"sample.py"),"line":1,"character":1})],
                "web":[("WebFetch",{"url":f"http://127.0.0.1:{server.server_port}/synthetic","prompt":"Summarize the synthetic local fixture."}), ("WebSearch",{"query":"SYNTHETIC_LOCAL_REFERENCE_NO_EXTERNAL_REQUEST_ALLOWED"})],
                "websearch":[("WebSearch",{"query":"SYNTHETIC_LOCAL_REFERENCE_NO_EXTERNAL_REQUEST_ALLOWED"})],
                "resources":[("ListMcpResourcesTool",{}), ("ReadMcpResourceTool",{"server":"synthetic","uri":"fixture://local/sample"})],
                "mcp":[("ListMcpResourcesTool",{}), ("ReadMcpResourceTool",{"server":"synthetic","uri":"fixture://local/sample"}), ("mcp__synthetic__echo_fixture",{})],
                "monitor":[("Monitor",{"command":"printf 'SYNTHETIC_MONITOR_DONE\\n'","description":"Observe local synthetic marker"})],
                "workflow":[("Workflow",{"script":"export const meta = { name: 'synthetic-reference', description: 'Return a synthetic local fixture', phases: [] }; return { marker: 'SYNTHETIC_WORKFLOW_DONE' };"})],
                "worktree":[("EnterWorktree",{"name":"synthetic-reference"}), ("ExitWorktree",{"action":"keep"})],
                "plan":[("EnterPlanMode",{}), ("ExitPlanMode",{})],
                "findings":[("ReportFindings",{"level":"medium","findings":[{"file":"sample.py","line":2,"summary":"Synthetic example finding for UI inspection only","short_summary":"Synthetic fixture finding","failure_scenario":"Synthetic example only; no actual defect was found.","category":"synthetic"}]})],
                "file":[("SendUserFile",{"file_path":str(work/"sample.py")})],
                "question":[("AskUserQuestion",{"questions":[{"question":"Which synthetic fixture should be selected?","header":"Fixture","options":[{"label":"Alpha","description":"First local fixture"},{"label":"Beta","description":"Second local fixture"}],"multiSelect":False}]})],
            }
            state["steps"] = cases[case]
            profile = root/"network.sb"
            profile.write_text(f'(version 1)\n(allow default)\n(deny network*)\n(allow network-outbound (remote ip "localhost:{server.server_port}"))\n')
            environment = {"PATH":"/opt/homebrew/bin:/usr/bin:/bin", "HOME":str(root/"home"),
                "CLAUDE_CONFIG_DIR":str(root/"config"), "TERM":"xterm-256color","COLORTERM":"truecolor",
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC":"1", "ANTHROPIC_API_KEY":"local-only-dummy",
                "ANTHROPIC_BASE_URL":f"http://127.0.0.1:{server.server_port}","HTTP_PROXY":"http://127.0.0.1:9",
                "HTTPS_PROXY":"http://127.0.0.1:9","ALL_PROXY":"http://127.0.0.1:9","NO_PROXY":"127.0.0.1,localhost",
                "BROWSER":"/usr/bin/false","EDITOR":"/usr/bin/false","VISUAL":"/usr/bin/false"}
            (output/"network-sandbox.sb").write_text(profile.read_text())
            # The documentation-only TEST-NET address cannot receive a packet:
            # sandbox-exec must reject connect() with EPERM before Claude starts.
            probe_code = (
                "import json,socket; "
                "remote=socket.socket(); remote.settimeout(1); "
                "blocked=remote.connect_ex(('192.0.2.1',443)); remote.close(); "
                "local=socket.socket(); local.settimeout(1); "
                f"allowed=local.connect_ex(('127.0.0.1',{server.server_port})); local.close(); "
                "print(json.dumps({'blocked_connect_errno':blocked,'loopback_connect_errno':allowed})); "
                "assert blocked==1 and allowed==0"
            )
            probe = subprocess.run(["/usr/bin/sandbox-exec","-f",str(profile),sys.executable,"-c",probe_code],
                                   env=environment,capture_output=True,text=True,timeout=4,check=True)
            result["network_enforcement_probe"] = json.loads(probe.stdout)
            args = ["/usr/bin/sandbox-exec","-f",str(profile),str(executable),"--safe-mode","--strict-mcp-config","--no-chrome",
                    "--setting-sources","project,local","--settings",'{"remoteControlAtStartup":false}',"--permission-mode","manual",
                    "--session-id",str(uuid.uuid4()),"--name","isolated-extended-tool-reference","--model","opus"]
            if case == "catalog":
                args.remove("--safe-mode")
            if case == "mcp":
                mcp_config = root/"mcp.json"
                mcp_config.write_text(json.dumps({"mcpServers":{"synthetic":{"type":"stdio","command":sys.executable,"args":[str(Path(__file__).parent.resolve()/"terminal_reference_mcp_fixture.py")]}}}))
                args.remove("--safe-mode")
                args.extend(["--mcp-config",str(mcp_config)])
            result["safe_mode"] = "--safe-mode" in args
            master, slave = pty.openpty(); fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack("HHHH",42,110,0,0))
            proc = subprocess.Popen(args,cwd=work,env=environment,stdin=slave,stdout=slave,stderr=slave,start_new_session=True)
            os.close(slave); screen = Screen(110,42); stream = TerminalByteStream(screen)
            def read(seconds=.2):
                deadline=time.monotonic()+seconds
                while time.monotonic()<deadline:
                    ready,_,_=select.select([master],[],[],min(.05,max(0,deadline-time.monotonic())))
                    if ready:
                        try: data=os.read(master,65536)
                        except OSError: break
                        if not data: break
                        raw.extend(data); stream.feed(data)
                return "\n".join(screen.display)
            def send(data): os.write(master,data); read(.15)
            def capture(name):
                content=read(.2); (output/f"{name}.txt").write_text(content)
                render_screen(screen,output/f"{name}.png"); captures.append(name); return content
            deadline=time.monotonic()+25
            while time.monotonic()<deadline:
                current=read(.3)
                if "custom API key" in current: send(b"\x1b[A\r")
                elif "trust this folder" in current.lower(): send(b"\x1b[B\r")
                elif "for shortcuts" in current or "shift+tab" in current: break
                elif proc.poll() is not None: raise RuntimeError(current)
            capture("00-ready")
            prompt = command or "This is a controlled extended tool-UI capture using synthetic local fixture responses."
            send(prompt.encode()); read(.3); capture("01-input"); send(b"\r")
            deadline=time.monotonic()+timeout; previous=""; approvals=0
            while time.monotonic()<deadline:
                current=read(.25)
                signature=str(len(requests))+str("Do you want to" in current)
                if signature != previous:
                    capture(f"{len(captures):02d}-state"); previous=signature
                if MARKER in current: break
                if "Do you want to" in current or "Run a dynamic workflow?" in current or "Exit plan mode?" in current:
                    capture(f"{len(captures):02d}-approval"); approvals+=1
                    if case in ("web","websearch"): send(b"\x1b"); break
                    send(b"1")
                if case == "question" and "Which synthetic" in current: capture("question-open"); send(b"\x1b"); break
                if command and time.monotonic()>deadline-timeout+4 and not requests: break
                if proc.poll() is not None: break
            capture("90-settled"); send(b"\x0f"); read(.4); capture("91-expanded")
            for page in range(2): send(b"\x1b[5~"); capture(f"92-scrollback-{page}")
            names={tool.get("name") for tool in state["tools"]}
            result.update(status="captured", captures=captures, approvals=approvals, local_requests=len(requests),
                          requested_tools=[n for n,_ in state["steps"]] if not command else [],
                          tool_catalog_observed=bool(names),
                          unavailable_in_tool_catalog=[n for n,_ in state["steps"] if n not in names] if names and not command else [],
                          completion_marker_seen=MARKER in "\n".join((output/f"{c}.txt").read_text() for c in captures),
                          fixture_origin=f"http://127.0.0.1:{server.server_port}")
    except Exception as error:
        result.update(error=f"{type(error).__name__}: {error}")
    finally:
        if proc is not None and proc.poll() is None: os.killpg(proc.pid,signal.SIGTERM)
        if proc is not None:
            try: proc.wait(timeout=3)
            except subprocess.TimeoutExpired: os.killpg(proc.pid,signal.SIGKILL); proc.wait()
        if master is not None: os.close(master)
        server.shutdown(); server.server_close(); thread.join(timeout=2)
        (output/"terminal.ansi").write_bytes(raw)
        (output/"fixture-requests.json").write_text(json.dumps(requests,indent=2))
        (output/"tool-catalog.json").write_text(json.dumps(state["tools"],indent=2))
        (output/"result.json").write_text(json.dumps(result,indent=2)+"\n")
    return result


if __name__ == "__main__":
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output",type=Path,required=True); parser.add_argument("--case",default="discover")
    parser.add_argument("--command"); parser.add_argument("--timeout",type=float,default=25)
    args=parser.parse_args(); print(json.dumps(run(args.output,args.case,args.command,args.timeout)))
