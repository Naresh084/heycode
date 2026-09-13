#!/usr/bin/env python3
"""Capture native question selection, custom answers and cancellation on an isolated PTY.

Only an authenticated-looking loopback fixture is used; the child environment
contains no inherited credentials and macOS sandboxing denies external network.
"""
from __future__ import annotations
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import shlex
import sys
import tempfile
import threading
import time
from command_panel_terminal_check import Screen
from plan_review_pty import MODEL
from terminal_screenshot import TerminalByteStream, render_screen
from tui_blackbox import FullScreenTui


def run(binary: Path, output: Path, columns: int, rows: int) -> dict:
    binary = binary.resolve(strict=True)
    output.mkdir(parents=True, exist_ok=False)
    requests, captures, tool_results = [], [], {}
    result = {'status': 'blocked', 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'viewport': [columns, rows], 'synthetic_fixture': True, 'external_inference': False, 'credential_inheritance': False}
    with tempfile.TemporaryDirectory(prefix='heycode-question-fixture-') as temporary:
        root = Path(temporary)
        home, work = root/'home', root/'work'
        home.mkdir(); work.mkdir()
        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_args): pass
            def do_GET(self):
                payload = {'data': {'label': 'fixture'}} if self.path.endswith('/key') else ({'data': MODEL} if '/model/' in self.path else {'data': [MODEL], 'total_count': 1, 'links': {'next': None}})
                data=json.dumps(payload).encode(); self.send_response(200); self.send_header('Content-Type','application/json'); self.send_header('Content-Length',str(len(data))); self.end_headers(); self.wfile.write(data)
            def do_POST(self):
                body=json.loads(self.rfile.read(int(self.headers['Content-Length']))); requests.append(body)
                for message in body.get('messages', []):
                    if message.get('role') == 'tool': tool_results[message['tool_call_id']] = message.get('content')
                step=len(requests)
                if step <= 3:
                    args={'header':'Fixture','question':f'Question {step}: Which synthetic fixture should be selected?','options':[{'label':'Alpha','description':'First local fixture'},{'label':'Beta','description':'Second local fixture'}]}
                    delta={'tool_calls':[{'index':0,'id':f'question-{step}','type':'function','function':{'name':'ask_user_question','arguments':json.dumps(args)}}]}
                else: delta={'content':'SYNTHETIC_QUESTION_DONE'}
                if step <= 3: delta['reasoning_details']=[{'type':'reasoning.text','text':'Exercise the deterministic local required question fixture.'}]
                self.send_response(200); self.send_header('Content-Type','text/event-stream'); self.end_headers()
                for content, finish in [(delta,None),({},'tool_calls' if step <= 3 else 'stop')]:
                    chunk={'id':'question-fixture','object':'chat.completion.chunk','model':MODEL['id'],'choices':[{'index':0,'delta':content,'finish_reason':finish}]}
                    self.wfile.write(('data: '+json.dumps(chunk)+'\n\n').encode()); self.wfile.flush()
                self.wfile.write(b'data: [DONE]\n\n'); self.wfile.flush()
        server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
        threading.Thread(target=server.serve_forever,daemon=True).start()
        base=f'http://127.0.0.1:{server.server_port}/api/v1'
        profile=root/'network.sb'; profile.write_text(f'(version 1)\n(allow default)\n(deny network*)\n(allow network-outbound (remote ip "localhost:{server.server_port}"))\n')
        (output/'network-sandbox.sb').write_text(profile.read_text())
        environment={'PATH':'/opt/homebrew/bin:/usr/bin:/bin','HOME':str(home),'HEYCODE_HOME':str(home),'TERM':'xterm-256color','COLORTERM':'truecolor','HEYCODE_QUESTION_FIXTURE':'synthetic-local-only','HTTP_PROXY':'http://127.0.0.1:9','HTTPS_PROXY':'http://127.0.0.1:9','NO_PROXY':'127.0.0.1,localhost'}
        (home/'config.toml').write_text(f'schema_version = 31\n[llm]\nprovider="openrouter"\nmodel="{MODEL["id"]}"\napi_key_env="HEYCODE_QUESTION_FIXTURE"\nbase_url="{base}"\n')
        wrapper=root/'launch'; wrapper.write_text('#!/bin/sh\nexec '+shlex.join(['/usr/bin/env','-i',*[f'{k}={v}' for k,v in environment.items()],'/usr/bin/sandbox-exec','-f',str(profile),str(binary)])+' "$@"\n'); wrapper.chmod(0o755)
        tui=FullScreenTui(str(home),str(work),str(wrapper),fake=False,color=True,rows=rows,columns=columns)
        screen=Screen(columns,rows); stream=TerminalByteStream(screen)
        def read(seconds=.2): stream.feed(tui.read(seconds)); return '\n'.join(screen.display)
        def send(data): os.write(tui.fd,data); read(.3)
        def wait(needle):
            deadline=time.monotonic()+35
            while time.monotonic()<deadline:
                value=read()
                if needle in value: return value
                if not tui.alive(): break
            raise AssertionError(f'missing {needle}: {value}')
        def capture(label):
            value=read(.5); (output/f'{label}.txt').write_text(value); render_screen(screen,output/f'{label}.png'); captures.append(label); return value
        try:
            wait('for shortcuts'); capture('00-ready')
            send(b'Exercise the three synthetic required question fixtures\r')
            wait('Question 1:'); value=capture('01-question-open')
            assert '❯ 1. Alpha' in value and 'Type something.' in value, value
            assert 'Enter to select' in value and 'Esc to cancel' in value, value
            send(b'\x1b[B'); value=capture('02-question-beta'); assert '❯ 2. Beta' in value,value
            send(b'\r'); wait('Question 2:')
            send(b'\x1b[B\x1b[B'); value=capture('03-question-custom'); assert '❯ 3. Type something.' in value, value
            send(b'Custom fixture answer'); value=capture('04-custom-typed'); assert 'Answer  Custom fixture answer' in value, value
            send(b'\r'); wait('Question 3:'); capture('05-question-cancel')
            send(b'\x1b'); wait('SYNTHETIC_QUESTION_DONE'); capture('06-settled')
            assert 'Beta' in str(tool_results.get('question-1')),tool_results
            assert 'Custom fixture answer' in str(tool_results.get('question-2')),tool_results
            assert 'cancel' in str(tool_results.get('question-3')).lower(),tool_results
            result.update(status='passed',selected_answer_verified=True,custom_answer_verified=True,cancellation_verified=True)
        except Exception as error:
            result['error']=repr(error); capture('99-blocked')
        finally:
            result.update(captures=captures,requests=len(requests))
            (output/'terminal.ansi').write_bytes(tui.transcript); tui.close()
            (output/'requests.json').write_text(json.dumps(requests,indent=2)); (output/'tool-results.json').write_text(json.dumps(tool_results,indent=2)); (output/'result.json').write_text(json.dumps(result,indent=2)+'\n')
            server.shutdown(); server.server_close()
    return result

if __name__ == '__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,required=True); parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--columns',type=int,default=110); parser.add_argument('--rows',type=int,default=42)
    args=parser.parse_args(); result=run(args.binary,args.output,args.columns,args.rows)
    print(json.dumps(result)); sys.exit(0 if result['status']=='passed' else 1)
