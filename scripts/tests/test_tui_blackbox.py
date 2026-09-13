"""The full-screen black-box lane runs, and its checks can actually fail.

A scenario suite that cannot fail is decoration. These tests do not drive the
TUI themselves — that needs a built binary and several seconds per scenario —
they prove the harness around it: the escape stripper, the space-insensitive
matcher, and the failure accounting.
"""

import os
import subprocess
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

import tui_blackbox  # noqa: E402


REPOSITORY = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
BINARY = os.path.join(REPOSITORY, "target", "debug", "heycode")


class PlainTextTests(unittest.TestCase):
    def test_escapes_are_removed_and_text_is_kept(self):
        raw = b"\x1b[?1049h\x1b[2J\x1b[1;1HAsk anything\x1b[0m"
        self.assertEqual(tui_blackbox.plain(raw), "Ask anything")

    def test_matching_ignores_the_spacing_cursor_moves_leave_behind(self):
        # Painted through cursor positioning, the words arrive unspaced.
        self.assertTrue(tui_blackbox.shows("Askanything—/forcommands", "Ask anything"))
        self.assertTrue(tui_blackbox.shows("a b c", "abc"))
        self.assertFalse(tui_blackbox.shows("Ask anything", "FAKE-REPLY"))


class FailureAccountingTests(unittest.TestCase):
    def test_a_failed_check_is_reported_and_changes_the_exit_code(self):
        scenarios = tui_blackbox.Scenarios("/nonexistent/heycode")
        scenarios.check("first", True)
        self.assertEqual(scenarios.failures, [])
        scenarios.check("second", False, "explained")
        self.assertEqual(scenarios.failures, ["second: explained"])

    def test_a_missing_binary_is_its_own_exit_code(self):
        argv = sys.argv
        sys.argv = ["tui_blackbox.py", "--binary", "/nonexistent/heycode"]
        try:
            self.assertEqual(tui_blackbox.main(), 2)
        finally:
            sys.argv = argv


@unittest.skipUnless(os.access(BINARY, os.X_OK), "build target/debug/heycode first")
class LiveTerminalTests(unittest.TestCase):
    """The real thing, when a binary is available to drive."""

    def test_every_scenario_passes_against_the_built_binary(self):
        result = subprocess.run(
            [sys.executable, os.path.join(REPOSITORY, "scripts", "tui_blackbox.py")],
            capture_output=True,
            text=True,
            timeout=300,
            cwd=REPOSITORY,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
