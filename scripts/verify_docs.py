#!/usr/bin/env python3
"""One-pass generation, freshness, link, diagram, and example verification."""

from __future__ import annotations

import argparse
import html.parser
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

import generate_doc_references


ROOT = Path(__file__).resolve().parents[1]
GUIDES = ROOT / "docs/guides"
REFERENCE = ROOT / "docs/reference"
DIAGRAMS = ROOT / "docs/guides/diagrams"


class VerificationError(RuntimeError):
    """A documentation contract failed."""


def markdown_files() -> list[Path]:
    """Documentation owned by this lane."""

    return sorted(GUIDES.rglob("*.md")) + sorted(
        path
        for path in REFERENCE.glob("*.md")
        if path.name != "capabilities.md"
    )


def slugify_heading(text: str) -> str:
    """Approximate GitHub Markdown's ordinary heading slug."""

    text = re.sub(r"`([^`]*)`", r"\1", text).strip().lower()
    text = re.sub(r"[^a-z0-9 _-]", "", text)
    return re.sub(r"[ _]+", "-", text).strip("-")


def markdown_anchors(path: Path) -> set[str]:
    """Collect ordinary heading anchors from one Markdown file."""

    anchors: set[str] = set()
    for line in path.read_text(encoding="utf-8").splitlines():
        match = re.match(r"^#{1,6}\s+(.+?)\s*#*\s*$", line)
        if match:
            anchors.add(slugify_heading(match.group(1)))
    return anchors


def verify_links(paths: list[Path]) -> int:
    """Verify relative Markdown links and optional local anchors."""

    checked = 0
    pattern = re.compile(r"(?<!!)\[[^\]]+\]\(([^)]+)\)")
    for path in paths:
        text = path.read_text(encoding="utf-8")
        for raw_target in pattern.findall(text):
            target = raw_target.strip().strip("<>")
            if not target or target.startswith(("http://", "https://", "mailto:")):
                continue
            file_part, separator, anchor = target.partition("#")
            destination = path if not file_part else (path.parent / file_part).resolve()
            try:
                destination.relative_to(ROOT)
            except ValueError as error:
                raise VerificationError(f"{path.relative_to(ROOT)} links outside the repo") from error
            if not destination.is_file():
                raise VerificationError(
                    f"{path.relative_to(ROOT)} has missing link target {target}"
                )
            if separator and destination.suffix.lower() == ".md":
                if anchor not in markdown_anchors(destination):
                    raise VerificationError(
                        f"{path.relative_to(ROOT)} has missing anchor {target}"
                    )
            checked += 1
    return checked


def shell_blocks(path: Path) -> list[tuple[str, str]]:
    """Extract classified shell blocks."""

    blocks: list[tuple[str, str]] = []
    pattern = re.compile(r"```(?:sh|shell|bash)\n(.*?)\n```", re.DOTALL)
    for body in pattern.findall(path.read_text(encoding="utf-8")):
        lines = body.splitlines()
        if not lines or not lines[0].startswith("# docs-check: "):
            raise VerificationError(
                f"{path.relative_to(ROOT)} has an unclassified shell example"
            )
        classification = lines[0].removeprefix("# docs-check: ").strip()
        if classification not in {"run", "syntax", "manual-live"}:
            raise VerificationError(
                f"{path.relative_to(ROOT)} has unknown docs-check class {classification}"
            )
        blocks.append((classification, "\n".join(lines[1:])))
    return blocks


def run_shell(command: str, environment: dict[str, str], timeout: int) -> None:
    """Run one shell example with bounded, plain output."""

    completed = subprocess.run(
        ["sh", "-eu", "-c", command],
        cwd=ROOT,
        env=environment,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=timeout,
        check=False,
    )
    if completed.returncode != 0:
        tail = "\n".join(completed.stdout.splitlines()[-30:])
        raise VerificationError(f"shell example failed ({completed.returncode}):\n{tail}")


def verify_examples(paths: list[Path]) -> tuple[int, int]:
    """Syntax-check every example and execute deterministic examples once."""

    syntax_count = 0
    run_count = 0
    with tempfile.TemporaryDirectory(prefix="heycode-docs-") as temporary:
        environment = os.environ.copy()
        environment.update(
            {
                "TMPDIR": temporary,
                "HEYCODE_DOCS_TMP": temporary,
                "CARGO_TERM_COLOR": "never",
                "NO_COLOR": "1",
            }
        )
        for path in paths:
            for classification, body in shell_blocks(path):
                syntax = subprocess.run(
                    ["sh", "-n", "-c", body],
                    cwd=ROOT,
                    env=environment,
                    text=True,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.STDOUT,
                    timeout=15,
                    check=False,
                )
                if syntax.returncode != 0:
                    raise VerificationError(
                        f"invalid shell syntax in {path.relative_to(ROOT)}:\n{syntax.stdout}"
                    )
                syntax_count += 1
                if classification == "run":
                    run_shell(body, environment, timeout=300)
                    run_count += 1
    return syntax_count, run_count


