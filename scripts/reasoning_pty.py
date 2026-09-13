#!/usr/bin/env python3
"""Exercise readable/opaque reasoning through the actual CLI and local SSE transport."""
from __future__ import annotations
import argparse
import http.server
import json
import os
import threading
import time
from pathlib import Path
from tempfile import TemporaryDirectory

import pyte
from task_console_pty import MODEL, Screen
from tui_blackbox import FullScreenTui
from terminal_screenshot import render_screen

OPAQUE = 'OPAQUE-CONTINUATION-NEVER-DISPLAY'
READABLE = 'Reasoning visible first segment.'
SECOND = 'Second readable summary.'


def run(binary: Path, output: Path):
    output.mkdir(parents=True, exist_ok=True)
    requests = []
    active = threading.Event()
    release = threading.Event()
    current = {'case': 'none'}

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            value = {'data': {'label': 'reasoning fixture'}} if self.path.endswith('/key') else ({'data': MODEL} if '/model/' in self.path else {'data': [MODEL], 'total_count': 1, 'links': {'next': None}})
            data = json.dumps(value).encode()
            self.send_response(200); self.send_header('Content-Type', 'application/json'); self.send_header('Content-Length', str(len(data))); self.end_headers(); self.wfile.write(data)

        def do_POST(self):
            requests.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
            case = current['case']
            self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.end_headers()
            def chunk(delta, finish=None):
                data = {'id': f'reasoning-{case}', 'model': MODEL['id'], 'choices': [{'index': 0, 'delta': delta, 'finish_reason': finish}]}
                self.wfile.write(('data: ' + json.dumps(data) + '\n\n').encode()); self.wfile.flush()
            try:
                if case == 'empty': chunk({'reasoning': ''})
                elif case == 'whitespace': chunk({'reasoning': ' \n\t '})
                elif case == 'opaque': chunk({'reasoning_details': [{'type': 'reasoning.encrypted', 'data': OPAQUE}]})
                elif case in ('readable', 'mixed', 'cancel', 'error'):
                    chunk({'reasoning_details': [{'type': 'reasoning.text', 'text': READABLE}]})
                    if case == 'mixed': chunk({'reasoning_details': [{'type': 'reasoning.encrypted', 'data': OPAQUE}]})
                    chunk({'reasoning_details': [{'type': 'reasoning.summary', 'summary': SECOND}]})
                active.set()
                if case in ('cancel', 'error', 'opaque'): release.wait(12)
                if case == 'cancel': return
                if case == 'error':
                    self.wfile.write(b'data: {"error":{"message":"REASONING-FIXTURE-ERROR","code":400}}\n\n'); self.wfile.flush(); return
                chunk({'content': f'REASONING-DONE-{case}'}); chunk({}, 'stop')
                self.wfile.write(b'data: [DONE]\n\n'); self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError): pass

        def log_message(self, *_): pass

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    results = []
    try:
        for case in ('none', 'empty', 'whitespace', 'opaque', 'readable', 'mixed', 'cancel', 'error'):
            current['case'] = case; active.clear(); release.clear()
            case_out = output / case; case_out.mkdir(exist_ok=True)
            with TemporaryDirectory(prefix='heycode-reasoning-') as temporary:
                root = Path(temporary); home = root / 'home'; work = root / 'work'; home.mkdir(); work.mkdir()
                base = f'http://127.0.0.1:{server.server_port}/api/v1'
                (home / 'settings.toml').write_text('schema_version = 1\n')
                (home / 'config.toml').write_text(f'schema_version = 31\n[llm]\nprovider="openrouter"\nmodel="{MODEL["id"]}"\napi_key_env="HEYCODE_REASONING_FIXTURE"\nbase_url="{base}"\n')
                os.environ['HEYCODE_REASONING_FIXTURE'] = 'fixture'
                extra = ['--provider', 'openrouter', '--model', MODEL['id'], '--set', f'llm.base_url={base}', '--set', 'llm.api_key_env=HEYCODE_REASONING_FIXTURE']
                def launch(resume=None):
                    tui = FullScreenTui(str(home), str(work), str(binary), fake=False, color=True, rows=45, columns=100, extra=extra + (['--resume', str(resume)] if resume else []))
                    screen = Screen(100, 45); return tui, screen, pyte.ByteStream(screen)
                tui, screen, stream = launch()
                def read(): stream.feed(tui.read(.15))
                def text(): return '\n'.join(screen.display)
                def wait(needle):
                    end = time.monotonic() + 25
                    while time.monotonic() < end:
                        read()
                        if needle in text(): return
                    raise AssertionError((case, needle, text()))
                def send(data): os.write(tui.fd, data); read()
                def capture(name):
                    read(); (case_out / f'{name}.txt').write_text(text()); render_screen(screen, case_out / f'{name}.png')
                    assert OPAQUE not in text(), text()
                def click_thought():
                    row, line = next((i, row) for i, row in enumerate(screen.display) if 'ctrl+r' in row and ('Thought' in row or 'Thinking interrupted' in row))
                    column = next(i for i, c in enumerate(line) if not c.isspace())
                    send(f'\x1b[<0;{column+1};{row+1}M\x1b[<0;{column+1};{row+1}m'.encode())
                try:
                    wait('shift+tab to cycle'); send(f'Test {case} reasoning\r'.encode())
                    end = time.monotonic() + 20
                    while not active.is_set() and time.monotonic() < end: read()
                    assert active.is_set(), (case, text())
                    if case == 'opaque':
                        capture('active'); assert 'ctrl+r' not in text(), text(); release.set()
                    if case in ('cancel', 'error'):
                        capture('active')
                        if case == 'cancel': send(b'\x1b')
                        release.set()
                        # Wait for the stream to settle, then inspect disclosure state.
                        for _ in range(12): read()
                    else: wait(f'REASONING-DONE-{case}')
                    capture('collapsed')
                    readable = case in ('readable', 'mixed', 'cancel', 'error')
                    if readable:
                        assert ('Thinking interrupted' if case in ('cancel', 'error') else 'Thought for') in text(), text()
                        click_thought(); wait(READABLE); wait(SECOND); capture('mouse-expanded')
                        send(b'\x12'); capture('keyboard-collapsed'); assert READABLE not in text(), text()
                    else:
                        assert 'ctrl+r' not in text() and 'Thought for' not in text(), text()
                        send(b'\x12'); capture('no-disclosure'); assert READABLE not in text() and 'ctrl+r' not in text(), text()
                    logs = list(home.rglob('session.jsonl')); assert len(logs) == 1, logs
                    records = [json.loads(line) for line in logs[0].read_text().splitlines()]
                    (case_out / 'events.json').write_text(json.dumps(records, indent=2))
                    (case_out / 'live.ansi').write_bytes(tui.transcript)
                    count = len(requests); tui.close()
                    tui, screen, stream = launch(logs[0]); wait('shift+tab to cycle'); capture('replayed')
                    assert len(requests) == count, 'Resume unexpectedly sent a provider request'
                    if readable:
                        assert ('Thinking interrupted' if case in ('cancel', 'error') else 'Thought') in text(), text()
                        click_thought(); wait(READABLE); wait(SECOND); capture('replayed-expanded')
                    else:
                        assert 'ctrl+r' not in text() or readable, text()
                    results.append({'case': case, 'status': 'passed', 'requests': len(requests) - count + 1, 'readable': readable})
                finally:
                    release.set(); (case_out / 'terminal.ansi').write_bytes(tui.transcript); tui.close()
    finally:
        release.set(); server.shutdown(); (output / 'requests.json').write_text(json.dumps(requests, indent=2)); (output / 'result.json').write_text(json.dumps(results, indent=2))
    print(f'PASS: {len(results)} reasoning cases in {output}')

if __name__ == '__main__':
    parser = argparse.ArgumentParser(); parser.add_argument('--binary', default='target/debug/heycode'); parser.add_argument('--output', default='tmp/terminal-evidence/reasoning-pty')
    args = parser.parse_args(); run(Path(args.binary).resolve(), Path(args.output))
