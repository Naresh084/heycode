#!/usr/bin/env python3
"""Render actual pyte terminal cells to PNG for visual review.

Requires Pillow and pyte. This does not synthesize UI: callers pass the live
screen from their PTY capture. Default terminal colors are an explicit dark
terminal profile; ANSI/truecolor cell attributes take precedence.
"""
from __future__ import annotations

import argparse
import math
from pathlib import Path
import re

import pyte
from PIL import Image, ImageDraw, ImageFont

PALETTE = {
    "black": "#000000", "red": "#cd3131", "green": "#0dbc79",
    "brown": "#e5e510", "yellow": "#e5e510", "blue": "#2472c8",
    "magenta": "#bc3fbc", "cyan": "#11a8cd", "white": "#e5e5e5",
    "brightblack": "#666666", "brightred": "#f14c4c",
    "brightgreen": "#23d18b", "brightbrown": "#f5f543",
    "brightyellow": "#f5f543", "brightblue": "#3b8eea",
    "brightmagenta": "#d670d6", "brightcyan": "#29b8db",
    "brightwhite": "#ffffff",
}


class TerminalByteStream(pyte.ByteStream):
    """pyte byte stream with unsupported private CSI key modes ignored.

    pyte 0.8.x drops the ``>`` marker from ``CSI > Ps m`` and dispatches the
    numeric parameter as SGR. In particular, xterm's ``CSI > 4 m`` keyboard
    option becomes SGR underline and decorates every subsequently erased cell.
    A real terminal treats that private sequence as a keyboard-mode control,
    not text styling. Preserve the raw ANSI at the caller, but exclude only
    this unsupported private ``...m`` family from screen emulation.
    """

    _PRIVATE_KEY_MODE = re.compile(rb"\x1b\[>[0-9;:]*m")
    _PARTIAL_PRIVATE_KEY_MODE = re.compile(
        rb"(?:\x1b|\x1b\[|\x1b\[>[0-9;:]*)$"
    )

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self._private_key_mode_pending = b""

    def feed(self, data: bytes) -> None:
        combined = self._private_key_mode_pending + data
        self._private_key_mode_pending = b""
        partial = self._PARTIAL_PRIVATE_KEY_MODE.search(combined)
        if partial is not None:
            self._private_key_mode_pending = partial.group()
            combined = combined[: partial.start()]
        super().feed(self._PRIVATE_KEY_MODE.sub(b"", combined))


def _color(value, default):
    if value == "default":
        return default
    if value in PALETTE:
        return PALETTE[value]
    if len(value) == 6 and all(c in "0123456789abcdefABCDEF" for c in value):
        return "#" + value
    raise ValueError(f"Unknown terminal color: {value!r}")


def render_screen(screen, output, *, font_path=None, font_size=16,
                  foreground="#dddddd", background="#101014", padding=16):
    """Write a pixel-faithful cell layout, preserving colors and emphasis."""
    if font_path is None:
        candidates = [
            "/System/Library/Fonts/Menlo.ttc",
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        ]
        font_path = next((path for path in candidates if Path(path).is_file()), None)
        if font_path is None:
            raise ValueError("Pass font_path for an installed monospace font")
    regular = ImageFont.truetype(str(font_path), font_size)
    # Menlo collection index 1 is bold. Other font files use stroke emphasis.
    is_menlo = Path(font_path).name == "Menlo.ttc"
    bold = ImageFont.truetype(str(font_path), font_size, index=1) if is_menlo else regular
    fallbacks = [ImageFont.truetype(path, font_size) for path in [
        "/System/Library/Fonts/Apple Symbols.ttf",
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    ] if Path(path).is_file()]
    missing = {id(font): bytes(font.getmask(chr(0x10ffff)))
               for font in [regular, bold, *fallbacks]}
    glyph_fonts = {}

    def glyph_font(char, emphasis):
        key = (char, emphasis)
        if key not in glyph_fonts:
            primary = bold if emphasis else regular
            glyph_fonts[key] = next((font for font in [primary, regular, *fallbacks]
                                    if bytes(font.getmask(char)) != missing[id(font)]), primary)
        return glyph_fonts[key]
    cell_width = math.ceil(regular.getlength("M"))
    ascent, descent = regular.getmetrics()
    cell_height = ascent + descent + 2
    canvas = Image.new("RGB", (screen.columns * cell_width + 2 * padding,
                               screen.lines * cell_height + 2 * padding), background)
    draw = ImageDraw.Draw(canvas)
    # Fill all backgrounds first: a double-width glyph can span its next cell.
    for row in range(screen.lines):
        for column in range(screen.columns):
            char = screen.buffer[row][column]
            fg, bg = _color(char.fg, foreground), _color(char.bg, background)
            if char.reverse:
                fg, bg = bg, fg
            x, y = padding + column * cell_width, padding + row * cell_height
            draw.rectangle((x, y, x + cell_width - 1, y + cell_height - 1), fill=bg)
    for row in range(screen.lines):
        for column in range(screen.columns):
            char = screen.buffer[row][column]
            fg, bg = _color(char.fg, foreground), _color(char.bg, background)
            if char.reverse:
                fg, bg = bg, fg
            x, y = padding + column * cell_width, padding + row * cell_height
            if char.data:
                draw.text((x, y + ascent), char.data, font=glyph_font(char.data, char.bold),
                          fill=fg, anchor="ls", stroke_width=int(char.bold and not is_menlo))
            if char.underscore:
                draw.line((x, y + ascent + 1, x + cell_width - 1, y + ascent + 1), fill=fg)
            if char.strikethrough:
                draw.line((x, y + ascent // 2, x + cell_width - 1, y + ascent // 2), fill=fg)
    output = Path(output)
    output.parent.mkdir(parents=True, exist_ok=True)
    canvas.save(output)
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("ansi", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--columns", type=int, required=True)
    parser.add_argument("--rows", type=int, required=True)
    parser.add_argument("--font", type=Path)
    args = parser.parse_args()
    class Screen(pyte.Screen):
        def set_mode(self, *modes, **kwargs):
            if kwargs.get("private") and 1049 in modes:
                self.reset()
            return super().set_mode(*modes, **kwargs)

    screen = Screen(args.columns, args.rows)
    TerminalByteStream(screen).feed(args.ansi.read_bytes())
    render_screen(screen, args.output, font_path=args.font)


if __name__ == "__main__":
    main()
