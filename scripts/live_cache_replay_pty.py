#!/usr/bin/env python3
"""Render a completed prefix of a real-provider session without inference calls.

Startup catalog responses are local fixtures; all displayed request/cache facts
come from the supplied unmodified session-event prefix. Source hashes are saved.
"""
import argparse, copy, hashlib, http.server, json, os, tempfile, threading, time
from pathlib import Path
import pyte
from plan_review_pty import MODEL
from terminal_screenshot import render_screen
from tui_blackbox import FullScreenTui


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--session', type=Path, required=True)
    parser.add_argument('--session-id', help='Original directory identity when using a flattened exported log')
    parser.add_argument('--through-turn', type=int, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args=parser.parse_args();args.output.mkdir(parents=True,exist_ok=False)
    source=args.session.read_bytes();lines=source.splitlines(keepends=True)
    events=[];prefix=[]
    for line in lines:
        event=json.loads(line);events.append(event);prefix.append(line)
        if event['kind']=='turn/end' and event['data']['turn']==args.through_turn:break
    assert events[-1]['kind']=='turn/end' and events[-1]['data']['turn']==args.through_turn
    header=next(e['data'] for e in reversed(events) if e['kind']=='request/header')
    metadata=next(e['data'] for e in reversed(events) if e['kind']=='assistant/response-metadata' and e['data']['request_id']==header['request_id'])
    cache=metadata['metadata']['cache_usage'];expected=f"cache read={cache['cache_read_tokens']}"
    model=copy.deepcopy(MODEL);model['id']=header['header']['model'];model['canonical_slug']=model['id'];model['name']='Offline replay catalog'
    posts=[]
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self,*args):pass
        def do_GET(self):
            data={'data':{'label':'offline'}} if self.path.endswith('/key') else ({'data':model} if '/model/' in self.path else {'data':[model]})
            body=json.dumps(data).encode();self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
        def do_POST(self):
            posts.append(self.path);self.send_error(503,'Inference forbidden for evidence replay')
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
    threading.Thread(target=server.serve_forever,daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='heycode-live-cache-replay-') as tmp:
            root=Path(tmp);home=root/'home';home.mkdir();work=root/'work';work.mkdir();session=root/(args.session_id or args.session.parent.name);session.mkdir()
            log=session/'session.jsonl';log.write_bytes(b''.join(prefix))
            os.environ['HEYCODE_OFFLINE_REPLAY']='offline-only'
            base=f'http://127.0.0.1:{server.server_port}/api/v1'
            (home/'config.toml').write_text(f'schema_version=31\n[llm]\nprovider="openrouter"\nmodel="{model["id"]}"\nbase_url="{base}"\napi_key_env="HEYCODE_OFFLINE_REPLAY"\n')
            tui=FullScreenTui(str(home),str(work),str(args.binary.resolve()),fake=False,color=True,rows=65,columns=110,
                extra=['--resume',str(log),'--set',f'llm.base_url={base}','--set','llm.api_key_env=HEYCODE_OFFLINE_REPLAY'])
            screen=pyte.Screen(110,65);stream=pyte.ByteStream(screen)
            def read():stream.feed(tui.read(.15))
            def text():return '\n'.join(screen.display)
            def wait(needle):
                end=time.monotonic()+35
                while time.monotonic()<end:
                    read()
                    if needle in text():return
                raise AssertionError((needle,text()))
            try:
                wait('shift+tab to cycle');os.write(tui.fd,b'/context\r');wait(expected)
                assert header['request_id'] in text(),text()
                read();(args.output/'context.txt').write_text(text());render_screen(screen,args.output/'context.png')
                os.write(tui.fd,b'/usage\r');wait('derived cost' if 'derived cost' in text() else 'cost')
                read();(args.output/'usage.txt').write_text(text());render_screen(screen,args.output/'usage.png')
                assert not posts,posts
                (args.output/'evidence.json').write_text(json.dumps({'source_sha256':hashlib.sha256(source).hexdigest(),'prefix_sha256':hashlib.sha256(b''.join(prefix)).hexdigest(),
                    'last_original_seq':events[-1]['seq'],'latest_request_id':header['request_id'],'cache':cache,'inference_requests':len(posts)},indent=2))
            finally:tui.close()
    finally:server.shutdown();server.server_close()
    print('PASS',args.output)

if __name__=='__main__':main()
