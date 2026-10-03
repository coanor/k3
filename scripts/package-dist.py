#!/usr/bin/env python3
"""将当前平台的程序与完整 runtime 打包；CLI 版本需显式指定 --cli-only。"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import sys
import tempfile
from pathlib import Path

from dist_manuals import copy_manuals
from dist_archive import write_archive

REPO = Path(__file__).resolve().parent.parent


def package(platform: str, name: str, binary: Path, runtime: Path | None,
            cli_only: bool, dist: Path, gui_binary: Path | None = None) -> Path:
    if not re.fullmatch(r"k3-[a-z0-9_-]+", name):
        raise ValueError(f"无效包名：{name}")
    if not binary.is_file():
        raise ValueError(f"找不到待打包二进制：{binary}")
    if cli_only != name.endswith("-cli"):
        raise ValueError("CLI 包必须使用 -cli 后缀；完整包不能使用此后缀")
    if not cli_only and runtime is None:
        raise ValueError("完整包必须指定 runtime 目录；仅打包程序时请指定 --cli-only")
    if cli_only and runtime:
        raise ValueError("--cli-only 不能同时指定 runtime")
    extension = ".exe" if platform == "windows" else ""
    dist.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="k3-package-", dir=dist) as directory:
        staging = Path(directory)
        root = staging / name
        root.mkdir()
        shutil.copy2(binary, root / f"k3{extension}")
        (root / f"k3{extension}").chmod(0o755)
        copy_manuals(root)
        if runtime:
            manifest = json.loads((runtime / "bundle-manifest.json").read_text(encoding="utf-8"))
            expected_platform = {"linux": "linux", "windows": "win32", "macos": "darwin"}[platform]
            machine = {"AMD64": "x86_64", "arm64": "aarch64"}.get(manifest["machine"], manifest["machine"])
            if manifest["platform"] != expected_platform or not name.endswith(f"-{machine}"):
                raise ValueError("runtime 的平台或架构与包名不匹配，不能跨平台复用 Python 依赖")
            launcher = binary.with_name(f"k3-separator{extension}")
            if not launcher.is_file():
                raise ValueError(f"缺少原生 worker 入口：{launcher}")
            shutil.copy2(launcher, root)
            (root / launcher.name).chmod(0o755)
            runtime_dir = root / "runtime"
            runtime_dir.mkdir()
            for component in ("python", "bin"):
                shutil.copytree(runtime / component, runtime_dir / component, symlinks=True)
            shutil.copytree(runtime / "models", root / "models")
            for filename in ("bundle-manifest.json", "requirements-resolved.txt"):
                shutil.copy2(runtime / filename, root)
            if platform in ("linux", "windows"):
                gui = gui_binary or binary.with_name(f"k3-gui{extension}")
                if not gui.is_file():
                    raise ValueError(f"缺少 GUI 二进制：{gui}")
                shutil.copy2(gui, root / f"k3-gui{extension}")
                (root / f"k3-gui{extension}").chmod(0o755)
                licenses = root / "licenses"
                licenses.mkdir()
                shutil.copy2(REPO / "crates/k3-gui/assets/fonts/OFL.txt",
                             licenses / "SourceHanSansCN-OFL.txt")
                shutil.copy2(REPO / "crates/k3-gui/assets/licenses/LicenseRef-Slint-Royalty-free-2.0.md",
                             licenses)
            if platform == "linux":
                shutil.copy2(REPO / "separate.sh", root)
                (root / "separate.sh").chmod(0o755)
                for source, destination in (
                    ("crates/k3-gui/assets/k3.desktop", "share/applications/k3.desktop"),
                    ("crates/k3-gui/assets/k3.svg", "share/icons/hicolor/scalable/apps/k3.svg"),
                ):
                    target = root / destination
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(REPO / source, target)
            if platform == "windows":
                for filename in ("separate.ps1", "install-separator.ps1"):
                    shutil.copy2(REPO / filename, root)
                shutil.copytree(REPO / "python/separator", root / "python/separator",
                                ignore=shutil.ignore_patterns("__pycache__", "*.pyc", "tests"))
        archive = dist / f"{name}{'.zip' if platform == 'windows' else '.tar.gz'}"
        write_archive(root, archive)
    print(f"已生成：{archive}（{archive.stat().st_size / 1024**2:.1f} MiB）")
    return archive


def main() -> None:
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("platform", choices=("linux", "windows", "macos"))
    parser.add_argument("name")
    parser.add_argument("binary", type=Path)
    parser.add_argument("runtime", type=Path, nargs="?")
    parser.add_argument("--cli-only", action="store_true")
    parser.add_argument("--gui-binary", type=Path, help="Linux/Windows 完整包的 GUI；默认取 CLI 同目录")
    parser.add_argument("--dist-dir", type=Path, default=Path(os.environ.get("DIST_DIR", REPO / "dist")))
    args = parser.parse_args()
    package(args.platform, args.name, args.binary, args.runtime, args.cli_only, args.dist_dir,
            args.gui_binary)


if __name__ == "__main__":
    main()
