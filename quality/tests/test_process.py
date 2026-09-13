from __future__ import annotations

import os
import sys
import tempfile
import time
import unittest
from pathlib import Path

from quality.process import isolated_environment, run_discarded


class ProcessTests(unittest.TestCase):
    def test_isolated_environment_drops_ambient_credentials(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            old = os.environ.get("Q10_TEST_SECRET_TOKEN")
            os.environ["Q10_TEST_SECRET_TOKEN"] = "must-not-cross"
            try:
                env = isolated_environment(
                    root=Path(directory),
                    heycode_home=Path(directory) / "heycode-home",
                    workspace=Path(directory) / "workspace",
                    seed=11,
                )
            finally:
                if old is None:
                    os.environ.pop("Q10_TEST_SECRET_TOKEN", None)
                else:
                    os.environ["Q10_TEST_SECRET_TOKEN"] = old
            self.assertNotIn("Q10_TEST_SECRET_TOKEN", env)
            self.assertEqual(env["HEYCODE_EVAL_SEED"], "11")
            self.assertNotEqual(env["HOME"], os.environ.get("HOME"))

    def test_process_output_is_counted_but_not_retained(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            env = isolated_environment(root, root / "home", root / "workspace", 1)
            result = run_discarded(
                [sys.executable, "-c", "import sys;sys.stdout.write('abc');sys.stderr.write('de')"],
                cwd=root,
                env=env,
                timeout_s=5.0,
            )
            self.assertEqual(result.returncode, 0)
            self.assertEqual(result.stdout_bytes, 3)
            self.assertEqual(result.stderr_bytes, 2)
            self.assertIsNotNone(result.first_output_ms)
            self.assertFalse(hasattr(result, "stdout"))
            self.assertFalse(hasattr(result, "stderr"))

    @unittest.skipIf(os.name == "nt", "process-group descendant assertion is POSIX-specific")
    def test_timeout_reaps_descendants(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            marker = root / "descendant-survived"
            code = (
                "import subprocess,sys,time;"
                "subprocess.Popen([sys.executable,'-c',"
                f"\"import time,pathlib;time.sleep(0.8);pathlib.Path({str(marker)!r}).write_text('x')\"]);"
                "time.sleep(30)"
            )
            env = isolated_environment(root, root / "home", root / "workspace", 1)
            result = run_discarded(
                [sys.executable, "-c", code],
                cwd=root,
                env=env,
                timeout_s=0.2,
            )
            self.assertTrue(result.timed_out)
            time.sleep(1.0)
            self.assertFalse(marker.exists())


if __name__ == "__main__":
    unittest.main()
