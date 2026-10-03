#!/usr/bin/env python3
"""Relocate a package and verify native programs, Python and audio separation."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import wave
import zipfile
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent


def run(*args: str | Path, **kwargs) -> subprocess.CompletedProcess:
    kwargs["env"] = dict(kwargs.get("env", os.environ), PYTHONUTF8="1")
    return subprocess.run([str(arg) for arg in args], check=True, timeout=600, **kwargs)


def check(archive: Path, cli_only: bool) -> None:
    archive = archive.resolve()
    with archive.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    expected = archive.with_name(archive.name + ".sha256").read_text().split()[0]
    if digest != expected:
        raise RuntimeError("Package SHA-256 verification failed")
    with tempfile.TemporaryDirectory(prefix="k3-dist-check-") as directory:
        work = Path(directory)
        unpacked = work / "unpacked"
        unpacked.mkdir()
        if archive.suffix == ".zip":
            with zipfile.ZipFile(archive) as stream:
                stream.extractall(unpacked)
        else:
            with tarfile.open(archive) as stream:
                stream.extractall(unpacked, filter="data")
        roots = list(unpacked.iterdir())
        if len(roots) != 1 or not roots[0].is_dir():
            raise RuntimeError("Package must contain exactly one top-level directory")
        root = work / "移动后的 K3 包"
        shutil.move(str(roots[0]), root)
        check_root(root, work, cli_only)


def check_root(root: Path, work: Path, cli_only: bool, command_dir: Path | None = None) -> None:
    extension = ".exe" if os.name == "nt" else ""
    commands = command_dir or root
    binary = commands / f"k3{extension}"
    run(binary, "--help", stdout=subprocess.DEVNULL, cwd=work)
    if cli_only:
        print("CLI package startup check passed")
        return
    python = root / ("runtime/python/python.exe" if os.name == "nt" else "runtime/python/bin/python3")
    # 将用户 Python 环境污染也纳入测试；原生 launcher 必须忽略它。
    environment = dict(os.environ, PYTHONHOME=str(work / "missing-python"),
                       PYTHONPATH=str(work / "missing-packages"), K3_LOG_DIR=str(work / "logs"))
    try:
        response = run(commands / f"k3-separator{extension}", input='{"id":"check","method":"health"}\n',
                       text=True, encoding="utf-8", capture_output=True, env=environment, cwd=work)
    except subprocess.CalledProcessError as error:
        raise RuntimeError(f"Worker health check failed: {error.stderr}") from error
    health = json.loads(response.stdout)
    if not health["ok"] or not health["result"]["runtime"]["audio_separator_installed"]:
        raise RuntimeError(f"Worker health check failed after relocation: {health}")
    run(python, "-s", SCRIPTS / "check-runtime.py", root, cwd=work)
    song = work / "测试音频.wav"
    with wave.open(str(song), "wb") as stream:
        stream.setparams((2, 2, 44_100, 0, "NONE", "not compressed"))
        stream.writeframes(b"".join(
            struct.pack("<hh", value, value) for value in
            (int(1000 * math.sin(2 * math.pi * 440 * index / 44_100)) for index in range(22_050))))
    project = work / "测试工程"
    run(binary, "new", "--root", project, "--song", song, "--title", "离线包检查", cwd=work)
    # 不指定 --worker / --model-dir，检查 CLI 自动发现同包 worker 与模型。
    run(binary, "separate", "--project", project, "--profile", "fast", "--segment-size", "128",
        "--no-autocast", env=environment, cwd=work)
    # SoundFile 支持模型输出的 IEEE float WAV；标准库 wave 只支持 PCM。
    audio_check = """
import sys
import json
from pathlib import Path
import numpy as np
import soundfile as sf
project = Path(sys.argv[1])
manifest = json.loads((project / 'project.json').read_text(encoding='utf-8'))['separation']['details']
for name in ('vocals', 'accompaniment', 'backing_vocals'):
    audio, rate = sf.read(project / manifest[name], always_2d=True)
    if len(audio) == 0 or rate != 44100 or not np.isfinite(audio).all():
        raise RuntimeError(f'Invalid separated audio: {name}')
"""
    run(python, "-I", "-c", audio_check, project, cwd=work)
    if os.name != "nt" and (root / "separate.sh").is_file():
        batch_environment = dict(environment, K3_PROFILE="fast", K3_AUTOCAST="false",
                                 K3_SEGMENT_SIZE="128")
        run(root / "separate.sh", "-f", song, "-d", work / "batch-projects",
            env=batch_environment, cwd=work)
        run(python, "-I", "-c", audio_check,
            work / "batch-projects" / song.stem, cwd=work)
    print("Program checks passed: offline model loading, default CLI separation and backing vocals output")


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path, nargs="?")
    parser.add_argument("--installed-root", type=Path, help="Check an existing installation without relocating or rewriting program files")
    parser.add_argument("--command-dir", type=Path, help="Check the directory containing system command links")
    parser.add_argument("--cli-only", action="store_true")
    args = parser.parse_args()
    if args.installed_root:
        if args.archive:
            parser.error("archive and --installed-root cannot be combined")
        with tempfile.TemporaryDirectory(prefix="k3-installed-check-") as directory:
            check_root(args.installed_root.resolve(), Path(directory), args.cli_only, args.command_dir)
    elif args.archive:
        if args.command_dir:
            parser.error("--command-dir requires --installed-root")
        check(args.archive, args.cli_only)
    else:
        parser.error("Provide archive or --installed-root")
