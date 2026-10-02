#!/usr/bin/env python3
"""解压到含空格和中文的新路径，验证包内程序、Python 与真实短音频分离。"""

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
        raise RuntimeError("发行包 SHA-256 校验失败")
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
            raise RuntimeError("发行包必须包含唯一顶级目录")
        root = work / "移动后的 K3 包"
        shutil.move(str(roots[0]), root)
        extension = ".exe" if os.name == "nt" else ""
        binary = root / f"k3{extension}"
        run(binary, "--help", stdout=subprocess.DEVNULL, cwd=work)
        if cli_only:
            print("CLI 包启动检查通过")
            return
        python = root / ("runtime/python/python.exe" if os.name == "nt" else "runtime/python/bin/python3")
        # 将用户 Python 环境污染也纳入测试；原生 launcher 必须忽略它。
        environment = dict(os.environ, PYTHONHOME=str(work / "missing-python"),
                           PYTHONPATH=str(work / "missing-packages"), K3_LOG_DIR=str(work / "logs"))
        response = run(root / f"k3-separator{extension}", input='{"id":"check","method":"health"}\n',
                       text=True, encoding="utf-8", capture_output=True, env=environment, cwd=work)
        health = json.loads(response.stdout)
        if not health["ok"] or not health["result"]["runtime"]["audio_separator_installed"]:
            raise RuntimeError(f"移动后 worker 健康检查失败：{health}")
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
from pathlib import Path
import numpy as np
import soundfile as sf
for name in ('vocals.wav', 'accompaniment.wav', 'backing-vocals.wav'):
    audio, rate = sf.read(Path(sys.argv[1]) / name, always_2d=True)
    if len(audio) == 0 or rate != 44100 or not np.isfinite(audio).all():
        raise RuntimeError(f'Invalid separated audio: {name}')
"""
        run(python, "-I", "-c", audio_check, project / "stems", cwd=work)
        if os.name != "nt" and (root / "separate.sh").is_file():
            batch_environment = dict(environment, K3_PROFILE="fast", K3_AUTOCAST="false",
                                     K3_SEGMENT_SIZE="128")
            run(root / "separate.sh", "-f", song, "-d", work / "batch-projects",
                env=batch_environment, cwd=work)
            run(python, "-I", "-c", audio_check,
                work / "batch-projects" / song.stem / "stems", cwd=work)
        print("发行包校验通过：可移动、模型离线载入、CLI 默认分离与和声输出")


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("--cli-only", action="store_true")
    args = parser.parse_args()
    check(args.archive, args.cli_only)
