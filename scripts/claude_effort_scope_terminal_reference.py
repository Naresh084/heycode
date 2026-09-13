#!/usr/bin/env python3
"""Observe effort session scope in disposable Claude sessions without inference."""
from __future__ import annotations
import argparse, hashlib, http.server, json, os, shutil, tempfile, threading, uuid
from pathlib import Path
from claude_compaction_terminal_reference import ClaudePty

def run(output: Path):
    output.mkdir(parents=True, exist_ok=False)
    executable = Path(shutil.which('claude')).resolve()
    requests, captures, processes = [], [], []
    class Refuse(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            self.rfile.read(int(self.headers.get('Content-Length', '0')))
            requests.append({'method':'POST', 'path':self.path})
            self.send_error(403)
        def do_GET(self):
            requests.append({'method':'GET', 'path':self.path}); self.send_error(403)
        def log_message(self, *_): pass
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Refuse)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    terminal = None
    result = {'status':'failed', 'model_prompt_sent':False, 'requests':requests, 'captures':captures}
    try:
        with tempfile.TemporaryDirectory(prefix='terminal-effort-scope-') as folder:
            root = Path(folder)
            home, config, workspace = (root/part for part in ('home','config','workspace'))
            for directory in (home,config,workspace): directory.mkdir()
            (config/'.claude.json').write_text(json.dumps({'hasCompletedOnboarding':True,'theme':'dark','lastOnboardingVersion':'2.1.269'}))
            env = {key:value for key,value in os.environ.items() if not any(token in key.upper() for token in ('API_KEY','AUTH_TOKEN','OAUTH_TOKEN','ACCESS_TOKEN')) and not key.startswith('CLAUDE_CODE_USE_')}
            env.update(HOME=str(home), CLAUDE_CONFIG_DIR=str(config), TERM='xterm-256color', COLORTERM='truecolor',
                       CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC='1', CLAUDE_CODE_REMOTE_CONTROL='0',
                       ANTHROPIC_API_KEY='local-only-dummy', ANTHROPIC_BASE_URL=f'http://127.0.0.1:{server.server_port}',
                       HTTP_PROXY='http://127.0.0.1:9', HTTPS_PROXY='http://127.0.0.1:9', NO_PROXY='127.0.0.1,localhost')
            env.pop('NO_COLOR',None)
            common = ['--safe-mode','--strict-mcp-config','--no-chrome','--setting-sources','user,project,local','--model','opus']
            original = str(uuid.uuid4())
            def launch(arguments):
                nonlocal terminal
                terminal = ClaudePty(str(executable),env,workspace,[*common,*arguments],columns=110,rows=42)
                processes.append(terminal.process.pid); terminal.ready()
            def capture(name):
                terminal.capture(output,name); captures.append(name)
            def close(name):
                nonlocal terminal
                terminal.send(b'/exit\r');terminal.read(1);terminal.close()
                (output/f'{name}.ansi').write_bytes(terminal.transcript);terminal=None
            launch(['--session-id',original,'--name','Effort scope original'])
            terminal.send(b'/effort\r');terminal.wait_for('Effort');capture('01-initial')
            terminal.send(b'\x1b[D\x1b[D');terminal.read(.4);capture('02-low-preview')
            terminal.send(b's');terminal.read(.8);capture('03-session-commit')
            close('original')
            settings = config/'settings.json'
            result['user_effort_after_session_commit'] = json.loads(settings.read_text()).get('effortLevel') if settings.exists() else None
            launch(['--resume',original])
            terminal.send(b'/effort\r');terminal.wait_for('Effort');capture('04-resumed-picker')
            terminal.send(b'\x1b');terminal.read(.3);close('resumed')
            launch(['--session-id',str(uuid.uuid4()),'--name','Effort scope fresh'])
            terminal.send(b'/effort\r');terminal.wait_for('Effort');capture('05-fresh-picker')
            terminal.send(b'\x1b');terminal.read(.3);close('fresh')
            assert not requests, requests
            result.update(status='captured', original_session=original, binary_sha256=hashlib.sha256(executable.read_bytes()).hexdigest(),
                          viewport=[110,42], disposable_home_config_workspace=True, seeded_history=False, owned_pids=processes, all_owned_processes_reaped=True)
    except Exception as error:
        result['error']=str(error)
        if terminal:
            terminal.capture(output,'failure');(output/'failure.ansi').write_bytes(terminal.transcript)
    finally:
        if terminal: terminal.close()
        server.shutdown();server.server_close()
        (output/'result.json').write_text(json.dumps(result,indent=2)+'\n')
        print(json.dumps(result),flush=True)
    if result['status']!='captured': raise SystemExit(1)

if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--out',type=Path,required=True);run(parser.parse_args().out)
