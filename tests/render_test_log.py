"""Render the tail of actual Cargo test output for documentation evidence."""
import argparse
import pathlib
from terminal_capture import TerminalCapture

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("log", type=pathlib.Path)
parser.add_argument("output", type=pathlib.Path)
args = parser.parse_args()
capture = TerminalCapture(120, 42)
text = args.log.read_text(encoding="utf-8", errors="replace")
capture.feed(text.replace("\r\n", "\n").replace("\n", "\r\n").encode())
capture.save(args.output, "Forge / actual cargo test --locked --all-targets output (tail)")
