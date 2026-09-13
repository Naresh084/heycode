#!/usr/bin/env python3
"""Stress the default session-broker terminal with mouse reports and resize bursts.

Uses an isolated fake runtime. Captures actual terminal cells and verifies orderly
terminal restoration; never connects to a paid provider or touches a user session.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import signal
import select
import tempfile
import time
import termios

import pyte
from tui_blackbox import FullScreenTui
from terminal_screenshot import TerminalByteStream, render_screen


def run(binary, output, nonblocking_output=False, blocked_child=False, stalled_output=False):
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='heycode-transport-') as temporary:
        root = Path(temporary)
        home, workspace = root / 'home', root / 'work'
        home.mkdir(); workspace.mkdir()
        tui = FullScreenTui(str(home), str(workspace), str(binary), background=True, nonblocking_output=nonblocking_output)
        checks = {}
        stopped_child = None
        owned_processes = {}
        stalled_observation = None
        try:
            deadline = time.monotonic() + 25
            while b'\x1b[?2004h' not in tui.transcript and tui.alive() and time.monotonic() < deadline:
                tui.read(.2)
            checks['default_broker_ready'] = b'\x1b[?2004h' in tui.transcript
            if not checks['default_broker_ready']:
                raise RuntimeError('default broker did not enter terminal')
            # Exercise normal command output, then deliver wheel bursts while the
            # consumer pauses. This is the user's SGR wheel-down report family.
            tui.send(b'/help\r', .5)
            for index in range(40):
                os.write(tui.fd, b'\x1b[<65;64;32M' * 64)
                if index % 5 == 0:
                    tui.resize(24 if index % 10 == 0 else 40, 60 if index % 10 == 0 else 110)
                else:
                    tui.read(.025)
            tui.resize(40, 110)
            tui.send(b'\x1b', .2)
            tui.send(b'\x15', .2)
            tui.send(b'/help\r', .5)
            screen = pyte.Screen(110, 40)
            TerminalByteStream(screen).feed(tui.transcript)
            render_screen(screen, output / 'after-scroll.png')
            (output / 'after-scroll.txt').write_text('\n'.join(screen.display))
            checks['alive_after_scroll_and_resize'] = tui.alive()
            checks['no_resource_unavailable'] = b'Resource temporarily unavailable' not in tui.transcript
            if stalled_output:
                tty_name = subprocess.check_output(['ps', '-p', str(tui.pid), '-o', 'tty='], text=True).strip()
                if not tty_name.startswith('ttys'):
                    raise RuntimeError(f'unexpected owned terminal: {tty_name}')
                terminal_fd = os.open('/dev/' + tty_name, os.O_WRONLY | os.O_NONBLOCK)
                try:
                    filled = 0
                    while filled < 4 * 1024 * 1024:
                        try:
                            filled += os.write(terminal_fd, b'x' * 8192)
                        except BlockingIOError:
                            break
                    checks['terminal_output_backpressure_reached'] = filled < 4 * 1024 * 1024
                    os.write(tui.fd, b'\x1d')
                    deadline = time.monotonic() + 4
                    while tui.alive() and time.monotonic() < deadline:
                        time.sleep(.05)
                    reaped = not tui.alive()
                    stalled_observation = subprocess.run(['ps', '-p', str(tui.pid), '-o', 'state=,wchan=,comm='], capture_output=True, text=True).stdout.strip()
                    # macOS can defer reaping a terminal process until its PTY
                    # consumer drains. ps(1) E means it is already trying to exit.
                    state_flags = stalled_observation.split()[0] if stalled_observation else ''
                    checks['exit_started_without_terminal_consumer'] = reaped or 'E' in state_flags
                    attributes = termios.tcgetattr(terminal_fd)
                    checks['canonical_input_restored'] = bool(attributes[3] & termios.ICANON)
                    checks['echo_restored'] = bool(attributes[3] & termios.ECHO)
                finally:
                    os.close(terminal_fd)
                    tui.read(.5)
            elif blocked_child:
                # Identify only descendants of this newly created attachment.
                pairs = [tuple(map(int, line.split())) for line in subprocess.check_output(['ps', '-axo', 'pid=,ppid='], text=True).splitlines() if line.strip()]
                brokers = [pid for pid, parent in pairs if parent == tui.pid]
                children = [pid for pid, parent in pairs if parent in brokers]
                if len(children) != 1:
                    raise RuntimeError(f'cannot uniquely identify owned broker child: {children}')
                owned_processes = {pid: subprocess.check_output(['ps', '-p', str(pid), '-o', 'command='], text=True).strip() for pid in children + brokers}
                stopped_child = children[0]
                os.kill(stopped_child, signal.SIGSTOP)
                os.set_blocking(tui.fd, False)
                burst = bytearray(b'\x1b[<65;64;32M' * 2000 + b'\x1d')
                deadline = time.monotonic() + 5
                while burst and time.monotonic() < deadline:
                    _, writable, _ = select.select([], [tui.fd], [], .1)
                    if writable:
                        try:
                            sent = os.write(tui.fd, burst[:1024])
                            del burst[:sent]
                        except BlockingIOError:
                            pass
                tui.read(2)
                checks['detach_while_child_not_reading'] = not tui.alive() and b'Session detached' in tui.transcript
                checks['all_stress_input_delivered_to_attachment'] = not burst
                os.kill(stopped_child, signal.SIGCONT)
                stopped_child = None
            else:
                tui.send(b'\x1b', .2)
                tui.send(b'\x15/quit\r', 2)
                for _ in range(3):
                    if not tui.alive(): break
                    tui.send(b'\x03', .5)
            checks['orderly_exit'] = not tui.alive() and tui.exit_status == 0
            checks['no_resource_unavailable'] = b'Resource temporarily unavailable' not in tui.transcript
            checks['no_mouse_reports_echoed'] = b'^[[<65;' not in tui.transcript
            tail = tui.transcript[-4096:]
            if not stalled_output:
                checks['mouse_reporting_disabled'] = b'\x1b[?1006l' in tail
                checks['keyboard_protocol_restored'] = b'\x1b[<u' in tail
        finally:
            if stopped_child is not None:
                os.kill(stopped_child, signal.SIGCONT)
                tui.read(.5)
            cleanup = []
            for record in (home / 'background').glob('*.json'):
                state = json.loads(record.read_text())
                if state.get('exit_code') is None:
                    result = subprocess.run([str(binary), 'sessions', 'stop', state['host_id']], env={**os.environ, 'HEYCODE_HOME': str(home)}, capture_output=True, timeout=15)
                    cleanup.append({'host_id': state['host_id'], 'returncode': result.returncode})
                    if result.returncode:
                        for pid, expected in owned_processes.items():
                            observed = subprocess.run(['ps', '-p', str(pid), '-o', 'command='], capture_output=True, text=True).stdout.strip()
                            if observed == expected:
                                os.kill(pid, signal.SIGTERM)
                                cleanup.append({'verified_owned_pid': pid, 'signal': 'SIGTERM'})
            tui.close()
            (output / 'terminal.ansi').write_bytes(tui.transcript)
            receipt = {'binary': str(binary), 'sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'exit_code': tui.exit_status, 'stalled_process_observation': stalled_observation, 'checks': checks, 'cleanup': cleanup}
            (output / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    return receipt


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--nonblocking-output', action='store_true')
    parser.add_argument('--blocked-child', action='store_true')
    parser.add_argument('--stalled-output', action='store_true')
    args = parser.parse_args()
    receipt = run(args.binary.resolve(), args.output.resolve(), args.nonblocking_output, args.blocked_child, args.stalled_output)
    print(json.dumps(receipt, indent=2))
    raise SystemExit(0 if receipt['checks'] and all(receipt['checks'].values()) else 1)
