"""POSIX PTY smoke test: real keyboard approval, resize, and terminal cleanup.

Run with Python 3 and a built Forge binary: python3 tests/tui_smoke.py target/debug/forge
Uses only the standard library and a local mock HTTP server. No paid API calls.
"""
import argparse
import fcntl
import http.server
import json
import os
import pathlib
import pty
import select
import signal
import sqlite3
import struct
import subprocess
import tempfile
import termios
import threading
import time


class Provider(http.server.BaseHTTPRequestHandler):
    calls = 0

    def log_message(self, *_args):
        pass

    def do_POST(self):
        self.rfile.read(int(self.headers["Content-Length"]))
        type(self).calls += 1
        if self.calls == 1:
            delta = {"tool_calls": [{"index": 0, "id": "pty-write", "function": {
                "name": "write_file", "arguments": json.dumps({"path": "approved.txt", "content": "approved\n"})}}]}
            reason = "tool_calls"
        else:
            delta, reason = {"content": "PTY smoke complete"}, "stop"
        frame = {"choices": [{"delta": delta, "finish_reason": reason}],
                 "usage": {"prompt_tokens": 10, "completion_tokens": 10}}
        body = ("data: " + json.dumps(frame) + "\n\ndata: [DONE]\n\n").encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary")
    parser.add_argument("--screenshots", type=pathlib.Path)
    args = parser.parse_args()
    binary = str(pathlib.Path(args.binary).resolve())
    capture = None
    if args.screenshots:
        from terminal_capture import TerminalCapture
        capture = TerminalCapture()
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    with tempfile.TemporaryDirectory() as home, tempfile.TemporaryDirectory() as workspace:
        pathlib.Path(home, "config.toml").write_text(
            'default_model="mock"\n[models.mock]\nprovider="compatible"\nmodel="mock"\n'
            f'base_url="http://127.0.0.1:{server.server_port}/v1"\n'
            'key_env="FORGE_PTY_KEY"\ncontext_tokens=100000\nmax_output_tokens=100\n')
        master, slave = pty.openpty()
        original = termios.tcgetattr(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 35, 120, 0, 0))
        env = dict(os.environ, TERM="xterm-256color", FORGE_PTY_KEY="pty-test-key")
        process = subprocess.Popen([binary, "--home", home, "-C", workspace],
                                   stdin=slave, stdout=slave, stderr=slave, env=env)
        captured = bytearray()

        def receive():
            data = os.read(master, 65536)
            captured.extend(data)
            if capture:
                capture.feed(data)

        def screenshot(name, title):
            if not capture:
                return
            deadline = time.monotonic() + 0.2
            while time.monotonic() < deadline:
                if select.select([master], [], [], 0.025)[0]:
                    receive()
            capture.save(args.screenshots / name, title)

        def until(predicate, timeout=15):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                if select.select([master], [], [], 0.05)[0]:
                    receive()
                if predicate():
                    return
                if process.poll() is not None:
                    raise AssertionError("Forge exited unexpectedly: " + repr(bytes(captured[-2000:])))
            raise AssertionError("Timed out waiting for terminal state: " + repr(bytes(captured[-2000:])))

        try:
            until(lambda: b"Message" in captured)
            screenshot("01-start.png", "Forge / startup / real PTY test with a local mock provider")
            os.write(master, b"/us")
            until(lambda: b"/usage" in captured)
            screenshot("06-commands.png", "Forge / filtered slash commands in the composer")
            os.write(master, b"\r")
            until(lambda: b"No requests yet" in captured)
            assert Provider.calls == 0
            os.write(master, b"create approved.txt\r")
            until(lambda: b"Review action" in captured)
            screenshot("02-approval.png", "Forge / review a proposed file change before approval")
            assert not pathlib.Path(workspace, "approved.txt").exists()
            # Resize while reviewing the action, then approve using the keyboard.
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 20, 60, 0, 0))
            if capture:
                capture.resize(60, 20)
            process.send_signal(signal.SIGWINCH)
            screenshot("03-narrow.png", "Forge / 60-column approval dialog / actual terminal resize")
            os.write(master, b"y")
            until(lambda: pathlib.Path(workspace, "approved.txt").exists() and Provider.calls == 2)
            assert pathlib.Path(workspace, "approved.txt").read_text() == "approved\n"
            def finished():
                with sqlite3.connect(pathlib.Path(home, "forge.db")) as db:
                    row = db.execute("SELECT context FROM sessions").fetchone()
                    messages = json.loads(row[0])
                    return messages[-1]["role"] == "assistant" and messages[-1]["text"] == "PTY smoke complete"

            until(finished)
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 35, 120, 0, 0))
            if capture:
                capture.resize(120, 35)
            process.send_signal(signal.SIGWINCH)
            screenshot("04-complete.png", "Forge / approved file written / follow-up and usage visible")
            time.sleep(0.2)
            os.write(master, b"\x03")
            assert process.wait(timeout=5) == 0
            assert termios.tcgetattr(slave) == original, "terminal mode was not restored"
            assert b"pty-test-key" not in captured
            print("PTY smoke passed: keyboard approval, resize, agent follow-up, and terminal restoration")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            os.close(master)
            os.close(slave)
            server.shutdown()


if __name__ == "__main__":
    main()
