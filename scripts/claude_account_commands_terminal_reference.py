#!/usr/bin/env python3
"""Capture Claude's unauthenticated `/logout` and `/login` surfaces in isolation.

The process uses a disposable HOME, CLAUDE_CONFIG_DIR and workspace, no inherited
credentials, and closed loopback provider/proxy targets. `/logout` runs against
an already signed-out configuration. `/login` is opened only far enough to show
its method picker and is then cancelled with Escape, so no browser, OAuth
request, or model prompt is started.
"""
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

def run(out: Path, command: str, theme: str = 'dark', columns: int = 110, rows: int = 42):
    out.mkdir(parents=True, exist_ok=False)
    executable = Path(shutil.which('claude') or '/missing-claude').resolve()
    captures = []
    raw = bytearray()
    screen = Screen(columns, rows)
    stream = TerminalByteStream(screen)
    with tempfile.TemporaryDirectory(prefix='terminal-account-reference-') as folder:
        root = Path(folder)
        for part in ('home', 'config', 'workspace'): (root / part).mkdir()
        (root/'config/.claude.json').write_text(json.dumps({'hasCompletedOnboarding':True,'theme':theme,'lastOnboardingVersion':'2.1.268'}))
        env = {k:v for k,v in os.environ.items() if not any(s in k.upper() for s in ('API_KEY','ACCESS_TOKEN','AUTH_TOKEN','OAUTH_TOKEN')) and not k.startswith('CLAUDE_CODE_') and k != 'CLAUDECODE'}
        env.update(HOME=str(root/'home'), CLAUDE_CONFIG_DIR=str(root/'config'), TERM='xterm-256color', COLORTERM='truecolor', CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC='1', ANTHROPIC_BASE_URL='http://127.0.0.1:9', HTTPS_PROXY='http://127.0.0.1:9', HTTP_PROXY='http://127.0.0.1:9', BROWSER='/usr/bin/false')
        env.pop('NO_COLOR',None)
        args=[str(executable),'--bare','--restricted','--strict-mcp-config','--no-chrome','--setting-sources','','--session-id',str(uuid.uuid4()),'--name','isolated-account-commands']
        master,slave=pty.openpty()
        fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',rows,columns,0,0))
        proc=subprocess.Popen(args,cwd=root/'workspace',env=env,stdin=slave,stdout=slave,stderr=slave,start_new_session=True)
        os.close(slave)
        def read(seconds):
            deadline=time.monotonic()+seconds
            while time.monotonic()<deadline:
                ready,_,_=select.select([master],[],[],min(.1,max(0,deadline-time.monotonic())))
                if ready:
                    try: data=os.read(master,65536)
                    except OSError: break  # the process may exit after /logout
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
            if command == 'logout':
                send(b'/logout');read(.6);capture('01-logout-command-menu')
                send(b'\r');read(2.5);capture('02-logout-result')
            else:
                send(b'/login');read(.6);capture('01-login-command-menu')
                send(b'\r');read(2.5);capture('02-login-method-picker')
                send(b'\x1b');read(1.5);capture('03-login-cancelled')
            journal_kinds=[]
            for path in (root/'config').rglob('*.jsonl'):
                for line in path.read_text(errors='replace').splitlines():
                    try: value=json.loads(line)
                    except json.JSONDecodeError: continue
                    if value.get('type') in ('user','assistant'): journal_kinds.append(value.get('type'))
            result={'status':'captured','engine':'Claude Code','binary_sha256':hashlib.sha256(executable.read_bytes()).hexdigest(),'viewport':[columns,rows],'captures':captures,'model_prompt_sent':False,'command':command,'login_method_selected':False,'process_exit_code':proc.poll(),'browser_command':'/usr/bin/false','disposable_home_config_workspace':True,'provider_and_proxy_targets':'closed loopback','journal_message_types':journal_kinds}
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
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--out',type=Path,required=True);p.add_argument('--command',choices=('logout','login'),required=True);p.add_argument('--theme',choices=('dark','light'),default='dark');p.add_argument('--columns',type=int,default=110);p.add_argument('--rows',type=int,default=42);args=p.parse_args();run(args.out,args.command,args.theme,args.columns,args.rows)
