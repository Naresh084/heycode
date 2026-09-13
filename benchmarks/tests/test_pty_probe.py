from __future__ import annotations

import os
import sys
import tempfile
import time
import unittest
from pathlib import Path

from benchmarks.pty_probe import run_first_frame_probe
from quality.process import isolated_environment


@unittest.skipIf(os.name == "nt", "PTY lifecycle proof is POSIX-specific")
class PtyProbeTests(unittest.TestCase):
    def test_first_frame_probe_reaps_a_descendant_before_returning(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            workspace = root / "workspace"
            release = root / "release"
            marker = root / "survived"
            child = (
                "import pathlib,time;"
                f"release=pathlib.Path({str(release)!r});"
                f"marker=pathlib.Path({str(marker)!r});"
                "\nwhile not release.exists(): time.sleep(0.02)\n"
                "marker.write_text('survived')"
            )
            parent = (
                "import subprocess,sys,time;"
                f"subprocess.Popen([sys.executable,'-c',{child!r}]);"
                "print('frame',flush=True);time.sleep(30)"
            )
            environment = isolated_environment(root, root / "home", workspace, 17)
            result = run_first_frame_probe(
                [sys.executable, "-c", parent],
                cwd=workspace,
                env=environment,
                timeout_s=5.0,
            )
            self.assertIsNotNone(result.first_output_ms)
            self.assertNotEqual(result.settlement, "cleanup_failed")
            release.write_text("go", encoding="utf-8")
            time.sleep(0.4)
            self.assertFalse(marker.exists())


if __name__ == "__main__":
    unittest.main()
