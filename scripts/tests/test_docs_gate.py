"""Tests for the documentation lane's own freshness gate.

The docs lane is the one lane whose gate is a script rather than a Rust test,
so these tests exist to hold it to principle #14: the gate existing is not the
gate passing. A verification run must be able to FAIL on a stale generated
reference, and it must not repair the drift it is supposed to report.
"""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]
if str(SCRIPTS) not in sys.path:
    sys.path.insert(0, str(SCRIPTS))

import generate_doc_references  # noqa: E402
import verify_docs  # noqa: E402


REFERENCE = Path("docs/reference/commands.md")
GENERATED = "# generated from current sources\n"


class GeneratedReferenceGateTests(unittest.TestCase):
    """Freshness verification over a synthetic single-reference tree."""

    def test_test_modules_do_not_hide_later_production_commands(self) -> None:
        source = '''before();
#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod examples { fn fixture() { let marker = "}"; } }
fn production() { descriptor("help", "Show available commands"); }
#[cfg(test)]
mod more_examples { fn fixture() {} }
after();
'''
        production = generate_doc_references.without_test_modules(source)
        self.assertIn('descriptor("help", "Show available commands")', production)
        self.assertIn('before();', production)
        self.assertIn('after();', production)
        self.assertNotIn('fixture', production)

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        (self.root / REFERENCE.parent).mkdir(parents=True)
        self._saved_root = generate_doc_references.ROOT
        self._saved_render_all = generate_doc_references.render_all
        generate_doc_references.ROOT = self.root
        generate_doc_references.render_all = lambda: {REFERENCE: GENERATED}
        self.addCleanup(self._restore)

    def _restore(self) -> None:
        generate_doc_references.ROOT = self._saved_root
        generate_doc_references.render_all = self._saved_render_all
        self._tmp.cleanup()

    @property
    def _destination(self) -> Path:
        return self.root / REFERENCE

    def test_stale_reference_fails_and_is_left_untouched(self) -> None:
        self._destination.write_text("# hand-edited drift\n", encoding="utf-8")
        with self.assertRaises(verify_docs.VerificationError) as raised:
            verify_docs.verify_generated_references()
        self.assertIn("stale docs/reference/commands.md", str(raised.exception))
        self.assertEqual(
            self._destination.read_text(encoding="utf-8"),
            "# hand-edited drift\n",
            "verification must report drift, not silently rewrite it",
        )

    def test_missing_reference_fails(self) -> None:
        with self.assertRaises(verify_docs.VerificationError) as raised:
            verify_docs.verify_generated_references()
        self.assertIn("missing docs/reference/commands.md", str(raised.exception))
        self.assertFalse(self._destination.exists())

    def test_fresh_reference_passes(self) -> None:
        self._destination.write_text(GENERATED, encoding="utf-8")
        self.assertEqual(verify_docs.verify_generated_references(), 1)

    def test_regeneration_writes_and_then_verification_passes(self) -> None:
        self._destination.write_text("# hand-edited drift\n", encoding="utf-8")
        self.assertEqual(verify_docs.regenerate_generated_references(), 1)
        self.assertEqual(self._destination.read_text(encoding="utf-8"), GENERATED)
        self.assertEqual(verify_docs.verify_generated_references(), 1)


if __name__ == "__main__":
    unittest.main()
