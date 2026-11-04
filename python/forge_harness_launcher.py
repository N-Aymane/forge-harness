"""Download a verified Forge release on first use, then run its Rust binary."""

import hashlib
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
from urllib.request import urlopen
import zipfile

VERSION = "v0.2.0-preview.1"
BASE_URL = "https://github.com/N-Aymane/forge-harness/releases/download"
ASSETS = {
    ("Linux", "x86_64"): (
        "forge-Linux-X64.tar.gz",
        "70f1ad610ddd371e4e2c299b809970a6aa9e11ba80638ebdb81266dcf96db75d",
    ),
    ("Darwin", "arm64"): (
        "forge-macOS-ARM64.tar.gz",
        "fa17fc85cbb31d8ef96363a6df0dc8b3845e087cae9705d10c535a2d6e077b5b",
    ),
    ("Windows", "amd64"): (
        "forge-Windows-X64.zip",
        "73a978683ed9c06714d4067dbcde9fa896c05b53745ce22819dbf1727b3e1d31",
    ),
}


def install_binary():
    system = platform.system()
    machine = platform.machine().lower()
    if system == "Windows" and machine == "x86_64":
        machine = "amd64"
    asset = ASSETS.get((system, machine))
    if asset is None:
        raise ValueError(f"No Forge release for {system} {machine}. Build from source.")
    archive_name, expected_hash = asset
    executable = "forge.exe" if system == "Windows" else "forge"
    cache = Path.home() / ".forge-launcher"
    binary = cache / VERSION / machine / executable
    if binary.is_file():
        return binary

    binary.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=binary.parent) as work:
        archive = Path(work) / archive_name
        digest = hashlib.sha256()
        with urlopen(f"{BASE_URL}/{VERSION}/{archive_name}", timeout=30) as response:
            with archive.open("wb") as output:
                for chunk in iter(lambda: response.read(65536), b""):
                    digest.update(chunk)
                    output.write(chunk)
        if digest.hexdigest() != expected_hash:
            raise ValueError("Forge archive checksum mismatch.")

        staged = Path(work) / executable
        with staged.open("wb") as output:
            if system == "Windows":
                with zipfile.ZipFile(archive) as bundle, bundle.open(executable) as source:
                    shutil.copyfileobj(source, output)
            else:
                with tarfile.open(archive, "r:gz") as bundle:
                    member = bundle.getmember(executable)
                    if not member.isfile():
                        raise ValueError("Forge archive executable is not a regular file.")
                    with bundle.extractfile(member) as source:
                        shutil.copyfileobj(source, output)
        staged.chmod(0o755)
        try:
            staged.replace(binary)
        except PermissionError:
            # A concurrent launch can already have installed and opened it on Windows.
            if not binary.is_file():
                raise
    return binary


def main():
    try:
        binary = install_binary()
        argv = [str(binary), *sys.argv[1:]]
        if os.name != "nt":
            os.execv(binary, argv)
        with subprocess.Popen(argv) as child:
            while True:
                try:
                    return child.wait()
                except KeyboardInterrupt:
                    # The Rust process receives the same console event.
                    continue
    except (OSError, ValueError, KeyError, tarfile.TarError, zipfile.BadZipFile) as error:
        print(f"Forge launcher: {error}", file=sys.stderr)
        return 1
