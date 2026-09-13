#!/usr/bin/env python3
"""Capture the actual HeyCode process in a dedicated tmux server.

Uses a disposable demo home by default. ANSI is retained beside each PNG;
images render tmux's observed cells, never fabricated conversation content.
Requires tmux, pyte and Pillow. Does not capture other user terminal sessions.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import pyte
from PIL import Image
from terminal_screenshot import TerminalByteStream, render_screen


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--live-home', type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    args.output.mkdir(parents=True, exist_ok=True)
    socket = 'heycode-capture-' + str(os.getpid())
    def tmux(*arguments):
        return subprocess.check_output(['tmux', '-L', socket, '-f', '/dev/null', *arguments])
    with tempfile.TemporaryDirectory(prefix='heycode-demo-') as temporary:
        root = Path(temporary)
        workspace = root / 'hello-heycode'
        home = args.live_home.resolve() if args.live_home else root / 'home'
        workspace.mkdir()
        home.mkdir(exist_ok=True)
        (workspace / 'README.md').write_text('# Hello HeyCode\nA tiny example project for the terminal demo.\n')
        mode = [] if args.live_home else ['--fake']
        command = ['env', '-u', 'NO_COLOR', 'TERM=xterm-256color', f'HEYCODE_HOME={home}', 'HEYCODE_AUTO_UPDATE=0', 'COLORTERM=truecolor', str(binary), '--no-background', '--trust-workspace', *mode]
        try:
            tmux('new-session', '-d', '-s', 'demo', '-x', '104', '-y', '28', '-c', str(workspace), *command)
            tmux('set-option', '-g', 'status', 'off')
            time.sleep(12)
            def capture(name):
                raw = tmux('capture-pane', '-p', '-e', '-t', 'demo')
                (args.output / f'{name}.ansi').write_bytes(raw)
                screen = pyte.Screen(104, 28)
                TerminalByteStream(screen).feed(raw.replace(b'\n', b'\r\n'))
                (args.output / f'{name}.txt').write_text('\n'.join(screen.display))
                render_screen(screen, args.output / f'{name}.png', font_size=18)
                return '\n'.join(screen.display)
            start = capture('terminal-home')
            if 'HeyCode' not in start:
                raise RuntimeError('HeyCode header not observed')
            # Forward actual SGR mouse events to the product, then sample live frames.
            tmux('send-keys', '-t', 'demo', '-H', '1b', '5b', '3c', '30', '3b', '35', '3b', '33', '4d')
            tmux('send-keys', '-t', 'demo', '-H', '1b', '5b', '3c', '30', '3b', '35', '3b', '33', '6d')
            animation = []
            for index in range(20):
                capture(f'mascot-{index:02d}')
                animation.append(Image.open(args.output / f'mascot-{index:02d}.png').copy())
                time.sleep(0.1)
            animation[0].save(args.output / 'terminal-mascot.gif', save_all=True,
                              append_images=animation[1:], duration=100, loop=0, optimize=True)
            recording_done = threading.Event()
            recording_paths = []
            def record_message():
                for index in range(150):
                    if recording_done.is_set(): break
                    name = f'message-{index:03d}'
                    capture(name)
                    recording_paths.append(args.output / f'{name}.png')
                    recording_done.wait(0.4)
            recording = threading.Thread(target=record_message)
            recording.start()
            prompt = 'Hello! Read README.md and explain this tiny project in two sentences. Do not change any files.'
            for offset in range(0, len(prompt), 6):
                tmux('send-keys', '-t', 'demo', '-l', prompt[offset:offset + 6])
                time.sleep(0.12)
            tmux('send-keys', '-t', 'demo', 'Enter')
            time.sleep(15 if args.live_home else 3)
            conversation = capture('terminal-approval')
            if 'Read file' in conversation and 'Read(README.md)' in conversation and 'Do you want to proceed?' in conversation:
                tmux('send-keys', '-t', 'demo', 'Enter')
                time.sleep(12)
            capture('terminal-conversation')
            time.sleep(1)
            recording_done.set()
            recording.join()
            message_frames = [Image.open(path).copy() for path in recording_paths]
            if message_frames:
                message_frames[0].save(args.output / 'terminal-conversation.gif', save_all=True,
                                      append_images=message_frames[1:], duration=200, loop=0, optimize=True)
            tmux('send-keys', '-t', 'demo', '-l', '/provider')
            tmux('send-keys', '-t', 'demo', 'Enter')
            time.sleep(2)
            capture('terminal-providers')
            (args.output / 'capture.json').write_text(json.dumps({
                'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
                'viewport': [104, 28], 'capture': 'tmux capture-pane ANSI rendered with pyte',
                'message_recording_playback': 'approximately 2x speed; actual terminal frames',
                'provider': 'configured live provider' if args.live_home else 'deterministic fake provider',
                'prompt': 'Hello! Read README.md and explain this tiny project in two sentences. Do not change any files.',
            }, indent=2) + '\n')
        finally:
            subprocess.run(['tmux', '-L', socket, 'kill-server'], capture_output=True)

if __name__ == '__main__':
    main()
