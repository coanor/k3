#!/usr/bin/env python3
"""校验已构建的便携包，并在目标平台生成系统安装包。"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import zipfile
from pathlib import Path, PurePosixPath

REPO = Path(__file__).resolve().parent.parent
PACKAGES = {
    "k3-linux-x86_64": ("linux", "x86_64", False),
    "k3-windows-x86_64": ("win32", "x86_64", False),
    "k3-macos-aarch64": ("darwin", "aarch64", False),
    "k3-macos-x86_64-cli": ("darwin", "x86_64", True),
}


def run(*args: str | Path) -> None:
    subprocess.run([str(arg) for arg in args], check=True)


def unpack(archive: Path, destination: Path) -> tuple[Path, str, bool]:
    suffix = ".zip" if archive.name.endswith(".zip") else ".tar.gz"
    name = archive.name.removesuffix(suffix)
    if name not in PACKAGES or suffix != (".zip" if PACKAGES[name][0] == "win32" else ".tar.gz"):
        raise ValueError(f"不支持的便携包：{archive.name}")
    with archive.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    if digest != archive.with_name(archive.name + ".sha256").read_text().split()[0]:
        raise ValueError("便携包 SHA-256 校验失败")
    if suffix == ".zip":
        with zipfile.ZipFile(archive) as stream:
            for member in stream.infolist():
                path = PurePosixPath(member.filename)
                if (path.is_absolute() or ".." in path.parts or "\\" in member.filename
                        or ":" in member.filename or not path.parts or path.parts[0] != name
                        or (member.external_attr >> 16) & 0o170000 == 0o120000):
                    raise ValueError(f"ZIP 中存在不安全的路径：{member.filename}")
            stream.extractall(destination)
    else:
        with tarfile.open(archive) as stream:
            stream.extractall(destination, filter="data")
    root = destination / name
    if list(destination.iterdir()) != [root] or not root.is_dir():
        raise ValueError("便携包必须包含唯一的预期顶级目录")
    platform, machine, cli_only = PACKAGES[name]
    extension = ".exe" if platform == "win32" else ""
    required = [f"k3{extension}"]
    if not cli_only:
        manifest = json.loads((root / "bundle-manifest.json").read_text(encoding="utf-8"))
        actual_machine = {"AMD64": "x86_64", "arm64": "aarch64"}.get(manifest["machine"], manifest["machine"])
        if (manifest["platform"], actual_machine) != (platform, machine):
            raise ValueError("便携包的 runtime 平台或架构与包名不一致")
        required += [f"k3-separator{extension}", "models", "runtime/bin",
                     "runtime/python/python.exe" if platform == "win32" else "runtime/python/bin/python3"]
        if platform in ("linux", "win32"):
            required += [f"k3-gui{extension}"]
    for path in required:
        if not (root / path).exists():
            raise ValueError(f"便携包缺少：{path}")
    return root, platform, cli_only


def unix_payload(root: Path, staging: Path, prefix: str, cli_only: bool) -> Path:
    """保留 bundle 的相对布局，并将命令入口链接到系统 PATH。"""
    payload = staging / prefix / "k3"
    payload.parent.mkdir(parents=True)
    shutil.move(root, payload)
    bin_dir = staging / ("usr/bin" if prefix == "opt" else "usr/local/bin")
    bin_dir.mkdir(parents=True)
    commands = ["k3"] if cli_only else ["k3", "k3-separator"]
    if (payload / "k3-gui").exists():
        commands += ["k3-gui"]
    for command in commands:
        entry = bin_dir / command
        if prefix == "opt":
            entry.symlink_to(os.path.relpath(payload / command, bin_dir))
        else:
            # macOS current_exe 可能返回符号链接路径；先执行包内真实路径。
            entry.write_text('#!/bin/sh\n'
                             'k3_package_dir=$(CDPATH= cd "$(dirname "$0")/../lib/k3" && pwd -P) || exit 1\n'
                             f'exec "$k3_package_dir/{command}" "$@"\n', encoding="utf-8")
            entry.chmod(0o755)
    # 系统安装目录由 root 管理；用户只能在自己的工程目录写入输出。
    for path in [staging, *staging.rglob("*")]:
        if not path.is_symlink():
            path.chmod(0o755 if path.is_dir() or path.stat().st_mode & 0o111 else 0o644)
    return payload


def deb(root: Path, staging: Path, output: Path, version: str) -> Path:
    payload = unix_payload(root, staging, "opt", False)
    for source in ("share/applications/k3.desktop", "share/icons/hicolor/scalable/apps/k3.svg"):
        target = staging / "usr" / source
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(payload / source, target)
    desktop = staging / "usr/share/applications/k3.desktop"
    desktop.write_text(desktop.read_text().replace("Comment=Karaoke project player",
                                                  "Comment=卡拉 OK 工程播放器"), encoding="utf-8")
    control = staging / "DEBIAN/control"
    control.parent.mkdir()
    size = sum(path.stat().st_size for path in staging.rglob("*") if path.is_file() and not path.is_symlink())
    control.write_text(
        f"Package: k3\nVersion: {version}\nArchitecture: amd64\n"
        "Maintainer: K3 contributors <noreply@github.com>\nSection: sound\nPriority: optional\n"
        f"Installed-Size: {(size + 1023) // 1024}\n"
        "Depends: libc6 (>= 2.39), libgcc-s1, libstdc++6, libasound2t64, libfontconfig1, libxkbcommon-x11-0, libegl1, libgl1\n"
        "Recommends: libgl1-mesa-dri\nHomepage: https://github.com/coanor/k3\n"
        "Description: K3 卡拉 OK 播放器与离线音源分离工具\n"
        " 包含 GUI、CLI、独立 Python 环境、FFmpeg 和三个默认分离模型。\n",
        encoding="utf-8")
    artifact = output / f"k3_{version}_amd64.deb"
    run("dpkg-deb", "--build", "--root-owner-group", "-Zzstd", "-z6", "--threads-max=2", staging, artifact)
    return artifact


def windows(root: Path, output: Path, version: str, compiler: str) -> Path:
    name = f"k3-{version}-windows-x86_64-setup"
    run(compiler, f"/DBundleDir={root}", f"/DOutputDir={output}",
        f"/DAppVersion={version}", f"/DOutputName={name}", REPO / "scripts/installers/windows.iss")
    return output / f"{name}.exe"


def macos(root: Path, staging: Path, output: Path, version: str, cli_only: bool) -> Path:
    unix_payload(root, staging, "usr/local/lib", cli_only)
    component = staging.parent / "component.pkg"
    identifier = "io.github.coanor.k3"
    run("pkgbuild", "--root", staging, "--identifier", identifier, "--version", version,
        "--install-location", "/", "--ownership", "recommended", component)
    architecture = "x86_64" if cli_only else "arm64"
    distribution = staging.parent / "distribution.xml"
    distribution.write_text(f'''<?xml version="1.0" encoding="utf-8"?>
<installer-gui-script minSpecVersion="2">
  <title>K3 {version}</title>
  <options customize="never" require-scripts="false" hostArchitectures="{architecture}"/>
  <domains enable_anywhere="false" enable_currentUserHome="false" enable_localSystem="true"/>
  <volume-check><allowed-os-versions><os-version min="14.0"/></allowed-os-versions></volume-check>
  <choices-outline><line choice="k3"/></choices-outline>
  <choice id="k3" visible="false" title="K3 命令行工具" description="安装 K3 程序{'与离线分离环境' if not cli_only else '（Intel 仅 CLI）'}">
    <pkg-ref id="{identifier}"/>
  </choice>
  <pkg-ref id="{identifier}" version="{version}">component.pkg</pkg-ref>
</installer-gui-script>
''', encoding="utf-8")
    name = "macos-x86_64-cli" if cli_only else "macos-aarch64"
    artifact = output / f"k3-{version}-{name}.pkg"
    run("productbuild", "--distribution", distribution, "--package-path", staging.parent, artifact)
    return artifact


def build(archive: Path, output: Path, version: str, compiler: str, refresh_worker: bool = False) -> Path:
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError("安装包版本必须是 major.minor.patch 三段数字")
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="k3-installer-", dir=output) as directory:
        work = Path(directory)
        unpacked = work / "unpacked"
        unpacked.mkdir()
        root, platform, cli_only = unpack(archive.resolve(), unpacked)
        if platform != sys.platform:
            raise ValueError("安装包必须在对应操作系统上构建")
        binary = root / ("k3.exe" if platform == "win32" else "k3")
        actual = subprocess.check_output([str(binary), "--version"], text=True).strip()
        if actual != f"k3 {version}":
            raise ValueError(f"二进制版本 {actual!r} 与安装包版本 {version!r} 不一致")
        if refresh_worker and not cli_only:
            python = root / ("runtime/python/python.exe" if platform == "win32" else "runtime/python/bin/python3")
            run("uv", "pip", "install", "--python", python, "--no-config", "--break-system-packages", "--no-deps", "--reinstall", "--link-mode=copy",
                REPO / "python/separator")
        # 安装说明跟随安装器源码，程序/runtime/模型保持便携包中的版本。
        shutil.copy2(REPO / "docs/install-packages.md", root / "INSTALL-PACKAGE.md")
        if platform == "linux":
            artifact = deb(root, work / "payload", output, version)
        elif platform == "win32":
            artifact = windows(root, output, version, compiler)
        else:
            artifact = macos(root, work / "payload", output, version, cli_only)
    if artifact.stat().st_size >= 2 * 1024**3:
        raise ValueError("安装包超过 GitHub Release 的 2 GiB 限制")
    with artifact.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    artifact.with_name(artifact.name + ".sha256").write_text(f"{digest}  {artifact.name}\n", encoding="ascii")
    print(f"已生成安装包：{artifact}（{artifact.stat().st_size / 1024**2:.1f} MiB）", flush=True)
    return artifact


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("--output-dir", type=Path, default=REPO / "dist/installers")
    parser.add_argument("--version", default=tomllib.loads((REPO / "Cargo.toml").read_text())["workspace"]["package"]["version"])
    parser.add_argument("--iscc", default="ISCC.exe", help="Windows Inno Setup 编译器路径")
    parser.add_argument("--refresh-worker", action="store_true", help="用当前源码重新安装 worker；保留原生程序、其他依赖和模型，需要 uv")
    args = parser.parse_args()
    build(args.archive, args.output_dir, args.version, args.iscc, args.refresh_worker)
