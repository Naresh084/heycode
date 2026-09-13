#!/usr/bin/env python3
"""Bounded native-CLI cache experiment. Requires --execute to spend provider credits.

The baseline relay removes only explicit message cache markers. Both arms use
identical native binaries, tools, fixtures and model. This isolates cache policy,
not a comparison against an older application. No authorization headers are saved.
"""
import argparse
import copy
import hashlib
import http.server
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request


def unmarked(body):
    result = copy.deepcopy(body)
    for message in result.get('messages', []):
        message.pop('cache_control', None)
        content = message.get('content')
        if isinstance(content, list):
            for block in content:
                if isinstance(block, dict):
                    block.pop('cache_control', None)
    return result


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def report(records, outcomes):
    rows = []
    for arm in ['baseline', 'cached']:
        requests = [r for r in records if r['arm'] == arm]
        usage = [r.get('usage', {}) for r in requests]
        costs = [u.get('cost') for u in usage]
        known_cost = sum(c for c in costs if isinstance(c, (float, int)))
        complete_cost = bool(costs) and all(isinstance(c, (float, int)) for c in costs)
        checks = [r for r in outcomes if r['arm'] == arm]
        successes = sum(r['correct'] for r in checks)
        warm = [u for r, u in zip(requests, usage) if r['scenario'].startswith('warm-')]
        warm_input = sum(u.get('prompt_tokens', 0) for u in warm)
        warm_read = sum(u.get('prompt_tokens_details', {}).get('cached_tokens', 0) for u in warm)
        rows.append({'arm': arm, 'provider_requests_including_children': len(requests),
                     'upstream_providers': sorted({r['upstream_provider'] for r in requests if r.get('upstream_provider')}),
                     'tasks_correct': successes, 'tasks_attempted': len(checks),
                     'reported_cost_usd': known_cost if complete_cost else None,
                     'known_cost_lower_bound_usd': known_cost,
                     'cost_per_correct_task_usd': known_cost / successes if complete_cost and successes else None,
                     'warm_input_tokens': warm_input, 'warm_cache_read_tokens': warm_read,
                     'warm_cached_input_share': warm_read / warm_input if warm_input else None,
                     'warm_reusable_prefix_efficiency': None,
                     'request_hit_frequency': sum(u.get('prompt_tokens_details', {}).get('cached_tokens', 0) > 0 for u in usage) / len(usage) if usage else None,
                     'request_errors': sum('error' in r or r.get('status', 200) >= 400 for r in requests)})
    return {'arms': rows, 'limitations': [
        'A small paired cache-policy experiment, not evidence of general task-quality superiority.',
        'Warm cached-input share uses all billed input as its denominator; reusable-prefix token coverage is unknown.',
        'Model outputs and provider routing can vary between arms. No latency or universal 98 percent guarantee.',
        'Baseline removes cache markers in the relay; native durable headers still describe the configured cache policy.',
        'Missing provider-reported costs remain unknown. All relayed child requests count toward arm cost.']}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--execute', action='store_true')
    parser.add_argument('--binary', type=Path, default=Path('target/debug/heycode'))
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--scenarios', nargs='+', help='Optional subset for a diagnostic run; reported separately')
    parser.add_argument('--model', default='anthropic/claude-sonnet-4')
    parser.add_argument('--max-requests', type=int, default=60)
    parser.add_argument('--max-cost-usd', type=float, default=3.0)
    args = parser.parse_args()
    if not args.execute:
        parser.error('--execute is required; this benchmark makes paid provider requests')
    if not 1 <= args.max_requests <= 80 or not 0 < args.max_cost_usd <= 10:
        parser.error('request cap must be 1..80 and cost cap must be >0..10 USD')
    binary = args.binary.resolve(strict=True)
    output = args.output.resolve(); output.mkdir(parents=True, exist_ok=False)
    records, outcomes = [], []
    lock = threading.Lock()
    state = {'arm': '', 'scenario': ''}
    def persist():
        (output / 'requests.json').write_text(json.dumps(records, indent=2))
        (output / 'outcomes.json').write_text(json.dumps(outcomes, indent=2))
        (output / 'summary.json').write_text(json.dumps(report(records, outcomes), indent=2))
    class Relay(http.server.BaseHTTPRequestHandler):
        def log_message(self, *unused):
            pass
        def do_GET(self):
            self.forward(False)
        def do_POST(self):
            self.forward(True)
        def forward(self, post):
            record = None
            payload = None
            if post:
                body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                with lock:
                    cost = sum(r.get('usage', {}).get('cost') or 0 for r in records)
                    if len(records) >= args.max_requests or cost >= args.max_cost_usd:
                        self.send_error(429, 'Benchmark request or observed-cost cap reached')
                        return
                    wire = unmarked(body) if state['arm'] == 'baseline' else body
                    record = {'arm': state['arm'], 'scenario': state['scenario'],
                              'index': len(records), 'request_sha256': digest(body),
                              'unmarked_sha256': digest(unmarked(body)), 'wire': wire,
                              'started_at': time.time()}
                    records.append(record)
                    persist()
                payload = json.dumps(wire).encode()
            headers = {'Content-Type': 'application/json'}
            if self.headers.get('Authorization'):
                headers['Authorization'] = self.headers['Authorization']
            request = urllib.request.Request('https://openrouter.ai' + self.path,
                                             data=payload, headers=headers,
                                             method='POST' if post else 'GET')
            try:
                upstream = urllib.request.urlopen(request, timeout=100)
                with upstream:
                    self.send_response(upstream.status)
                    self.send_header('Content-Type', upstream.headers.get('Content-Type', 'application/json'))
                    self.end_headers()
                    if record is not None:
                        record['status'] = upstream.status
                    for line in upstream:
                        if record is not None and line.startswith(b'data: '):
                            try:
                                event = json.loads(line[6:])
                                record.setdefault('events', []).append(event)
                                if event.get('usage'):
                                    record['usage'] = event['usage']
                                if event.get('provider'):
                                    record['upstream_provider'] = event['provider']
                                if event.get('id'):
                                    record['generation_id'] = event['id']
                                if event.get('error'):
                                    record['error'] = event['error']
                            except (ValueError, TypeError):
                                pass
                        self.wfile.write(line); self.wfile.flush()
            except (urllib.error.URLError, OSError) as error:
                if record is not None:
                    # Record the error class only; network exception strings may contain headers.
                    record['error'] = type(error).__name__
                try:
                    self.send_error(502, 'Benchmark relay upstream failed')
                except OSError:
                    pass
            finally:
                if record is not None:
                    record['seconds'] = time.time() - record['started_at']
                    with lock:
                        persist()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Relay)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='heycode-cache-quality-') as temporary:
            root = Path(temporary); work = root / 'work'; work.mkdir()
            values = [173, 281, 419, 557, 683]
            for index, value in enumerate(values, 1):
                (work / f'fixture-{index}.txt').write_text(f'VERIFIED_VALUE={value}\n')
            (work / 'long-records.txt').write_text(''.join(f'record-{i:04d}: value={i * 13 + 7} status=retained\n' for i in range(1, 501)))
            scenarios = [('cold', 'Reply exactly COLD-OK. Do not call tools.', ['COLD-OK'])]
            scenarios += [(f'warm-{i}', f'Reply exactly WARM-{i}-OK. Do not call tools.', [f'WARM-{i}-OK']) for i in range(1, 4)]
            scenarios += [('parallel-reads', 'Make exactly two separate read tool calls in a single assistant message: one for fixture-2.txt and one for fixture-3.txt, before receiving either result. Do not use read_many, grep, Bash, or agents for this specific test. Return both exact VERIFIED_VALUE values in file order.', ['281', '419']),
                ('one-read', 'Use read to inspect fixture-1.txt and report the exact VERIFIED_VALUE.', ['173']),
                ('long-result', 'Inspect long-records.txt using file tools. Find record-0500 and report its exact value; use paging if needed. Do not infer or calculate it from earlier records.', ['6507']),
                ('five-agents', 'Launch five agents, one for each fixture-1.txt through fixture-5.txt. Each must read its file and return its VERIFIED_VALUE. Use background=false for this dependent check, wait for all five, then return all five values in file order. Do not guess values or run shells.', [str(v) for v in values])]
            if args.scenarios:
                scenarios = [scenario for scenario in scenarios if scenario[0] in args.scenarios]
            for arm in ['baseline', 'cached']:
                home = root / arm; home.mkdir()
                shutil.copyfile(Path.home() / '.heycode/credentials.toml', home / 'credentials.toml')
                os.chmod(home / 'credentials.toml', 0o600)
                base = f'http://127.0.0.1:{server.server_port}/api/v1'
                (home / 'config.toml').write_text(f'schema_version=31\n[llm]\nprovider="openrouter"\nmodel="{args.model}"\nbase_url="{base}"\n')
                parent_log = None
                for scenario, prompt, expected in scenarios:
                    state.update(arm=arm, scenario=scenario)
                    command = [str(binary), 'run', '--provider', 'openrouter', '--model', args.model,
                               '--set', f'llm.base_url={base}', '--max-output-tokens', '1024',
                               '--sandbox', 'readonly', '--restricted-workspace', '--output-format', 'json']
                    if parent_log:
                        command += ['--resume', str(parent_log)]
                    command += [prompt]
                    started = time.monotonic()
                    try:
                        result = subprocess.run(command, cwd=work, env=dict(os.environ, HEYCODE_HOME=str(home)),
                                                capture_output=True, text=True, timeout=240)
                        try:
                            response = json.loads(result.stdout)
                        except ValueError:
                            response = {'invalid_json': True}
                        reply = response.get('reply', '')
                        correct = result.returncode == 0 and all(re.search(r'(?<![0-9])' + re.escape(value) + r'(?![0-9])', reply) for value in expected) and not response.get('errors')
                        if scenario == 'cold' or scenario.startswith('warm-'):
                            correct = correct and reply.strip() == expected[0]
                        if parent_log is None:
                            logs = list(home.rglob('session.jsonl'))
                            if len(logs) == 1: parent_log = logs[0]
                        if parent_log is not None and scenario in ('one-read', 'long-result', 'parallel-reads', 'five-agents'):
                            events = [json.loads(line) for line in parent_log.read_text().splitlines()]
                            latest_turn = max(e['data'].get('turn', 0) for e in events)
                            calls = [call for e in events if e['kind'] == 'assistant/message' and e['data'].get('turn') == latest_turn
                                     for call in e['data'].get('tool_calls') or []]
                            if scenario == 'parallel-reads':
                                correct = correct and any(sum(call.get('name') == 'read' for call in event['data'].get('tool_calls') or []) >= 2
                                    for event in events if event['kind'] == 'assistant/message' and event['data'].get('turn') == latest_turn)
                            elif scenario in ('one-read', 'long-result'):
                                allowed = ('read',) if scenario == 'one-read' else ('read', 'read_many', 'grep')
                                correct = correct and any(call.get('name') in allowed for call in calls)
                            else:
                                children = [log for log in home.rglob('session.jsonl') if log != parent_log]
                                actual_reads = 0
                                for child in children:
                                    child_events = [json.loads(line) for line in child.read_text().splitlines()]
                                    actual_reads += any(call.get('name') == 'read' for event in child_events
                                        if event['kind'] == 'assistant/message' for call in event['data'].get('tool_calls') or [])
                                correct = correct and sum(call.get('name') in ('agent', 'task') for call in calls) == 5 and actual_reads == 5
                        outcomes.append({'arm': arm, 'scenario': scenario, 'exit': result.returncode,
                                         'seconds': time.monotonic() - started, 'correct': bool(correct), 'response': response, 'stderr': result.stderr})
                    except subprocess.TimeoutExpired:
                        outcomes.append({'arm': arm, 'scenario': scenario, 'correct': False, 'error': 'timeout'})
                        persist(); break
                    logs = list(home.rglob('session.jsonl'))
                    if parent_log is None and len(logs) == 1:
                        parent_log = logs[0]
                    persist()
                    print(json.dumps({'arm': arm, 'scenario': scenario, 'correct': bool(correct)}), flush=True)
                    if result.returncode:
                        break
                destination = output / f'{arm}-sessions'; destination.mkdir()
                for index, log in enumerate(home.rglob('session.jsonl')):
                    shutil.copyfile(log, destination / f'{log.parent.name}-session.jsonl')
                audit = []
                for log in destination.glob('*-session.jsonl'):
                    events = [json.loads(line) for line in log.read_text().splitlines()]
                    headers = {e['data']['request_id']: e['data'] for e in events if e['kind'] == 'request/header'}
                    contexts = {e['data']['request_id']: e['data']['context'] for e in events if e['kind'] == 'request/context'}
                    responses = {e['data']['request_id']: e['data']['metadata'] for e in events if e['kind'] == 'assistant/response-metadata'}
                    for identity, entry in headers.items():
                        header = entry['header']
                        audit.append({'session_file': log.name, 'request_id': identity, 'turn': entry['turn'], 'step': entry['step'],
                                      'provider': header['provider'], 'model': header['model'],
                                      'configuration': header.get('configuration'), 'tool_schema_bytes': len(json.dumps(header.get('tools', []), separators=(',', ':')).encode()),
                                      'context': contexts.get(identity), 'response_metadata': responses.get(identity)})
                (output / f'{arm}-context-audit.json').write_text(json.dumps(audit, indent=2))
    finally:
        server.shutdown(); server.server_close(); persist()
    print(json.dumps(report(records, outcomes), indent=2))


if __name__ == '__main__':
    main()
