#!/usr/bin/env python3
"""Real native workflow + local HTTP fixture, with actual terminal-cell PNG evidence."""
from __future__ import annotations
import argparse
from datetime import datetime, timezone
import fcntl
import hashlib
import http.server
import json
import os
import re
from pathlib import Path
import struct
import tempfile
import termios
import threading
import time
import pyte
from tui_blackbox import FullScreenTui
from task_console_pty import MODEL, Screen
from terminal_screenshot import render_screen

ASSIGNMENTS = {
    "reliability": ("Research reliability", "Review reliability: compare failure recovery, data integrity, and observability. Report concise evidence."),
    "performance": ("Research performance", "Review performance: compare streaming latency, throughput, and resource limits. Report concise evidence."),
    "usability": ("Research usability", "Review usability: compare navigation, accessibility, and recovery paths. Report concise evidence."),
}
UPDATES = {
    "reliability": "Comparing recovery behavior and checking durable state. The core paths preserve completed work across interruption.",
    "performance": "Measuring streaming behavior and checking concurrency limits. Three independent reviews are running together.",
    "usability": "Reviewing navigation and accessibility. Each phase and agent needs a clear status and a reliable way back.",
}

def definition(before=False, name="release-readiness", title="Release readiness"):
    phases = [{"id": "discover", "title": "Set the scope"}, {"id": "analyze", "title": "Review in parallel"}, {"id": "deliver", "title": "Deliver findings"}]
    steps = [{"id":"scope","phase_id":"discover","label":"Define review scope","action":{"kind":"emit","value":"Review reliability, performance, and usability."}}]
    steps += [{"id":key,"phase_id":"analyze","label":label,"depends_on":["scope"],"action":{"kind":"agent","prompt":prompt}} for key,(label,prompt) in ASSIGNMENTS.items()]
    steps += [{"id":"report","phase_id":"deliver","label":"Write the review report","depends_on":list(ASSIGNMENTS),"action":{"kind":"tool","name":"write","arguments":{"path":"workflow-report.md","content":{"$ref":"reliability#"}}}}]
    result = {"version":2,"name":name,"description":"Three focused reviews, one clear release report.","capabilities":["progress","agent","tool"],"max_parallel":3,"steps":steps}
    if not before: result.update(title=title,phases=phases)
    else:
        for step in steps: step.pop("phase_id")
    return result

