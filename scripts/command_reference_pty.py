#!/usr/bin/env python3
"""Capture fresh isolated command surfaces; no conversation prompt or inference.

Claude uses a new disposable session with integrations disabled. heycode uses its
controlled fake inference composition, but executes the real command owners,
renderer, session store and keyboard loop. Captures establish observed surfaces,
not an automatic claim of visual parity or paid-provider validation.
"""
from __future__ import annotations
import argparse
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import struct
import subprocess
import tempfile
import termios
import time
import uuid
import pyte
from terminal_screenshot import TerminalByteStream, render_screen

class Screen(pyte.Screen):
    def set_mode(self, *modes, **kwargs):
        if kwargs.get('private') and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)

def run(engine: str, binary: Path, output: Path):
    output.mkdir(parents=True, exist_ok=True)
    session_id = str(uuid.uuid4())
    screen = Screen(110, 42)
    stream = TerminalByteStream(screen)
    transcript = bytearray()
    captures = []
    with tempfile.TemporaryDirectory(prefix='command-reference-') as folder:
        root = Path(folder)
        workspace = root / 'workspace'; workspace.mkdir()
        home = root / 'heycode'; home.mkdir()
        env = os.environ.copy()
        env.update(TERM='xterm-256color', COLORTERM='truecolor')
        env.pop('NO_COLOR', None)
        if engine == 'claude':
            executable = shutil.which('claude')
            if not executable: raise RuntimeError('Claude unavailable')
            args = [executable, '--safe-mode', '--strict-mcp-config', '--no-chrome', '--session-id', session_id, '--name', 'command-parity-reference']
        else:
            env['HEYCODE_HOME'] = str(home)
            args = [str(binary.resolve()), '--fake', '--no-background', '--trust-workspace']
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 42, 110, 0, 0))
        proc = subprocess.Popen(args, cwd=workspace, env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        def read(seconds):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                ready, _, _ = select.select([master], [], [], max(0, min(.1, deadline-time.monotonic())))
                if not ready: continue
                try: data = os.read(master, 65536)
                except OSError: break
                if not data: break
                transcript.extend(data); stream.feed(data)
            return '\n'.join(screen.display)
        def send(data): os.write(master, data)
        def capture(name):
            visible = read(.5)
            (output / f'{name}.txt').write_text(visible)
            render_screen(screen, output / f'{name}.png')
            captures.append(name)
            return visible
        blocker = None
        try:
            initial = read(4)
            if engine == 'claude' and 'trust' in initial.lower() and 'folder' in initial.lower():
                send(b'\x1b[B\r'); initial = read(3)
            if engine == 'heycode' and b'\x1b[?2004h' not in transcript:
                initial = read(8)
            capture('00-start')
            cases = [
                ('01-rename', '/rename Parity   command session'),
                ('02-copy-missing', '/copy 2'),
                ('03-context', '/context'),
                ('04-help', '/help'),
            ]
            if engine == 'heycode':
                cases.append(('05-release-notes', '/release-notes'))
            for name, command in cases:
                send(command.encode()); read(.3); send(b'\r'); read(2)
                visible = capture(name)
                if engine == 'heycode':
                    expected = {
                        '01-rename': ('Renamed to Parity   command session',),
                        '02-copy-missing': ('no completed assistant answer 2 to copy',),
                        '03-context': ('no durable request header', 'no committed request envelope'),
                        '04-help': ('Help', 'General', 'Commands', 'current bindings'),
                        '05-release-notes': ('Unreleased',),
                    }
                    for needle in expected[name]:
                        if needle not in visible:
                            raise AssertionError(f'{name}: expected {needle!r} in terminal')
                elif name == '04-help':
                    send(b'\t'); read(.5); capture('04b-help-commands')
                    send(b'\t'); read(.5); capture('04c-help-custom')
                send(b'\x1b'); read(.3)
            if proc.poll() is not None:
                raise RuntimeError(f'Process exited during captures: {proc.returncode}')
        except Exception as error:
            blocker = f'{type(error).__name__}: {error}'
            capture('blocked')
        finally:
            (output/'terminal.ansi').write_bytes(transcript)
            if engine == 'heycode':
                events=[]
                for path in sorted(home.rglob('session.jsonl')):
                    events.extend(json.loads(line) for line in path.read_text().splitlines() if line.strip())
                (output/'events.json').write_text(json.dumps(events, indent=2))
                if not events:
                    blocker = blocker or 'No durable session events found'
                unexpected = [e['kind'] for e in events if e.get('kind') in ('user/message', 'request/header')]
                if unexpected:
                    blocker = blocker or f'Local commands unexpectedly entered inference: {unexpected}'
            if proc.poll() is None: proc.send_signal(signal.SIGTERM)
            try: proc.wait(timeout=5)
            except subprocess.TimeoutExpired: proc.kill(); proc.wait(timeout=5)
            os.close(master)
    result = {'engine': engine, 'status':'blocked' if blocker else 'captured', 'captures':captures, 'blocker':blocker, 'inference_prompt_sent':False, 'claude_session_id':session_id if engine=='claude' else None}
    (output/'result.json').write_text(json.dumps(result,indent=2))
    print(json.dumps(result))
    if blocker:
        raise SystemExit(1)

if __name__ == '__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--engine', choices=['claude','heycode'], required=True)
    p.add_argument('--binary', type=Path, default=Path('target/debug/heycode'))
    p.add_argument('--output', type=Path, required=True)
    a=p.parse_args(); run(a.engine,a.binary,a.output)
