#!/usr/bin/env python3
"""Exercise compact tool cards and committed edit permission in the production CLI."""
from __future__ import annotations
import argparse, fcntl, http.server, json, os, struct, tempfile, termios, threading, time
from pathlib import Path
from plan_review_pty import MODEL
from tui_blackbox import FullScreenTui, plain, shows
import pyte
from terminal_screenshot import render_screen


def journey(binary: str, output: Path, transcript=False):
    requests = []
    thinking_seen = threading.Event()
    with tempfile.TemporaryDirectory(prefix='heycode-tool-cards-') as folder:
        root = Path(folder); home = root / 'home'; work = root / 'work'
        home.mkdir(); work.mkdir()
        (work / 'input.txt').write_text('\n'.join(f'input detail {i}' for i in range(25)))
        calls = [('read', {'path': str(work / 'input.txt')}),
                 ('write', {'path': str(work / 'first.txt'), 'content': 'FIRST'}),
                 ('write', {'path': str(work / 'second.txt'), 'content': 'SECOND'}),
                 ('bash', {'command': 'printf forbidden > denied.txt'}),
                 ('bash', {'command': "for i in $(seq 1 400); do printf 'OUTPUT_LINE_%s_abcdefghijklmnopqrstuvwxyz\\n' \"$i\"; done; printf 'EXPANDED_OUTPUT_TAIL\\n'"}),
                 ('job_output', {'job_id':'job-0'})]
        if transcript:
            (work/'crates').mkdir()
            for i in range(8):
                folder=work/'crates'/f'crate-{i}'
                folder.mkdir(); (folder/'Cargo.toml').write_text('[package]\n')
            todos=[{'content':'Inspect project files','status':'completed'}, {'content':'Verify the failing search','status':'in_progress'}, {'content':'Report findings','status':'pending'}]
            calls=[('todo_write', {'todos':todos}), ('read', {'path':str(work/'input.txt')}),
                   ('edit', {'path':str(work/'input.txt'), 'old_string':'input detail 0', 'new_string':'input detail zero'}),
                   ('glob', {'pattern':'crates/*/Cargo.toml'}),
                   ('glob', {'path':'missing-folder','pattern':'*.rs'}),
                   ('bash', {'command':"printf 'Scan complete\\n8 packages found\\n'"})]
        final_text = 'Finished checking the project files and search results.' if transcript else 'TOOL-CARDS-DONE'
        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                payload = {'data': {'label': 'fixture'}} if self.path.endswith('/key') else ({'data': MODEL} if '/model/' in self.path else {'data': [MODEL], 'total_count': 1, 'links': {'next': None}})
                body = json.dumps(payload).encode(); self.send_response(200); self.send_header('Content-Type','application/json'); self.send_header('Content-Length',str(len(body))); self.end_headers(); self.wfile.write(body)
            def do_POST(self):
                requests.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
                step = len(requests)-1
                if step < len(calls):
                    name,args = calls[step]; delta = {'tool_calls':[{'index':0,'id':f'card-{step}','type':'function','function':{'name':name,'arguments':json.dumps(args)}}]}; finish='tool_calls'
                else: delta={'content':final_text}; finish='stop'
                if step < len(calls):
                    delta['reasoning_details']=[{'type':'reasoning.text','text':'Exercise tool permission and rendering.'}]
                if transcript and step in (1,3,4):
                    delta['content']={1:'I’ll inspect the project files and package manifests.',3:'The package layout is clear. I’ll check the missing search root.',4:'The missing directory explains that failure. I’ll verify the command output.'}[step]
                self.send_response(200); self.send_header('Content-Type','text/event-stream'); self.end_headers()
                if step == 0:
                    for detail in [{'type':'reasoning.encrypted','data':'opaque-fixture-state'}, {'type':'reasoning.summary','summary':'Checking tool permissions before reading files.'}]:
                        chunk={'id':'cards','object':'chat.completion.chunk','model':MODEL['id'],'choices':[{'index':0,'delta':{'reasoning_details':[detail]},'finish_reason':None}]}
                        self.wfile.write(('data: '+json.dumps(chunk)+'\n\n').encode()); self.wfile.flush()
                    thinking_seen.wait(12)
                for content,reason in [(delta,None),({},finish)]:
                    chunk={'id':'cards','object':'chat.completion.chunk','model':MODEL['id'],'choices':[{'index':0,'delta':content,'finish_reason':reason}]}
                    if reason: chunk['usage']={'prompt_tokens':300000,'completion_tokens':100,'total_tokens':300100}
                    self.wfile.write(('data: '+json.dumps(chunk)+'\n\n').encode()); self.wfile.flush()
                self.wfile.write(b'data: [DONE]\n\n'); self.wfile.flush()
            def log_message(self,*args): pass
        server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
        thread=threading.Thread(target=server.serve_forever,daemon=True); thread.start()
        base=f'http://127.0.0.1:{server.server_port}/api/v1'
        (home/'settings.toml').write_text('schema_version = 1\n')
        (home/'config.toml').write_text(f'schema_version = 30\n[llm]\nprovider = "openrouter"\nmodel = "{MODEL["id"]}"\napi_key_env = "HEYCODE_TOOL_CARD_FIXTURE"\nbase_url = "{base}"\n')
        tui=FullScreenTui(str(home),str(work),binary,fake=False,color=True,rows=44,columns=130,extra=['--provider','openrouter','--model',MODEL['id'],'--set',f'llm.base_url={base}','--set','llm.api_key_env=HEYCODE_TOOL_CARD_FIXTURE'])
        class Screen(pyte.Screen):
            def set_mode(self, *modes, **kwargs):
                if kwargs.get('private') and 1049 in modes: self.reset()
                return super().set_mode(*modes, **kwargs)
        screen=Screen(130,44); stream=pyte.ByteStream(screen); seen=''

        def read(seconds=.15):
            nonlocal seen
            data=tui.read(seconds); stream.feed(data); seen+=plain(data)
        def send(data):
            nonlocal seen
            seen=''; os.write(tui.fd,data)
        def wait(text):
            deadline=time.monotonic()+25
            while time.monotonic()<deadline:
                read()
                if shows(seen,text) or text in '\n'.join(screen.display): return
                if not tui.alive(): break
            raise AssertionError(f'missing {text}: {seen[-4000:]}')
        def snapshot(name):
            read(.35)
            columns = 131 if screen.columns == 130 else 130
            screen.resize(lines=44, columns=columns)
            fcntl.ioctl(tui.fd, termios.TIOCSWINSZ, struct.pack('HHHH',44,columns,0,0)); read(.5)
            value='\n'.join(screen.display); (output/f'{name}.txt').write_text(value); render_screen(screen,output/f'{name}.png'); return value
        try:
            wait('Welcome to heycode'); send(b'\x1b[B\x1b[B\r'); wait('Select a provider')
            send(b'OpenRouter\r'); wait('Paste your OpenRouter API key'); send(b'fixture-key\r'); wait('Choose a model'); send(b'\r'); wait('Default (shift+tab to cycle)')
            initial=snapshot('idle'); assert 'Tasks 0' not in initial
            send(b'/permissions full_access\r'); wait('Full access')
            if transcript:
                send(b'Inspect the project and report the failing search\r')
                wait('Checking tool permissions before reading files.')
                thinking_seen.set()
                wait(final_text)
                rendered=snapshot('transcript')
                assert 'unknown tool: todo_write' in rendered, rendered
                assert 'todo_write(' not in rendered, rendered
                assert 'Read 1 file' in rendered and 'Added 1 line, removed 1 line' in rendered, rendered
                assert 'Searched for 1 pattern' in rendered, rendered
                assert 'Ran 1 shell command' in rendered, rendered
                assert 'input detail 24' not in rendered, rendered
                assert 'File not found' in rendered and 'failed' in rendered, rendered
                assert '1 matches' not in rendered, rendered
                assert 'Tasks 5' not in rendered, rendered
                # Opening the read card must reveal its retained lines.
                row=next(i+1 for i,line in enumerate(screen.display) if '▸ Read 1 file' in line)
                col=screen.display[row-1].index('▸')+1
                send(f'\x1b[<0;{col};{row}M\x1b[<0;{col};{row}m'.encode())
                expanded=snapshot('read-expanded')
                assert 'input detail 0' in expanded, expanded
                assert '1    input detail 0' in expanded, expanded
                send(b'\x1b[6~')
                scrolled=snapshot('read-scrolled')
                assert 'input detail 24' in scrolled, scrolled
                requests_before_focus = len(requests)
                send(b'/focus\r')
                wait('Focus view enabled')
                focused = snapshot('focus-tool-summary')
                assert final_text in focused, focused
                assert 'Edited 1 file +1 -1' in focused, focused
                assert 'read 1 file' in focused, focused
                send(b'/focus\r')
                wait('Focus view disabled')
                restored = snapshot('focus-restored')
                assert final_text in restored, restored
                assert 'Added 1 line, removed 1 line' in restored, restored
                assert len(requests) == requests_before_focus
                events=[json.loads(line) for log in home.rglob('session.jsonl') for line in log.read_text().splitlines()]
                (output/'events.json').write_text(json.dumps(events,indent=2))
                result = {
                    'todo_write_removed': True,
                    'glob_matches': 8,
                    'failure_is_not_a_match': True,
                    'read_expands': True,
                    'focus_preserves_tool_summary_and_answer': True,
                    'focus_restore_preserves_full_transcript': True,
                    'focus_provider_requests': len(requests) - requests_before_focus,
                }
                (output/'result.json').write_text(json.dumps(result,indent=2))
                return result
            send(b'/permissions default\r'); wait('Default')
            send(b'ultracode workflow inspect this project')
            draft=snapshot('workflow-keywords')
            assert 'Workflow requested for this turn' in draft
            draft_row=next(i for i,line in enumerate(screen.display) if 'ultracode workflow inspect' in line)
            assert not any(cell.underscore for cell in screen.buffer[draft_row].values())
            send(b'\r'); wait('Checking tool permissions before reading files.')
            thinking=snapshot('live-thinking')
            assert 'Thinking… (' in thinking
            assert 'opaque-fixture-state' not in thinking
            thinking_seen.set()
            wait('Permission requested')
            assert 'allow edits this session' in snapshot('first-approval')
            send(b'a'); wait('Permission requested')
            assert (work/'first.txt').read_text()=='FIRST'
            assert (work/'second.txt').read_text()=='SECOND'
            assert not (work/'denied.txt').exists()
            assert 'bash' in snapshot('shell-approval')
            send(b'n'); wait('Permission requested'); send(b'y'); wait('Permission requested'); send(b'y'); wait(final_text)
            collapsed=snapshot('collapsed')
            assert not (work/'denied.txt').exists()
            assert 'rejected' in collapsed
            assert 'Task output' not in collapsed and 'job_output(' not in collapsed and 'total_bytes' not in collapsed
            assert 'Ran 1 shell command' in collapsed
            assert 'Permission requested' not in collapsed and '[job ' not in collapsed
            # The tail may occur in the compact command arguments, so check its output row.
            rows=[i for i,line in enumerate(screen.display) if '▸ Ran 1 shell command' in line]
            assert rows,collapsed
            row=rows[-1]+1; line=screen.display[row-1]; col=line.index('▸')+1
            send(f'\x1b[<0;{col};{row}M\x1b[<0;{col};{row}m'.encode())
            expanded=snapshot('expanded'); assert '▾ Ran 1 shell command' in expanded
            assert 'OUTPUT_LINE_' in expanded
            row=next(i+1 for i,line in enumerate(screen.display) if '▾ Ran 1 shell command' in line)
            col=screen.display[row-1].index('▾')+1
            send(f'\x1b[<0;{col};{row}M\x1b[<0;{col};{row}m'.encode())
            recollapsed=snapshot('recollapsed'); assert '▾ Ran 1 shell command' not in recollapsed
            events=[json.loads(line) for log in home.rglob('session.jsonl') for line in log.read_text().splitlines()]
            users=[e for e in events if e['kind']=='user/message']
            assert len(users)==1,users
            tokens=sum(e.get('data',{}).get('usage',{}).get('prompt_tokens',0) for e in events if e['kind']=='assistant/message' and e.get('data',{}).get('usage'))
            assert tokens > 1000000, tokens
            result={'cumulative_input_tokens':tokens,'provider_requests':len(requests),'future_files_written':2,'denied_shell_did_not_run':True,'single_user_message':True,'mouse_expand_collapse':True}
            (output/'events.json').write_text(json.dumps(events,indent=2)); (output/'result.json').write_text(json.dumps(result,indent=2)); return result
        finally:
            (output/'terminal.ansi').write_bytes(tui.transcript); tui.close(); server.shutdown(); server.server_close(); thread.join(timeout=2)

if __name__=='__main__':
    parser=argparse.ArgumentParser(); parser.add_argument('--binary',default='target/debug/heycode'); parser.add_argument('--output',default='tmp/live-product-audit/tool-cards-pty'); parser.add_argument('--transcript',action='store_true'); args=parser.parse_args()
    output=Path(args.output).resolve(); output.mkdir(parents=True,exist_ok=True)
    print(json.dumps(journey(str(Path(args.binary).resolve()),output,args.transcript),indent=2))