class SvgAudit(html.parser.HTMLParser):
    """Minimal accessible-SVG audit for a standalone diagram."""

    def __init__(self) -> None:
        super().__init__()
        self.svg_count = 0
        self.svg_role: str | None = None
        self.labelledby: list[str] = []
        self.ids: set[str] = set()
        self.first_svg_child: str | None = None
        self.svg_depth = 0
        self.title_text = ""
        self.desc_text = ""
        self.capture: str | None = None
        self.diagonal_lines: list[dict[str, str]] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        attr = {name: value or "" for name, value in attrs}
        if tag == "svg":
            self.svg_count += 1
            self.svg_depth = 1
            self.svg_role = attr.get("role")
            self.labelledby = attr.get("aria-labelledby", "").split()
        elif self.svg_depth:
            if self.svg_depth == 1 and self.first_svg_child is None:
                self.first_svg_child = tag
            self.svg_depth += 1
            identifier = attr.get("id")
            if identifier:
                self.ids.add(identifier)
            if tag in {"title", "desc"}:
                self.capture = tag
            if tag == "line":
                x1, x2 = attr.get("x1"), attr.get("x2")
                y1, y2 = attr.get("y1"), attr.get("y2")
                if x1 and x2 and y1 and y2 and x1 != x2 and y1 != y2:
                    self.diagonal_lines.append(attr)

    def handle_startendtag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        self.handle_starttag(tag, attrs)
        if self.svg_depth:
            self.svg_depth -= 1

    def handle_endtag(self, tag: str) -> None:
        if tag in {"title", "desc"}:
            self.capture = None
        if self.svg_depth:
            self.svg_depth -= 1

    def handle_data(self, data: str) -> None:
        if self.capture == "title":
            self.title_text += data
        elif self.capture == "desc":
            self.desc_text += data


def installed_diagram_self_check() -> Path | None:
    """Locate the optional installed Diagram Design checker."""

    base = Path.home() / ".codex/plugins/cache/diagram-design/diagram-design"
    candidates = sorted(base.glob("*/skills/diagram-design/scripts/self_check.py"))
    return candidates[-1] if candidates else None


def verify_diagrams() -> tuple[int, bool]:
    """Verify standalone diagram accessibility and optional skill checks."""

    diagrams = sorted(DIAGRAMS.glob("*.html"))
    if len(diagrams) != 3:
        raise VerificationError(f"expected 3 DOC05 diagrams, found {len(diagrams)}")
    external = installed_diagram_self_check()
    for path in diagrams:
        text = path.read_text(encoding="utf-8")
        if any(forbidden in text for forbidden in ("JetBrains Mono", "box-shadow", "writing-mode")):
            raise VerificationError(f"{path.relative_to(ROOT)} contains a forbidden diagram style")
        audit = SvgAudit()
        audit.feed(text)
        if audit.svg_count != 1 or audit.svg_role != "img":
            raise VerificationError(f"{path.relative_to(ROOT)} must contain one role=img SVG")
        if audit.first_svg_child != "title":
            raise VerificationError(f"{path.relative_to(ROOT)} SVG title is not first")
        if len(audit.labelledby) != 2 or not set(audit.labelledby).issubset(audit.ids):
            raise VerificationError(f"{path.relative_to(ROOT)} aria-labelledby is unresolved")
        if not audit.title_text.strip() or not audit.desc_text.strip():
            raise VerificationError(f"{path.relative_to(ROOT)} has an empty title/description")
        if audit.diagonal_lines:
            raise VerificationError(f"{path.relative_to(ROOT)} contains diagonal SVG lines")
        if external is not None:
            completed = subprocess.run(
                [sys.executable, str(external), str(path)],
                cwd=ROOT,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                timeout=30,
                check=False,
            )
            if completed.returncode != 0:
                raise VerificationError(
                    f"Diagram Design self-check failed for {path.relative_to(ROOT)}:\n"
                    f"{completed.stdout}"
                )
    return len(diagrams), external is not None


def verify_generated_references() -> int:
    """Fail unless every generated reference on disk matches current sources.

    Verification never writes. Regeneration is a separate, explicit step
    (`--write` here, or `generate_doc_references.py --write`), because a gate
    that rewrites the bytes it is about to compare can never fail.
    """

    rendered = generate_doc_references.render_all()
    stale = generate_doc_references.check_all(rendered)
    if stale:
        raise VerificationError(
            ", ".join(stale)
            + "; regenerate with python3 scripts/verify_docs.py --write"
        )
    return len(rendered)


def regenerate_generated_references() -> int:
    """Rewrite every generated reference from current sources."""

    rendered = generate_doc_references.render_all()
    generate_doc_references.write_all(rendered)
    return len(rendered)


def main(argv: list[str] | None = None) -> int:
    """Run the complete docs lane verification once."""

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--write",
        action="store_true",
        help="regenerate the generated references before verifying them",
    )
    args = parser.parse_args(argv)
    try:
        if args.write:
            regenerate_generated_references()
        references = verify_generated_references()
        paths = markdown_files()
        links = verify_links(paths)
        syntax_examples, run_examples = verify_examples(paths)
        diagrams, external_check = verify_diagrams()
        if not (REFERENCE / "capabilities.md").is_file():
            raise VerificationError("existing generated capabilities.md is missing")
        print(f"generated references: {references} fresh")
        print(f"markdown links: {links} valid across {len(paths)} files")
        print(
            f"shell examples: {syntax_examples} syntax-valid; "
            f"{run_examples} deterministic example(s) executed"
        )
        print(
            f"diagrams: {diagrams} accessible; "
            f"installed skill self-check={'run' if external_check else 'not installed'}"
        )
        return 0
    except (VerificationError, generate_doc_references.GenerationError) as error:
        print(f"documentation verification failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
