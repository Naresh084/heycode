#!/usr/bin/env python3
"""Controlled request/cache attribution and real session-resume terminal check."""
from __future__ import annotations
import argparse, http.server, json, os, threading, time
from pathlib import Path
from tempfile import TemporaryDirectory
import pyte
from plan_review_pty import MODEL
from tui_blackbox import FullScreenTui
from terminal_screenshot import render_screen


def journey(binary: Path, output: Path, configuration=False):
    output.mkdir(parents=True, exist_ok=True)
    requests=[]
    with TemporaryDirectory(prefix='heycode-context-usage-') as temporary:
        root=Path(temporary);home=root/'home';work=root/'work';home.mkdir();work.mkdir()
        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                value={'data':{'label':'fixture'}} if self.path.endswith('/key') else ({'data':MODEL} if '/model/' in self.path else {'data':[MODEL],'total_count':1,'links':{'next':None}})
                body=json.dumps(value).encode();self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
            def do_POST(self):
                request=json.loads(self.rfile.read(int(self.headers['Content-Length'])));requests.append(request);step=len(requests)
                self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers()
                def send(payload):
                    self.wfile.write(('data: '+json.dumps(payload)+'\n\n').encode());self.wfile.flush()
                send({'id':f'context-{step}','model':MODEL['id'],'choices':[{'index':0,'delta':{'content':f'CONTEXT-RESPONSE-{step}','reasoning_details':[{'type':'reasoning.text','text':'Check the request attribution.'}]},'finish_reason':None}]})
                send({'id':f'context-{step}','model':MODEL['id'],'choices':[{'index':0,'delta':{},'finish_reason':'stop'}]})
                usage={'prompt_tokens':10000 if step==1 else 11000,'completion_tokens':5000 if step==1 else 4}
                if step==1:usage.update(prompt_tokens_details={'cached_tokens':6000},completion_tokens_details={'reasoning_tokens':4990})
                send({'id':f'context-{step}','choices':[],'usage':usage})
                self.wfile.write(b'data: [DONE]\n\n');self.wfile.flush()
            def log_message(self,*args):pass
        server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
        threading.Thread(target=server.serve_forever,daemon=True).start()
        base=f'http://127.0.0.1:{server.server_port}/api/v1'
        (home/'settings.toml').write_text('schema_version = 1\n')
        (home/'config.toml').write_text(f'schema_version = 31\n[llm]\nprovider="openrouter"\nmodel="{MODEL["id"]}"\napi_key_env="HEYCODE_CONTEXT_FIXTURE"\nbase_url="{base}"\n')
        os.environ['HEYCODE_CONTEXT_FIXTURE']='fixture-key'
        extra=['--provider','openrouter','--model',MODEL['id'],'--set',f'llm.base_url={base}','--set','llm.api_key_env=HEYCODE_CONTEXT_FIXTURE']
        class Screen(pyte.Screen):
            def set_mode(self,*modes,**kwargs):
                if kwargs.get('private') and 1049 in modes:self.reset()
                return super().set_mode(*modes,**kwargs)
        def launch(resume=None):
            tui=FullScreenTui(str(home),str(work),str(binary),fake=False,color=True,rows=65,columns=110,extra=extra+(['--resume',str(resume)] if resume else []))
            screen=Screen(110,65);return tui,screen,pyte.ByteStream(screen)
        tui,screen,stream=launch()
        def read():stream.feed(tui.read(.15))
        def text():return '\n'.join(screen.display)
        def wait(needle):
            end=time.monotonic()+35
            while time.monotonic()<end:
                read()
                if needle in text():return
            raise AssertionError((needle,text()))
        def send(value):os.write(tui.fd,value);read()
        def capture(name):
            read();(output/f'{name}.txt').write_text(text());render_screen(screen,output/f'{name}.png')
        def context(name):
            send(b'/context\r');wait('compaction strategies:');capture(name)
            rows=screen.display;starts=[i for i,row in enumerate(rows) if row.strip()=='context']
            assert starts, text()
            return '\n'.join(rows[starts[-1]:])
        def events():
            logs=list(home.rglob('session.jsonl'));assert len(logs)==1,logs
            return logs[0],[json.loads(line) for line in logs[0].read_text().splitlines()]
        try:
            wait('shift+tab to cycle')
            send(b'First request\r');wait('CONTEXT-RESPONSE-1');capture('first-response')
            first=context('first-context');assert 'cache read=6000' in first,first
            _,records=events();first_id=[e['data']['request_id'] for e in records if e['kind']=='request/header'][-1]
            assert f'request: {first_id}' in first,first
            send(b'Second request\r');wait('CONTEXT-RESPONSE-2');capture('second-response')
            second=context('second-context');assert 'cache read=6000' not in second,second
            log,records=events();second_id=[e['data']['request_id'] for e in records if e['kind']=='request/header'][-1]
            assert first_id!=second_id and f'request: {second_id}' in second,second
            assert len(requests)==2,requests
            (output/'live-terminal.ansi').write_bytes(tui.transcript);tui.close()
            tui,screen,stream=launch(log);wait('shift+tab to cycle');capture('resumed')
            resumed=context('resumed-context')
            assert f'request: {second_id}' in resumed, resumed
            assert 'cache read=6000' not in resumed,resumed
            assert 'contributors: restored from the latest request' in resumed,resumed
            for contributor in ['system','guidance','messages','tool_results','tools','provider_state','attachments']:
                live_line=next(row.strip() for row in second.splitlines() if row.strip().startswith(contributor+':'))
                assert live_line in resumed,(live_line,resumed)
            contexts=[e['data']['context'] for e in records if e['kind']=='request/context']
            assert len(contexts[-1]['contributors'])==7,contexts[-1]
            assert len(requests)==2,'Resume/context inspection made a model request'
            if configuration:
                headers=[e['data']['header'] for e in records if e['kind']=='request/header']
                assert len(headers)==2 and headers[0]['configuration']['revision']==1
                assert headers[1]['configuration']['revision']==1 and headers[1]['configuration']['changed']==[]
                assert headers[0]['configuration']['sha256']==headers[1]['configuration']['sha256']
                assert requests[0]['tools']==requests[1]['tools']
                assert requests[1]['messages'][:len(requests[0]['messages'])]==requests[0]['messages']
                names=[tool['function']['name'] for tool in requests[0]['tools'] if tool.get('type')=='function']
                assert names==sorted(names), names
                assert 'configuration revision 1: unchanged' in resumed,resumed
                send(b'/plan on\r');wait('Plan (shift+tab to cycle)')
                assert len(requests)==2, 'Plan transition made an unintended model request'
                send(b'Third request after explicit plan change\r');wait('CONTEXT-RESPONSE-3')
                changed=context('changed-context')
                _,records=events();headers=[e['data']['header'] for e in records if e['kind']=='request/header']
                assert headers[-1]['configuration']['revision']==2,headers[-1]['configuration']
                assert 'system' in headers[-1]['configuration']['changed'],headers[-1]['configuration']
                assert 'configuration revision 2: system/guidance' in changed,changed
                assert requests[2]['tools']==requests[1]['tools']
                assert requests[2]['messages'][1:len(requests[1]['messages'])]==requests[1]['messages'][1:]
            (output/'result.json').write_text(json.dumps({'status':'passed','requests':len(requests),'first_request_id':first_id,'second_request_id':second_id,'resumed_scope_retained':True,'configuration_verified':configuration},indent=2))
        finally:
            (output/'terminal.ansi').write_bytes(tui.transcript)
            (output/'requests.json').write_text(json.dumps(requests,indent=2))
            try:
                _,records=events();(output/'events.json').write_text(json.dumps(records,indent=2))
            finally:tui.close();server.shutdown()
    print(f'PASS: {output}')

if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--binary',default='target/debug/heycode');parser.add_argument('--output',default='tmp/terminal-evidence/context-usage-pty');parser.add_argument('--configuration',action='store_true');args=parser.parse_args();journey(Path(args.binary).resolve(),Path(args.output),args.configuration)
