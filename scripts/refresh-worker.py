#!/usr/bin/env python3
"""复用离线依赖和模型时，重新安装当前源码的 worker 并清理绝对路径入口。"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


def refresh(python_root: Path) -> None:
    python_root = python_root.resolve()
    python = python_root / ("python.exe" if os.name == "nt" else "bin/python3")
    if not python.is_file():
        raise ValueError(f"找不到包内 Python：{python}")
    uv = shutil.which("uv")
    if uv is None:
        raise RuntimeError("更新包内 worker 需要构建工具 uv")
    subprocess.run([uv, "pip", "install", "--python", str(python), "--no-config",
                    "--break-system-packages", "--no-deps", "--reinstall", "--link-mode=copy",
                    str(REPO / "python/separator")], check=True,
                   env=dict(os.environ, PYTHONUTF8="1"))
    scripts = python_root / ("Scripts" if os.name == "nt" else "bin")
    for entry in scripts.glob("k3-separator*"):
        entry.unlink()
    print("包内 worker 已更新为当前源码", flush=True)


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("python_root", type=Path)
    refresh(parser.parse_args().python_root)
