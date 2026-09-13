#!/usr/bin/env python3
"""Actual CLI same-session renderer switch and plugin reload, with no inference."""
from __future__ import annotations
import argparse
import json
import os
from pathlib import Path
import tempfile
import time
import pyte
from tui_blackbox import FullScreenTui
from terminal_screenshot import render_screen

class Screen(pyte.Screen):
    def set_mode(self,*modes,**kwargs):
        if kwargs.get('private') and 1049 in modes: self.reset()
        return super().set_mode(*modes,**kwargs)

def run(binary:Path, output:Path, failure:bool=False):
    output.mkdir(parents=True,exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='heycode-recomposition-') as folder:
        root=Path(folder);home=root/'home';work=root/'work';home.mkdir();work.mkdir()
        tui=FullScreenTui(str(home),str(work),str(binary.resolve()),fake=True,color=True,rows=42,columns=110)
        screen=Screen(110,42); stream=pyte.ByteStream(screen)
        def read(): stream.feed(tui.read(.15))
        def text(): return '\n'.join(screen.display)
        def wait(needle,timeout=45):
            deadline=time.monotonic()+timeout
            while time.monotonic()<deadline:
                read()
                if needle in text(): return
                if not tui.alive(): raise AssertionError(f'CLI exited while waiting for {needle}: {text()}')
            raise AssertionError(f'Timed out waiting for {needle}: {text()}')
        def send(value): os.write(tui.fd,value);read()
        def capture(name):
            read();(output/f'{name}.txt').write_text(text());render_screen(screen,output/f'{name}.png')
        def records():
            paths=list(home.rglob('session.jsonl'))
            assert len(paths)==1, paths
            return paths[0], [json.loads(line) for line in paths[0].read_text().splitlines() if line.strip()]
        try:
            wait('shift+tab to cycle');capture('00-start')
            before, original=records()
            # The first switch happens before a user turn or title. This catches
            # accidental deletion of an unused session during recomposition.
            offset=len(tui.transcript)
            send(b'/tui screen-reader\r')
            deadline=time.monotonic()+45
            while time.monotonic()<deadline:
                read()
                latest=tui.transcript[offset:]
                if b'\x1b[?1049l' in latest and b'input: empty' in latest and b'keys: Enter sends' in latest: break
            else: raise AssertionError('Did not leave alternate screen for screen-reader mode')
            # The flat projection is event-driven; wait for its startup before
            # sending more input, then obtain a unique local command receipt.
            read()
            send(b'/rename Renderer boundary preserved\r');wait('Renamed to Renderer boundary preserved');capture('01-flat-renamed')
            after, events=records();assert after==before
            assert events[:len(original)]==original
            offset=len(tui.transcript)
            send(b'/tui auto\r')
            deadline=time.monotonic()+45
            while time.monotonic()<deadline:
                read()
                if b'\x1b[?1049h' in tui.transcript[offset:] and 'shift+tab to cycle' in text():break
            else: raise AssertionError('Did not reenter automatic alternate-screen presentation')
            capture('02-auto-resumed')
            assert records()[0]==before
            offset=len(tui.transcript)
            send(b'/reload-plugins\r')
            deadline=time.monotonic()+45
            while time.monotonic()<deadline:
                read()
                latest=tui.transcript[offset:]
                if b'\x1b[?1049l' in latest and b'\x1b[?1049h' in latest and 'shift+tab to cycle' in text():break
            else: raise AssertionError('Plugin reload did not complete a terminal teardown/reentry')
            send(b'/release-notes\r');wait('Unreleased');capture('03-reloaded-release-notes')
            after,events=records();assert after==before
            assert not any(e.get('kind') in ('request/header','user/message') for e in events), events
            assert any(e.get('kind')=='session/title' and e.get('data',{}).get('title')=='Renderer boundary preserved' for e in events),events
            if failure:
                # A malformed durable configuration exercises activation failure
                # after shutdown, without modifying the retained conversation.
                (home/'config.toml').write_text('this is not valid TOML = [')
                offset=len(tui.transcript)
                send(b'/reload-plugins\r')
                deadline=time.monotonic()+45
                while time.monotonic()<deadline:
                    read()
                    if not tui.alive(): break
                else: raise AssertionError('Failed reload did not exit with its activation error')
                raw=tui.transcript[offset:].decode('utf-8','replace')
                assert 'Plugin reload failed; session' in raw and 'remains saved' in raw, raw
                assert raw.count('Plugin reload failed; session')==1, raw
                assert before.parent.name in raw, raw
                after,retained=records()
                assert after==before and retained[:len(events)]==events
                assert not any(e.get('kind') in ('request/header','user/message') for e in retained)
                events=retained
                capture('04-failed-reload-saved-session')
            (output/'events.json').write_text(json.dumps(events,indent=2))
            (output/'result.json').write_text(json.dumps({'status':'passed','session_id':before.parent.name,'model_requests':0,'switches':2,'plugin_reloads':1,'unused_session_preserved':True,'failed_reload_preserved_session':failure},indent=2))
        finally:
            (output/'terminal.ansi').write_bytes(tui.transcript)
            tui.close()
    print(f'PASS: {output}')

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,default=Path('target/debug/heycode'));p.add_argument('--output',type=Path,required=True);p.add_argument('--failure',action='store_true');a=p.parse_args();run(a.binary,a.output,a.failure)
