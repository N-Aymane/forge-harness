"""Render real terminal bytes as PNGs; no application UI is reconstructed by hand."""
import hashlib
import json
import math
import os
import pathlib

import pyte
from PIL import Image, ImageDraw, ImageFont


PALETTE = {
    "black": "#000000", "red": "#cd3131", "green": "#0dbc79",
    "brown": "#e5e510", "yellow": "#e5e510", "blue": "#2472c8",
    "magenta": "#bc3fbc", "cyan": "#11a8cd", "white": "#e5e5e5",
    "brightblack": "#666666", "brightred": "#f14c4c", "brightgreen": "#23d18b",
    "brightyellow": "#f5f543", "brightblue": "#3b8eea", "brightmagenta": "#d670d6",
    "brightcyan": "#29b8db", "brightwhite": "#ffffff",
}


class TerminalCapture:
    def __init__(self, columns=120, rows=35):
        self.screen = pyte.Screen(columns, rows)
        self.stream = pyte.ByteStream(self.screen)
        self.raw = bytearray()

    def feed(self, data):
        self.raw.extend(data)
        self.stream.feed(data)

    def resize(self, columns, rows):
        self.screen.resize(lines=rows, columns=columns)

    def save(self, path, title):
        path = pathlib.Path(path)
        path.parent.mkdir(parents=True, exist_ok=True)
        font_path = os.environ.get("FORGE_SCREENSHOT_FONT", "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf")
        font = ImageFont.truetype(font_path, 16)
        bold_path = font_path.replace(".ttf", "-Bold.ttf")
        bold = ImageFont.truetype(bold_path, 16) if pathlib.Path(bold_path).exists() else font
        width, height = math.ceil(font.getlength("M")), 22
        padding, header = 24, 50
        image = Image.new("RGB", (self.screen.columns * width + padding * 2,
                                  self.screen.lines * height + padding * 2 + header), "#101419")
        draw = ImageDraw.Draw(image)
        draw.text((padding, 14), title, font=font, fill="#aeb7c2")
        draw.line((padding, 41, image.width - padding, 41), fill="#303944")

        def color(value, fallback):
            if value == "default":
                return fallback
            if value in PALETTE:
                return PALETTE[value]
            if len(value) == 6 and all(c in "0123456789abcdefABCDEF" for c in value):
                return "#" + value
            return fallback

        for y in range(self.screen.lines):
            for x in range(self.screen.columns):
                cell = self.screen.buffer[y][x]
                fg, bg = color(cell.fg, "#e1e7ee"), color(cell.bg, "#101419")
                if cell.reverse:
                    fg, bg = bg, fg
                left, top = padding + x * width, padding + header + y * height
                if bg != "#101419":
                    draw.rectangle((left, top, left + width, top + height), fill=bg)
                if cell.data.strip():
                    draw.text((left, top), cell.data, font=bold if cell.bold else font, fill=fg)
                if cell.underscore:
                    draw.line((left, top + height - 2, left + width, top + height - 2), fill=fg)
        image.save(path, optimize=True)
        path.with_suffix(".ansi").write_bytes(self.raw)
        path.with_suffix(".txt").write_text("\n".join(self.screen.display) + "\n", encoding="utf-8")
        path.with_suffix(".json").write_text(json.dumps({
            "title": title, "source": "captured terminal bytes from the real Forge test",
            "columns": self.screen.columns, "rows": self.screen.lines,
            "ansi_sha256": hashlib.sha256(self.raw).hexdigest(),
            "renderer": "pyte terminal emulator and Pillow; fixed dark terminal theme",
        }, indent=2) + "\n", encoding="utf-8")
