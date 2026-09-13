#!/usr/bin/env python3
"""Read-only route/control dialogs in an isolated Claude process; never selects a route."""
from __future__ import annotations
import argparse, fcntl, hashlib, json, os, pty, re, select, shutil, signal, struct, subprocess, tempfile, termios, time, uuid
from pathlib import Path
import pyte
from terminal_screenshot import TerminalByteStream, render_screen

class Screen(pyte.Screen):
    def set_mode(self, *modes, **kwargs):
        if kwargs.get('private') and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)

def run(out: Path, probe_effort_pointer: bool = False, theme: str = 'dark'):
    out.mkdir(parents=True, exist_ok=False)
    executable = Path(shutil.which('claude') or '/missing-claude').resolve()
    captures = []
    raw = bytearray()
    screen = Screen(110, 42)
    stream = TerminalByteStream(screen)
    with tempfile.TemporaryDirectory(prefix='terminal-route-reference-') as folder:
        root = Path(folder)
        for part in ('home', 'config', 'workspace'): (root / part).mkdir()
        (root/'config/.claude.json').write_text(json.dumps({'hasCompletedOnboarding':True,'theme':theme,'lastOnboardingVersion':'2.1.268'}))
        env = {k:v for k,v in os.environ.items() if not any(s in k.upper() for s in ('API_KEY','ACCESS_TOKEN','AUTH_TOKEN','OAUTH_TOKEN'))}
        env.update(HOME=str(root/'home'), CLAUDE_CONFIG_DIR=str(root/'config'), TERM='xterm-256color', COLORTERM='truecolor', CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC='1', ANTHROPIC_BASE_URL='http://127.0.0.1:9', HTTPS_PROXY='http://127.0.0.1:9', HTTP_PROXY='http://127.0.0.1:9')
        env.pop('NO_COLOR',None)
        args=[str(executable),'--bare','--restricted','--strict-mcp-config','--no-chrome','--setting-sources','','--session-id',str(uuid.uuid4()),'--name','isolated-route-controls']
        master,slave=pty.openpty()
        fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',42,110,0,0))
        proc=subprocess.Popen(args,cwd=root/'workspace',env=env,stdin=slave,stdout=slave,stderr=slave,start_new_session=True)
        os.close(slave)
        def read(seconds):
            deadline=time.monotonic()+seconds
            while time.monotonic()<deadline:
                ready,_,_=select.select([master],[],[],min(.1,max(0,deadline-time.monotonic())))
                if ready:
                    try: data=os.read(master,65536)
                    except OSError: break
                    if not data: break
                    raw.extend(data);stream.feed(data)
            return '\n'.join(screen.display)
        def send(data): os.write(master,data)
        def capture(name):
            text=read(.25);(out/f'{name}.txt').write_text(text);render_screen(screen,out/f'{name}.png',foreground='#20242c' if theme == 'light' else '#dddddd',background='#f8f9fb' if theme == 'light' else '#101014');captures.append(name)
            return text
        result = {'status':'failed', 'captures':captures, 'model_prompt_sent':False}
        try:
            initial=read(4)
            if 'Yes, I trust this folder' in initial and str(root/'workspace').replace('/var/', '/private/var/') in initial:
                send(b'\x1b[B\r');read(3)
            capture('00-start')
            for index,command in enumerate(('model','effort','permissions','sandbox'),1):
                send(b'\x15');send(('/'+command).encode());read(.3);capture(f'{index:02d}-{command}-menu');send(b'\r');read(1.2);capture(f'{index:02d}-{command}-open')
                if command == 'effort' and probe_effort_pointer:
                    rows = list(screen.display)
                    target_row = next(i for i,row in enumerate(rows) if 'low' in row and 'medium' in row and 'high' in row)
                    target_column = rows[target_row].index('low') + 1
                    send(f'\x1b[<0;{target_column};{target_row+1}M'.encode())
                    send(f'\x1b[<0;{target_column};{target_row+1}m'.encode())
                    read(.6);capture('02-effort-pointer-low')
                send(b'\x1b');read(.5);send(b'\x1b');read(.3)
            for command in ('login','logout'):
                send(b'\x15');send(('/'+command).encode());read(.3);capture(f'05-{command}-menu');send(b'\x15');read(.2)
            journal_kinds=[]
            for path in (root/'config').rglob('*.jsonl'):
                for line in path.read_text(errors='replace').splitlines():
                    try: value=json.loads(line)
                    except json.JSONDecodeError: continue
                    if value.get('type') in ('user','assistant'): journal_kinds.append(value.get('type'))
            result={'status':'captured','engine':'Claude Code','binary_sha256':hashlib.sha256(executable.read_bytes()).hexdigest(),'viewport':[110,42],'captures':captures,'model_prompt_sent':False,'route_or_permission_selected':False if not probe_effort_pointer else 'effort click in disposable config; inspect captured state', 'effort_pointer_probed':probe_effort_pointer,'login_logout_executed':False,'disposable_home_config_workspace':True,'provider_and_proxy_targets':'closed loopback','journal_message_types':journal_kinds}
        except Exception as error:
            result.update(status='failed', error=str(error))
        finally:
            (out/'terminal.ansi').write_bytes(raw)
            if proc.poll() is None: proc.send_signal(signal.SIGTERM)
            try: proc.wait(timeout=5)
            except subprocess.TimeoutExpired: proc.kill();proc.wait(timeout=5)
            os.close(master)
        version=re.search(r'Claude Code v([0-9.]+)',(out/'00-start.txt').read_text())
        result['version']=version.group(1) if version else 'unavailable'
        result['theme']=theme
        (out/'result.json').write_text(json.dumps(result,indent=2)+'\n')
        print(json.dumps(result))
        if result['status'] != 'captured': raise SystemExit(1)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--out',type=Path,required=True);p.add_argument('--probe-effort-pointer',action='store_true');p.add_argument('--theme',choices=('dark','light'),default='dark');args=p.parse_args();run(args.out,args.probe_effort_pointer,args.theme)
