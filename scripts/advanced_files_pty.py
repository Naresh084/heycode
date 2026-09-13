#!/usr/bin/env python3
"""Exercise bounded file tools through the production binary, local HTTP and a PTY."""
from __future__ import annotations
import argparse, http.server, json, os, threading, time
from pathlib import Path
from tempfile import TemporaryDirectory
import pyte
from plan_review_pty import MODEL
from tui_blackbox import FullScreenTui, plain
from terminal_screenshot import render_screen


def journey(binary: Path, output: Path, search_bounds: bool = False, inspect_tools: bool = False):
    output.mkdir(parents=True, exist_ok=True)
    requests, results, errors = [], {}, []
    update_ready, update_continue = threading.Event(), threading.Event()
    update_captured = False
    with TemporaryDirectory(prefix='heycode-advanced-files-') as temporary:
        root = Path(temporary); home = root/'home'; work = root/'work'
        home.mkdir(); work.mkdir()
        original = ''.join(f'line {i:03d}\n' for i in range(1,438))
        (work/'large.txt').write_text(original)
        (work/'edit.txt').write_text('alpha\nbeta\ngamma\n')
        for name in ['wide-a.txt','wide-b.txt']: (work/name).write_text(('abcdefghij'*9+'\n')*437)
        if search_bounds: (work/'oversized.txt').write_text('x'*(1024*1024+1)+'\nneedle after oversized line\n')
        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                payload = {'data': {'label':'fixture'}} if self.path.endswith('/key') else ({'data': MODEL} if '/model/' in self.path else {'data':[MODEL],'total_count':1,'links':{'next':None}})
                body=json.dumps(payload).encode(); self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
            def do_POST(self):
                request=json.loads(self.rfile.read(int(self.headers['Content-Length'])));requests.append(request)
                for message in request['messages']:
                    if message.get('role') == 'tool':
                        content=message.get('content','')
                        try: results[message['tool_call_id']]=json.loads(content)
                        except (TypeError,ValueError): results[message['tool_call_id']]=content
                step=len(requests)-1
                try:
                    if step==0: name,args='read',{'path':'large.txt'}
                    elif step==1:
                        page=results['files-0']; assert page['lines_returned']==200 and page['lines_remaining']==237
                        name,args='read',page['continuation']
                    elif step==2:
                        page=results['files-1']; assert page['offset']==201 and page['lines_remaining']==37
                        name,args='read_many',{'files':[{'path':'edit.txt'},{'path':'missing.txt'},{'path':'large.txt','offset':401}]}
                    elif step==3:
                        batch=results['files-2']; assert [f['status'] for f in batch['files']]==['read','error','read']
                        name,args='multi_edit',{'path':'edit.txt','expected_revision':batch['files'][0]['revision'],'edits':[{'old_string':'beta','new_string':'delta'},{'old_string':'absent','new_string':'oops'}]}
                    elif step==4:
                        assert (work/'edit.txt').read_text()=='alpha\nbeta\ngamma\n'
                        assert 'edit 2' in str(results['files-3']).lower()
                        name,args='multi_edit',{'path':'edit.txt','expected_revision':results['files-2']['files'][0]['revision'],'dry_run':True,'edits':[{'old_string':'beta','new_string':'delta'}]}
                    elif step==5:
                        assert results['files-4']['dry_run'] and (work/'edit.txt').read_text()=='alpha\nbeta\ngamma\n'
                        name,args='multi_edit',{'path':'edit.txt','expected_revision':results['files-4']['revision'],'edits':[{'old_string':'beta','new_string':'delta'}]}
                    elif step==6:
                        assert results['files-5']['changed'] and (work/'edit.txt').read_text()=='alpha\ndelta\ngamma\n'
                        preview=results['files-5']['diff_previews'][0]
                        assert [(row['line'],row['kind'],row['text']) for row in preview['rows']]==[(1,'context','alpha'),(2,'removed','beta'),(2,'added','delta'),(3,'context','gamma')]
                        update_ready.set(); assert update_continue.wait(10), 'Update capture did not finish'
                        name,args='write',{'path':'edit.txt','content':'must not overwrite'}
                    elif step==7:
                        assert (work/'edit.txt').read_text()=='alpha\ndelta\ngamma\n'
                        name,args='write',{'path':'new.txt','content':'created\n'}
                    elif step==8:
                        name,args='read',{'path':'new.txt'}
                    elif step==9:
                        name,args='write',{'path':'new.txt','mode':'replace','expected_revision':results['files-8']['revision'],'content':'replaced\n'}
                    elif step==10:
                        name,args='read_many',{'files':[{'path':'wide-a.txt'},{'path':'wide-b.txt'}]}
                    elif step==11 and search_bounds:
                        name,args='grep',{'path':'oversized.txt','pattern':'needle'}
                    elif step==12 and search_bounds:
                        text=results['files-11']; assert 'oversized.txt:2: needle after oversized line' in text
                        assert 'Partial search; match count is a lower bound' in text and '1 oversized lines skipped' in text
                        name,args='glob',{'pattern':'**/*.txt'}
                    else:
                        if search_bounds: assert 'oversized.txt' in results['files-12']
                        batch=results['files-10']; assert all(f['total_lines']==437 and f['continuation'] for f in batch['files'])
                        assert sum(f['bytes_returned'] for f in batch['files']) <= 32768
                        assert len(json.dumps(batch)) > 32768
                        assert (work/'large.txt').read_text()==original
                        assert (work/'new.txt').read_text()=='replaced\n'
                        name,args=None,None
                    delta={'tool_calls':[{'index':0,'id':f'files-{step}','type':'function','function':{'name':name,'arguments':json.dumps(args)}}]} if name else {'content':'ADVANCED-FILES-VERIFIED'}
                except Exception as error:
                    errors.append(repr(error));delta={'content':'FIXTURE-FAILED '+repr(error)};name=None
                if name: delta['reasoning_details']=[{'type':'reasoning.text','text':'Verify bounded file tool contracts.'}]
                self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers()
                for content,finish in [(delta,None),({},'tool_calls' if name else 'stop')]:
                    chunk={'id':'files','object':'chat.completion.chunk','model':MODEL['id'],'choices':[{'index':0,'delta':content,'finish_reason':finish}]}
                    self.wfile.write(('data: '+json.dumps(chunk)+'\n\n').encode());self.wfile.flush()
                self.wfile.write(b'data: [DONE]\n\n');self.wfile.flush()
            def log_message(self,*args): pass
        server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
        threading.Thread(target=server.serve_forever,daemon=True).start()
        base=f'http://127.0.0.1:{server.server_port}/api/v1'
        (home/'settings.toml').write_text('schema_version = 1\n')
        (home/'config.toml').write_text(f'schema_version = 31\n[llm]\nprovider="openrouter"\nmodel="{MODEL["id"]}"\napi_key_env="HEYCODE_ADVANCED_FILES_FIXTURE"\nbase_url="{base}"\n')
        os.environ['HEYCODE_ADVANCED_FILES_FIXTURE']='fixture-key'
        tui=FullScreenTui(str(home),str(work),str(binary),fake=False,color=True,rows=45,columns=100,extra=['--provider','openrouter','--model',MODEL['id'],'--set',f'llm.base_url={base}','--set','llm.api_key_env=HEYCODE_ADVANCED_FILES_FIXTURE'])
        class Screen(pyte.Screen):
            def set_mode(self, *modes, **kwargs):
                if kwargs.get('private') and 1049 in modes: self.reset()
                return super().set_mode(*modes, **kwargs)
        screen=Screen(100,45);stream=pyte.ByteStream(screen);seen='' 
        def read():
            nonlocal seen
            data=tui.read(.15);stream.feed(data);seen+=plain(data)
        def send(data): os.write(tui.fd,data);read()
        def wait(text):
            nonlocal update_captured
            deadline=time.monotonic()+40
            while time.monotonic()<deadline:
                read()
                if update_ready.is_set() and not update_captured:
                    capture('update-completed');update_captured=True;update_continue.set()
                if text in seen:return
                if errors:raise AssertionError(errors)
            raise AssertionError(f'Missing {text}: {seen[-2500:]}')
        def capture(name):
            read();(output/f'{name}.txt').write_text('\n'.join(screen.display));render_screen(screen,output/f'{name}.png')
        try:
            wait('shift+tab to cycle')
            send(b'/permissions full_access\r');wait('Full access')
            send(b'Verify the bounded file operations in this fixture\r');wait('ADVANCED-FILES-VERIFIED');capture('completed')
            # Expand the last retained read using the real mouse hit target.
            if not search_bounds:
                row = max(i for i,line in enumerate(screen.display) if '▸ Read 1 file' in line)
                col = screen.display[row].index('▸')
                send(f'\x1b[<0;{col+1};{row+1}M\x1b[<0;{col+1};{row+1}m'.encode());capture('read-expanded')
                assert '1 total · 0 remaining' in '\n'.join(screen.display), '\n'.join(screen.display)
            assert len(requests)==(14 if search_bounds else 12), len(requests)
            assert not errors, errors
            if inspect_tools:
                for tool_name in ['read','read_many','multi_edit','write','grep','glob']:
                    start=len(seen);send(f'/tools {tool_name}\r'.encode())
                    deadline=time.monotonic()+10
                    while ('Parameters:' not in '\n'.join(screen.display) or f'{tool_name} · client' not in '\n'.join(screen.display)) and time.monotonic()<deadline: read()
                    read();text='\n'.join(screen.display)
                    capture(f'inspector-{tool_name}')
                    if 'Owner:' not in text:
                        send(b'\x1b[5~');capture(f'inspector-{tool_name}-top');text+='\n'+'\n'.join(screen.display)
                        send(b'\x1b[6~')
                    for required in ['Owner:', 'Registered:', 'Selected on current route:', 'Successful session calls:', 'Parameters:']:
                        assert required in text, (tool_name,required,text)
                    (output/f'inspector-{tool_name}-output.txt').write_text(text)
                assert len(requests)==(14 if search_bounds else 12), 'Inspector made a model request'
            events=[json.loads(line) for log in home.rglob('session.jsonl') for line in log.read_text().splitlines()]
            (output/'events.json').write_text(json.dumps(events,indent=2))
            (output/'result.json').write_text(json.dumps({'status':'passed','requests':len(requests),'expected_rejections':2,'search_bounds':search_bounds,'default_lines':200,'total_lines':437,'remaining_after_first_page':237},indent=2))
        finally:
            (output/'terminal.ansi').write_bytes(tui.transcript)
            (output/'requests.json').write_text(json.dumps(requests,indent=2))
            (output/'results.json').write_text(json.dumps(results,indent=2))
            tui.close();server.shutdown()
    print(f'PASS: {output}')

if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--binary',default='target/debug/heycode');parser.add_argument('--output',default='tmp/terminal-evidence/advanced-files-pty');parser.add_argument('--search-bounds',action='store_true');parser.add_argument('--inspect-tools',action='store_true');args=parser.parse_args()
    journey(Path(args.binary).resolve(),Path(args.output),args.search_bounds,args.inspect_tools)
