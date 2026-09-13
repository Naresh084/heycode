#!/usr/bin/env python3
"""Paired, prompt-free /exit lifecycle in isolated CLI instances."""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import re
import shutil
import tempfile
import threading
import time
import uuid
from workspace_transitions_pty import Terminal
from terminal_screenshot import TerminalByteStream


def run(binary, output):
    output.mkdir(parents=True, exist_ok=False)
    requests = []
    class Refuse(http.server.BaseHTTPRequestHandler):
        def refuse(self):
            size = int(self.headers.get('Content-Length', '0'))
            if size: self.rfile.read(size)
            requests.append({'method': self.command, 'path': self.path})
            body = b'{"error":{"message":"local reference only"}}'
            self.send_response(503)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        do_POST = do_GET = do_CONNECT = refuse
        def log_message(self, *_): pass
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Refuse)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    results = {}
    try:
        for engine in ['heycode', 'claude']:
            terminal = None
            with tempfile.TemporaryDirectory(prefix='terminal-exit-') as folder:
                root = Path(folder).resolve()
                home, config, workspace = [root / x for x in ['home', 'config', 'workspace']]
                for path in [home, config, workspace]: path.mkdir()
                env = {k: v for k, v in os.environ.items() if not (k.endswith(('_API_KEY', '_AUTH_TOKEN', '_ACCESS_TOKEN')) or k.startswith(('ANTHROPIC_', 'CLAUDE_CODE_OAUTH', 'CLAUDE_CODE_USE_')))}
                endpoint = f'http://127.0.0.1:{server.server_port}'
                env.update(HOME=str(home), HEYCODE_HOME=str(home / '.heycode'), CLAUDE_CONFIG_DIR=str(config), TERM='xterm-256color', COLORTERM='truecolor', CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC='1', CLAUDE_CODE_REMOTE_CONTROL='0', ANTHROPIC_BASE_URL=endpoint, ANTHROPIC_API_KEY='local-only-dummy', HTTP_PROXY=endpoint, HTTPS_PROXY=endpoint, NO_PROXY='localhost,127.0.0.1')
                env.pop('NO_COLOR', None)
                (config / '.claude.json').write_text(json.dumps({'hasCompletedOnboarding': True, 'theme': 'dark', 'lastOnboardingVersion': '2.1.268'}))
                executable = binary if engine == 'heycode' else Path(shutil.which('claude')).resolve()
                args = [str(executable), '--fake', '--no-background', '--trust-workspace'] if engine == 'heycode' else [str(executable), '--safe-mode', '--restricted', '--strict-mcp-config', '--no-chrome', '--permission-mode', 'manual', '--setting-sources', 'project,local', '--settings', '{"remoteControlAtStartup":false}', '--tools', '', '--session-id', str(uuid.uuid4())]
                try:
                    terminal = Terminal(args, workspace, env, output / engine)
                    terminal.stream = TerminalByteStream(terminal.screen)
                    for attempt in range(15):
                        terminal.read(1)
                        visible = terminal.visible()
                        if 'custom API key' in visible: os.write(terminal.master, b'\x1b[A\r')
                        elif 'trust this folder' in visible.lower(): os.write(terminal.master, b'\x1b[B\r')
                        elif any(x in visible.lower() for x in ['for shortcuts', 'shift+tab', 'try ', 'context']): break
                    else: raise RuntimeError('No idle composer')
                    terminal.capture('01-idle')
                    os.write(terminal.master, b'/exit')
                    terminal.read(0.8)
                    menu = terminal.capture('02-exit-draft')
                    assert terminal.process.poll() is None, 'Typing exited early'
                    if engine == 'claude':
                        assert re.search(r'/exit\s+\S', menu), f'No exact local exit command: {menu}'
                        assert 'No commands match' not in menu
                    os.write(terminal.master, b'\r')
                    deadline = time.monotonic() + 20
                    while terminal.process.poll() is None and time.monotonic() < deadline: terminal.read(0.2)
                    code = terminal.process.poll()
                    terminal.capture('03-exited')
                    assert code == 0, f'{engine} did not exit cleanly: {code}'
                    results[engine] = {'binary_sha256': hashlib.sha256(executable.read_bytes()).hexdigest(), 'typed_draft_kept_process_alive': True, 'exit_code': code, 'external_termination_used': False, 'captures': terminal.captures}
                    journal_root = home / '.heycode' if engine == 'heycode' else config
                    unexpected = []
                    journal_event_kinds = []
                    retained_journal = []
                    for journal in journal_root.rglob('*.jsonl'):
                        for raw in journal.read_text(errors='replace').splitlines():
                            retained_journal.append(raw)
                            try: item = json.loads(raw)
                            except ValueError: continue
                            kind = item.get('kind')
                            if isinstance(kind, str):
                                journal_event_kinds.append(kind)
                            if engine == 'heycode' and kind in {
                                'user/message', 'turn/start', 'turn/end',
                                'request/header', 'request/context',
                                'assistant/chunk', 'assistant/message',
                                'assistant/provider-item', 'assistant/response-metadata',
                            }:
                                unexpected.append(item)
                    assert not unexpected, 'Unexpected model-input events'
                    results[engine]['journal_event_kinds'] = journal_event_kinds
                    if retained_journal:
                        (output / engine / 'session.jsonl').write_text(
                            '\n'.join(retained_journal) + '\n'
                        )
                finally:
                    if terminal: terminal.close()
        assert not any(r['method'] == 'POST' and '/messages' in r['path'] for r in requests)
        result = {'status': 'passed', 'scope': 'idle /exit local command lifecycle', 'engines': results, 'network_attempts': requests, 'inference_requests': 0, 'commercial_requests': 0}
        (output / 'result.json').write_text(json.dumps(result, indent=2))
        return result
    finally:
        server.shutdown()
        server.server_close()

if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(run(args.binary.resolve(), args.output.resolve()), indent=2))