def run(binary, out, before=False, tool_only=False):
    out.mkdir(parents=True, exist_ok=True)
    gates = {name:threading.Event() for name in ASSIGNMENTS}
    started = {name:threading.Event() for name in ASSIGNMENTS}
    requests = []
    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            data = json.dumps({"data":{"label":"local fixture"}} if self.path.endswith('/key') else {"data":MODEL} if '/model/' in self.path else {"data":[MODEL],"total_count":1,"links":{"next":None}}).encode()
            self.send_response(200); self.send_header("Content-Type","application/json"); self.send_header("Content-Length",str(len(data))); self.end_headers(); self.wfile.write(data)
        def do_POST(self):
            request=json.loads(self.rfile.read(int(self.headers['Content-Length']))); requests.append(request)
            (out/'requests.json').write_text(json.dumps(requests,indent=2))
            users=[str(m.get('content','')) for m in request['messages'] if m['role']=='user']
            joined='\n'.join(users); last=users[-1] if users else ''; tail=request['messages'][-1]['role']=='tool'
            self.send_response(200); self.send_header('Content-Type','text/event-stream'); self.end_headers()
            def chunk(delta, finish=None):
                if 'tool_calls' in delta: delta['reasoning']='Start the requested native workflow with three independent review agents.'
                payload={'id':'workflow-visual-fixture','object':'chat.completion.chunk','model':MODEL['id'],'choices':[{'index':0,'delta':delta,'finish_reason':finish}]}
                self.wfile.write(('data: '+json.dumps(payload)+'\n\n').encode()); self.wfile.flush()
            try:
                child=next((name for name,(_,prompt) in ASSIGNMENTS.items() if prompt in joined),None)
                if child:
                    chunk({'content':UPDATES[child], 'reasoning':f'Inspect the evidence for {child} before making a recommendation.'}); started[child].set(); gates[child].wait(120)
                    chunk({'content':f'\n\nReview complete. {child.title()} checks passed, with the evidence recorded for the final report.'}); chunk({},'stop')
                elif 'WORKFLOW_VISUAL_QUEUED' in last and not tail:
                    queued=definition(False, 'queued-review', 'Queued release review')
                    queued['capabilities'].append('delay')
                    queued['steps'][0]['action']={'kind':'delay','millis':30000,'value':'Queued scope'}
                    chunk({'tool_calls':[{'index':0,'id':'queued-workflow-start','type':'function','function':{'name':'workflow','arguments':json.dumps({'action':'start','definition':queued})}}]}); chunk({},'tool_calls')
                elif 'WORKFLOW_VISUAL_START' in last and not tail:
                    chunk({'tool_calls':[{'index':0,'id':'visual-workflow-start','type':'function','function':{'name':'workflow','arguments':json.dumps({'action':'start','definition':definition(before)})}}]}); chunk({},'tool_calls')
                else:
                    chunk({'content':'The release review is underway. Open the workflow below to follow each phase.'}); chunk({},'stop')
                self.wfile.write(b'data: [DONE]\n\n'); self.wfile.flush()
            except (BrokenPipeError,ConnectionResetError): pass
        def log_message(self,*_): pass
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler); threading.Thread(target=server.serve_forever,daemon=True).start()
    os.environ['HEYCODE_WORKFLOW_VISUAL_FIXTURE']='local-only-not-a-real-credential'
    captures=[]
    try:
        with tempfile.TemporaryDirectory(prefix='heycode-workflow-home-') as home, tempfile.TemporaryDirectory(prefix='heycode-workflow-work-') as cwd:
            base=f'http://127.0.0.1:{server.server_port}/api/v1'
            Path(home,'settings.toml').write_text('schema_version = 1\n')
            Path(home,'config.toml').write_text(f'schema_version = 30\n[llm]\nprovider = "openrouter"\nmodel = "{MODEL["id"]}"\napi_key_env = "HEYCODE_WORKFLOW_VISUAL_FIXTURE"\nbase_url = "{base}"\n')
            columns, rows = (110, 42) if tool_only else (120, 38)
            tui=FullScreenTui(home,cwd,str(binary),fake=False,color=True,rows=rows,columns=columns,extra=['--provider','openrouter','--model',MODEL['id'],'--approval','full_access','--set',f'llm.base_url={base}','--set','llm.api_key_env=HEYCODE_WORKFLOW_VISUAL_FIXTURE'])
            screen=Screen(columns,rows); stream=pyte.ByteStream(screen)
            def read(seconds=.1): stream.feed(tui.read(seconds)); return '\n'.join(screen.display)
            def send(data): os.write(tui.fd,data)
            def wait(text,timeout=25):
                until=time.monotonic()+timeout
                while time.monotonic()<until:
                    current=read()
                    if text in current: return current
                    if not tui.alive(): raise AssertionError(f'CLI exited: {current}')
                raise AssertionError(f'Missing {text}:\n{current}')
            def resize(columns,rows):
                screen.resize(rows,columns); fcntl.ioctl(tui.fd,termios.TIOCSWINSZ,struct.pack('HHHH',rows,columns,0,0)); read(.3)
            def capture(name):
                read(.15); (out/f'{name}.txt').write_text('\n'.join(screen.display)); (out/f'{name}.ansi').write_bytes(tui.transcript); render_screen(screen,out/f'{name}.png'); captures.append(name)
            def click_text(text, last=False):
                read(.1)
                for row,value in (reversed(list(enumerate(screen.display))) if last else enumerate(screen.display)):
                    if text in value:
                        col=value.index(text)+1; send(f'\x1b[<0;{col};{row+1}M'.encode()); read(.2); return
                raise AssertionError(f'No click target {text}:\n'+'\n'.join(screen.display))
            try:
                initial=read(2)
                if 'Welcome to heycode' in initial:
                    send(b'\x1b[B\x1b[B\r');wait('Select a provider');send(b'OpenRouter\r');wait('Paste your OpenRouter API key');send(b'local-only-not-a-real-credential\r');wait('Choose a model');send(b'\r')
                wait('full access on');send(b'WORKFLOW_VISUAL_START\r')
                until=time.monotonic()+20
                while not all(event.is_set() for event in started.values()) and time.monotonic()<until: read(.1)
                assert all(event.is_set() for event in started.values()), 'all three native review agents must stream together'
                read(.5)
                if not before:
                    current=wait('0/3 agents')
                    assert 'Workflow(Release readiness)' in current
                    assert 'Started in background' in current
                    assert 'Completed in' not in current
                    for forbidden in ['run_id','job_id','workflow node','workflow checkpoint','workflow progress','workflow start']:
                        assert forbidden not in current, f'Raw workflow chatter in main transcript: {forbidden}'
                capture('before-rail' if before else 'after-rail')
                if tool_only:
                    for gate in gates.values(): gate.set()
                    wait('3/3 agents');wait('Done');wait('3/3 phases')
                    assert Path(cwd,'workflow-report.md').exists()
                    capture('completed-tool-and-rail')
                    send(b'\x0f');wait('Showing detailed transcript');wait('"run_id"');wait('"job_id"')
                    capture('expanded-workflow-result')
                    send(b'\x0f');wait('full access on')
                    resize(60,24);capture('completed-tool-and-rail-narrow')
                    resize(110,42)
                    events = [json.loads(line) for journal in Path(home).rglob('session.jsonl') for line in journal.read_text().splitlines() if line.strip()]
                    (out/'events.json').write_text(json.dumps(events,indent=2))
                    result = {'passed':True,'runtime':'native','transport':'localhost HTTP fixture','parallel_agents':3,'tool_only':True,'captures':captures,'binary':str(binary),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'captured_at_utc':datetime.now(timezone.utc).isoformat(),'events':len(events),'report_written':True}
                    (out/'result.json').write_text(json.dumps(result,indent=2));print(json.dumps(result,indent=2))
                    return
                send(b'PARENT_REVIEW_DRAFT')
                if before: send(b'\x14');read(.2)
                else:
                    send(b'\x1b[B');read(.1);capture('rail-keyboard-focus');send(b'\r');wait('PHASES');wait('Research reliability')
                for columns,rows in [(120,38),(80,30),(40,24)]:
                    resize(columns,rows);capture(f'{"before" if before else "after"}-{columns}x{rows}')
                if not before:
                    resize(120,38);click_text('Research reliability');wait('ASSIGNMENT');wait('LATEST UPDATE');capture('agent-summary')
                    send(b'c');wait('Review reliability:');capture('agent-conversation')
                    send(b'\x1b');wait('ASSIGNMENT');send(b'\x1b');wait('PHASES')
                    gates['reliability'].set();wait('Done');capture('mixed-agent-states')
                    send(b'\x1b');wait('1/3 agents');capture('mixed-progress-rail')
                    send(b'\x1b[B\r');wait('PHASES');wait('Research reliability')
                    send(b'p');wait('Pause requested');capture('pausing')
                    for gate in gates.values():gate.set()
                    wait('Paused');capture('paused');send(b'r');wait('3 / 3 phases');capture('completed')
                    click_text('Deliver findings', last=True);wait('Write the review report');click_text('Review in parallel', last=True);wait('Research reliability');capture('completed-agents')
                    send(b'\x1b');wait('PARENT_REVIEW_DRAFT');capture('parent-draft-preserved')
                    assert Path(cwd,'workflow-report.md').exists()
                    send(b' continued');wait('PARENT_REVIEW_DRAFT continued')
                    send(b'\x03WORKFLOW_VISUAL_QUEUED\r');wait('Queued release review');current=wait('0/3 agents')
                    assert '[job job-' not in current, 'routine workflow inbox notice leaked into the main transcript'
                    assert not re.search(r'\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b',current), 'raw workflow run ID leaked into the main transcript'
                    capture('queued-agent-denominator')
                    send(b'\x1b[B\r');wait('PHASES');send(b'x');wait('Stopped');capture('stopped-workflow')
                    send(b'w');wait('Choose a workflow');capture('workflow-picker')
                    click_text('Release readiness');wait('PHASES');wait('3 / 3 phases');capture('restored-completed-workflow')
                    send(b'\x1b');wait('Full access');count_before_command=len(requests)
                    send(b'/workflows\r');wait('PHASES');wait('Release readiness');capture('slash-command-workspace')
                    assert len(requests)==count_before_command, '/workflows must open the native workspace without a model request'
                result={'passed':True,'before':before,'runtime':'native','transport':'localhost HTTP fixture','parallel_agents':3,'captures':captures,'binary':str(binary)}
                (out/'result.json').write_text(json.dumps(result,indent=2));print(json.dumps(result,indent=2))
            except Exception:
                capture('failure');raise
            finally:
                for gate in gates.values():gate.set()
                tui.close()
    finally: server.shutdown();server.server_close()
if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--binary',type=Path,required=True);parser.add_argument('--out',type=Path,required=True);parser.add_argument('--before',action='store_true');parser.add_argument('--tool-only',action='store_true');args=parser.parse_args();run(args.binary.resolve(),args.out.resolve(),args.before,args.tool_only)
