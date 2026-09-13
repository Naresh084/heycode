#!/usr/bin/env python3
"""Capture one Claude slash-command panel in isolation without any model prompt.

Each invocation starts a fresh disposable Claude process (disposable HOME,
CLAUDE_CONFIG_DIR and workspace, inherited credentials stripped, closed loopback
provider and proxy targets), types the command, captures the completion menu,
presses Enter, captures the opened surface at a few settle points, presses the
requested dismissal keys and captures the result. Only dialog-style commands
belong here; commands that submit a model turn must not be passed.
"""
from __future__ import annotations
import argparse, fcntl, hashlib, json, os, pty, re, select, shutil, signal, struct, subprocess, tempfile, termios, time, uuid
from pathlib import Path
import pyte
from terminal_screenshot import TerminalByteStream, render_screen

KEYS = {'escape': b'\x1b', 'enter': b'\r', 'down': b'\x1b[B', 'up': b'\x1b[A', 'tab': b'\t', 'space': b' ', 'backspace': b'\x7f'}

class Screen(pyte.Screen):
    def set_mode(self, *modes, **kwargs):
        if kwargs.get('private') and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)

def run(out: Path, command: str, keys: list[str], theme: str, columns: int, rows: int, settle: float):
    out.mkdir(parents=True, exist_ok=False)
    executable = Path(shutil.which('claude') or '/missing-claude').resolve()
    captures, raw = [], bytearray()
    screen = Screen(columns, rows)
    stream = TerminalByteStream(screen)
    slug = command.strip('/').replace(' ', '-') or 'command'
    with tempfile.TemporaryDirectory(prefix='terminal-panel-reference-') as folder:
        root = Path(folder)
        for part in ('home', 'config', 'workspace'): (root / part).mkdir()
        (root/'workspace/README.md').write_text('# Isolated panel reference workspace\n')
        (root/'config/.claude.json').write_text(json.dumps({'hasCompletedOnboarding':True,'theme':theme,'lastOnboardingVersion':'2.1.268'}))
        env = {k:v for k,v in os.environ.items() if not any(s in k.upper() for s in ('API_KEY','ACCESS_TOKEN','AUTH_TOKEN','OAUTH_TOKEN')) and not k.startswith('CLAUDE_CODE_') and k != 'CLAUDECODE'}
        env.update(HOME=str(root/'home'), CLAUDE_CONFIG_DIR=str(root/'config'), TERM='xterm-256color', COLORTERM='truecolor', CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC='1', ANTHROPIC_BASE_URL='http://127.0.0.1:9', HTTPS_PROXY='http://127.0.0.1:9', HTTP_PROXY='http://127.0.0.1:9', BROWSER='/usr/bin/false', EDITOR='/usr/bin/false', VISUAL='/usr/bin/false')
        env.pop('NO_COLOR', None)
        args=[str(executable),'--bare','--restricted','--strict-mcp-config','--no-chrome','--setting-sources','','--session-id',str(uuid.uuid4()),'--name',f'isolated-{slug}-panel']
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
                    except OSError: break
                    if not data: break
                    raw.extend(data);stream.feed(data)
            return '\n'.join(screen.display)
        def send(data): os.write(master,data)
        def capture(name):
            text=read(.25);(out/f'{name}.txt').write_text(text);render_screen(screen,out/f'{name}.png',foreground='#20242c' if theme == 'light' else '#dddddd',background='#f8f9fb' if theme == 'light' else '#101014');captures.append(name)
            return text
        result = {'status':'failed','captures':captures,'model_prompt_sent':False}
        try:
            initial=read(4)
            if 'Yes, I trust this folder' in initial and str(root/'workspace').replace('/var/', '/private/var/') in initial:
                send(b'\x1b[B\r');read(3)
            capture('00-start')
            send(command.encode());read(.6);capture('01-command-menu')
            send(b'\r');read(settle);capture('02-opened')
            for index, key in enumerate(keys, start=3):
                send(KEYS[key]);read(1.0);capture(f'{index:02d}-after-{key}')
            journal_kinds=[]
            for path in (root/'config').rglob('*.jsonl'):
                for line in path.read_text(errors='replace').splitlines():
                    try: value=json.loads(line)
                    except json.JSONDecodeError: continue
                    if value.get('type') in ('user','assistant'): journal_kinds.append(value.get('type'))
            result={'status':'captured','engine':'Claude Code','binary_sha256':hashlib.sha256(executable.read_bytes()).hexdigest(),'viewport':[columns,rows],'captures':captures,'command':command,'keys':keys,'model_prompt_sent':False,'process_exit_code':proc.poll(),'disposable_home_config_workspace':True,'provider_and_proxy_targets':'closed loopback','journal_message_types':journal_kinds}
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
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--out',type=Path,required=True);p.add_argument('--command',required=True)
    p.add_argument('--keys',nargs='*',default=['escape'],choices=sorted(KEYS))
    p.add_argument('--theme',choices=('dark','light'),default='dark');p.add_argument('--columns',type=int,default=110);p.add_argument('--rows',type=int,default=42);p.add_argument('--settle',type=float,default=2.0)
    a=p.parse_args();run(a.out,a.command,a.keys,a.theme,a.columns,a.rows,a.settle)
