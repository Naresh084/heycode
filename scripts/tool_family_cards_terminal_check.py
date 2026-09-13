#!/usr/bin/env python3
"""Exercise real notebook/MCP tools and card expansion over a kernel-isolated PTY.

The provider is synthetic loopback HTTP; the MCP server is a disposable installed
declarative package with explicit resource exposure. No provider credentials,
real account state, outside writes, or external inference are used.
"""
from __future__ import annotations
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import queue
import shlex
import subprocess
import sys
import tempfile
import threading
import time

from core_tool_cards_claude_reference_pty import Screen
from plan_review_pty import MODEL
from terminal_screenshot import TerminalByteStream, render_screen
from tui_blackbox import FullScreenTui

MARKER="SYNTHETIC_TOOL_FAMILIES_DONE"


def run(binary: Path, output: Path, capture_baseline: bool = False, notebook_only: bool = False) -> dict:
    binary=binary.resolve(); output.mkdir(parents=True,exist_ok=False)
    requests=[]; results={}; gates=queue.Queue(); captures=[]; errors=[]
    result={"status":"blocked","binary":str(binary),"binary_sha256":hashlib.sha256(binary.read_bytes()).hexdigest(),
            "external_inference":False,"synthetic_fixture":True,"credential_inheritance":False,"viewport":[110,42],"capture_baseline":capture_baseline,"notebook_only":notebook_only}
    with tempfile.TemporaryDirectory(prefix="heycode-tool-family-reference-") as temporary:
        root=Path(temporary); home=root/"home"; work=root/"work"; home.mkdir(); work.mkdir()
        notebook={"nbformat":4,"nbformat_minor":5,"metadata":{"language_info":{"name":"python"}},"cells":[{"cell_type":"code","id":"fixture-cell","metadata":{},"source":["print('before')\n"],"outputs":[],"execution_count":None}]}
        (work/"sample.ipynb").write_text(json.dumps(notebook))
        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self,*args): pass
            def do_GET(self):
                payload={"data":{"label":"fixture"}} if self.path.endswith('/key') else ({"data":MODEL} if '/model/' in self.path else {"data":[MODEL],"total_count":1,"links":{"next":None}})
                data=json.dumps(payload).encode(); self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
            def do_POST(self):
                body=json.loads(self.rfile.read(int(self.headers['Content-Length']))); requests.append(body)
                for message in body.get('messages',[]):
                    if message.get('role')=='tool':
                        content=message.get('content','')
                        if isinstance(content,str) and content.startswith('[BEGIN UNTRUSTED MCP SERVER CONTENT '):
                            content=content.split('\n',1)[1].rsplit('\n[END UNTRUSTED MCP SERVER CONTENT]',1)[0]
                        try: results[message['tool_call_id']]=json.loads(content)
                        except (ValueError,TypeError): results[message['tool_call_id']]=content
                step=len(requests)-1; name=None; args=None
                try:
                    if step in ((2,3) if notebook_only else (2,4,5,6)):
                        event=threading.Event(); gates.put((step,event)); assert event.wait(20),f'capture gate {step} timed out'
                    if step==0: name,args='notebook_read',{'path':'sample.ipynb'}
                    elif step==1:
                        name,args='notebook_edit',{'path':'sample.ipynb','expected_revision':results['family-0']['revision'],'action':'replace','cell_index':0,'cell_id':'fixture-cell','source':"print('synthetic after')\nsecond_line = 2\nthird_line = 3\nfourth_line = 4\n"}
                    elif step==2:
                        assert results['family-1']['executed'] is False
                        if not capture_baseline:
                            assert results['family-1']['cell_type']=='code'
                            assert results['family-1']['language']=='python'
                        saved_source=json.loads((work/'sample.ipynb').read_text())['cells'][0]['source']
                        if isinstance(saved_source,list): saved_source=''.join(saved_source)
                        assert saved_source=="print('synthetic after')\nsecond_line = 2\nthird_line = 3\nfourth_line = 4\n",repr(saved_source)
                        if notebook_only:
                            name,args='notebook_edit',{'path':'sample.ipynb','expected_revision':results['family-0']['revision'],'action':'replace','cell_index':0,'source':'must not be written'}
                        else:
                            name,args='list_mcp_resources',{}
                    elif step==3 and notebook_only:
                        assert 'changed' in str(results['family-2']).lower()
                        assert 'must not be written' not in (work/'sample.ipynb').read_text()
                    elif step==3:
                        servers=results['family-2']['servers']; server=next(row['server'] for row in servers if row.get('resources_exposed'))
                        name,args='list_mcp_resources',{'server':server,'limit':1}
                    elif step==4:
                        listing=results['family-3']; assert listing['returned']==1 and listing['total']==2 and listing['continuation']
                        name,args='read_mcp_resource',{'server':listing['server'],'uri':listing['resources'][0]['uri']}
                    elif step==5:
                        assert 'SYNTHETIC_MCP_RESOURCE_CONTENT' in str(results['family-4'])
                        name,args='notebook_edit',{'path':'sample.ipynb','expected_revision':results['family-0']['revision'],'action':'replace','cell_index':0,'source':'must not be written'}
                    elif step==6:
                        assert 'changed' in str(results['family-5']).lower()
                        assert 'must not be written' not in (work/'sample.ipynb').read_text()
                except Exception as error:
                    errors.append(repr(error)); name=None
                delta={'tool_calls':[{'index':0,'id':f'family-{step}','type':'function','function':{'name':name,'arguments':json.dumps(args)}}]} if name else {'content':MARKER if not errors else 'FIXTURE_FAILED '+str(errors)}
                if name:
                    delta['reasoning_details']=[{'type':'reasoning.text','text':'Exercise deterministic synthetic tool card fixtures.'}]
                self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers()
                for content,finish in [(delta,None),({},'tool_calls' if name else 'stop')]:
                    chunk={'id':'families','object':'chat.completion.chunk','model':MODEL['id'],'choices':[{'index':0,'delta':content,'finish_reason':finish}]}
                    self.wfile.write(('data: '+json.dumps(chunk)+'\n\n').encode());self.wfile.flush()
                self.wfile.write(b'data: [DONE]\n\n');self.wfile.flush()
        server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
        threading.Thread(target=server.serve_forever,daemon=True).start()
        base=f'http://127.0.0.1:{server.server_port}/api/v1'
        profile=root/'network.sb';profile.write_text(f'(version 1)\n(allow default)\n(deny network*)\n(allow network-outbound (remote ip "localhost:{server.server_port}"))\n')
        environment={'PATH':'/opt/homebrew/bin:/usr/bin:/bin','HOME':str(home),'HEYCODE_HOME':str(home),'TERM':'xterm-256color','COLORTERM':'truecolor','HEYCODE_FAMILY_FIXTURE':'synthetic-local-only','HTTP_PROXY':'http://127.0.0.1:9','HTTPS_PROXY':'http://127.0.0.1:9','NO_PROXY':'127.0.0.1,localhost'}
        sandbox=['/usr/bin/sandbox-exec','-f',str(profile)]
        (output/'network-sandbox.sb').write_text(profile.read_text())
        package=root/'package';(package/'.heycode-plugin').mkdir(parents=True);(package/'mcp').mkdir();(package/'bin').mkdir()
        (package/'.heycode-plugin/plugin.toml').write_text('''schema_version = 1
id = "fixture/resource-cards"
name = "Synthetic resource cards"
version = "1.0.0"
description = "Local synthetic terminal resource fixture"
license = "MIT"
default_enabled = true
requested_permissions = ["mcp_connect", "process_spawn"]
platforms = [{ os = "macos", architecture = "aarch64" }]
dependencies = []
conflicts = []
[[contributions]]
kind = "mcp"
id = "fixture"
path = "mcp/fixture.json"
exposure = { mode = "namespaced" }
[api]
minimum = 1
maximum = 1
[source]
kind = "local"
locator = "fixture/resource-cards"
revision = "1.0.0"
update_channel = "pinned"
[authentication]
policy = "none"
credentials = []
''')
        fixture_source=(Path(__file__).parent/'terminal_reference_mcp_fixture.py').read_text()
        fixture_source=fixture_source.replace('"resources/list": {"resources":[{"uri":"fixture://local/sample","name":"Synthetic fixture resource","mimeType":"text/plain"}]}', '"resources/list": {"resources":[{"uri":"fixture://local/sample","name":"Synthetic fixture resource","mimeType":"text/plain"},{"uri":"fixture://local/other","name":"Other synthetic fixture","mimeType":"text/plain"}]}')
        fixture_source=fixture_source.replace('#!/usr/bin/env python3',f'#!{sys.executable}')
        fixture_source=fixture_source.replace('"protocolVersion":"2024-11-05"','"protocolVersion":request.get("params",{}).get("protocolVersion","2024-11-05")')
        fixture_source=fixture_source.replace('    request = json.loads(line)','    request = json.loads(line)\n    with open(sys.argv[1], "a") as log: log.write(json.dumps(request)+"\\n")')
        fixture=package/'bin/mcp-server';fixture.write_text(fixture_source);fixture.chmod(0o755)
        fixture_log=root/'mcp-requests.jsonl'
        (package/'mcp/fixture.json').write_text(json.dumps({'transport':'stdio','command':'bin/mcp-server','args':[str(fixture_log)],'required':False,'display_name':'Synthetic resource fixture','exposure':{'resources':True,'prompts':False,'instructions':False}}))
        tui=None
        try:
            if not notebook_only:
                seed=subprocess.run([*sandbox,str(binary),'plugin','install',str(package)],cwd=work,env=environment,capture_output=True,text=True,timeout=25)
                (output/'plugin-seed.json').write_text(json.dumps({'returncode':seed.returncode,'stdout':seed.stdout,'stderr':seed.stderr},indent=2));assert seed.returncode==0,seed.stderr
            (home/'config.toml').write_text(f'schema_version = 31\n[llm]\nprovider="openrouter"\nmodel="{MODEL["id"]}"\napi_key_env="HEYCODE_FAMILY_FIXTURE"\nbase_url="{base}"\n')
            wrapper=root/'launch';wrapper.write_text('#!/bin/sh\nexec '+shlex.join(['/usr/bin/env','-i',*[f'{k}={v}' for k,v in environment.items()],*sandbox,str(binary)])+' "$@"\n');wrapper.chmod(0o755)
            tui=FullScreenTui(str(home),str(work),str(wrapper),fake=False,color=True,rows=42,columns=110)
            screen=Screen(110,42);stream=TerminalByteStream(screen)
            def read(seconds=.2): stream.feed(tui.read(seconds)); return '\n'.join(screen.display)
            def send(data): os.write(tui.fd,data);read(.3)
            def capture(label):
                value=read(.4);(output/f'{label}.txt').write_text(value);render_screen(screen,output/f'{label}.png');captures.append(label);return value
            def wait(needle,timeout=30):
                deadline=time.monotonic()+timeout
                while time.monotonic()<deadline:
                    value=read()
                    if needle.casefold() in value.casefold(): return value
                    if errors: raise AssertionError(errors)
                    if not tui.alive(): break
                raise AssertionError(f'missing {needle}: {value}')
            wait('for shortcuts');send(b'/permissions full_access\r');wait('Full access');read(2)
            capture('00-ready');send(b'Exercise the synthetic notebook and resource card fixtures\r')
            labels={2:'01-notebook-completed',3:'02-notebook-rejected'} if notebook_only else {2:'01-notebook-completed',4:'02-mcp-list-completed',5:'03-mcp-read-completed',6:'04-notebook-rejected'}
            for expected in ([2,3] if notebook_only else [2,4,5,6]):
                deadline=time.monotonic()+35; pending=None
                while time.monotonic()<deadline:
                    read()
                    try: pending=gates.get_nowait();break
                    except queue.Empty: pass
                    if errors: raise AssertionError(errors)
                assert pending is not None,f'gate {expected} missing'
                step,event=pending;assert step==expected
                text=capture(labels[step])
                if step==2 and not capture_baseline:
                    assert 'Edit Notebook(sample.ipynb@fixture-cell)' in text and 'Updated cell fixture-cell' in text,text
                    assert 'source lines' in text,text
                if step==4 and not capture_baseline: assert 'listMcpResources' in text and '1 of 2 returned' in text and 'more available' in text,text
                if step==5:
                    if not capture_baseline: assert 'readMcpResource' in text and 'UNTRUSTED' in text,text
                    send(b'\x0f');expanded=capture('03-mcp-read-expanded');assert 'SYNTHETIC_MCP_RESOURCE_CONTENT' in expanded,expanded
                    send(b'\x0f')
                event.set()
            wait(MARKER);capture('05-settled')
            result.update(status='passed',requests=len(requests),captures=captures,notebook_atomic_change_verified=True,stale_revision_rejected=True,mcp_pagination_verified=not notebook_only,mcp_full_output_expanded=not notebook_only)
        except Exception as error:
            if tui is not None:
                stream.feed(tui.read(.2))
                (output/'99-blocked.txt').write_text('\n'.join(screen.display))
                render_screen(screen,output/'99-blocked.png')
                captures.append('99-blocked')
            result.update(error=repr(error),captures=captures)
        finally:
            while not gates.empty(): gates.get()[1].set()
            if tui is not None:
                (output/'terminal.ansi').write_bytes(tui.transcript);tui.close()
            (output/'requests.json').write_text(json.dumps(requests,indent=2));(output/'tool-results.json').write_text(json.dumps(results,indent=2))
            if fixture_log.exists(): (output/'mcp-requests.jsonl').write_bytes(fixture_log.read_bytes())
            (output/'result.json').write_text(json.dumps(result,indent=2)+'\n');server.shutdown();server.server_close()
    return result


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--binary',type=Path,required=True);parser.add_argument('--output',type=Path,required=True);parser.add_argument('--capture-baseline',action='store_true');parser.add_argument('--notebook-only',action='store_true')
    args=parser.parse_args(); value=run(args.binary,args.output,args.capture_baseline,args.notebook_only);print(json.dumps(value));raise SystemExit(0 if value['status']=='passed' else 1)
