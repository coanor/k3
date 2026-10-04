#!/usr/bin/env python3
"""Generate native programs, platform manifests and shared online installation files."""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import tomllib
import zipfile
from pathlib import Path

from dist_manuals import MANUALS

REPO = Path(__file__).resolve().parent.parent
VERSION = tomllib.loads((REPO / "Cargo.toml").read_text())["workspace"]["package"]["version"]
CAPABILITIES = {
    ("linux", "x86_64"): (True, True),
    ("linux", "aarch64"): (True, True),
    ("windows", "x86_64"): (True, True),
    ("windows", "aarch64"): (False, False),
    ("macos", "x86_64"): (False, False),
    ("macos", "aarch64"): (False, True),
}


def checksum(path: Path) -> str:
    with path.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    path.with_name(path.name + ".sha256").write_text(
        f"{digest}  {path.name}\n", encoding="ascii")
    return digest


def support(output: Path) -> Path:
    output.mkdir(parents=True, exist_ok=True)
    archive = output / "k3-install-support.zip"
    paths = [REPO / "scripts" / name for name in
             ("install-online.py", "install_shortcuts.py", "install-shortcuts.ps1", "installation_state.py", "uninstall-online.py", "build-runtime.py", "check-runtime.py")]
    paths += [REPO / "docs" / name for name in MANUALS]
    paths += [REPO / "python/separator" / name for name in
              ("pyproject.toml", "README.md", "requirements-runtime.txt")]
    paths += sorted((REPO / "python/separator/src").rglob("*.py"))
    paths += [REPO / "crates/k3-gui/assets" / name for name in
              ("fonts/OFL.txt", "licenses/LicenseRef-Slint-Royalty-free-2.0.md", "k3.svg")]
    paths += [REPO / "separate.sh", REPO / "separate.ps1", REPO / "LICENSE"]
    paths += [REPO / f"{name}.{extension}" for name in ("install", "update", "uninstall")
              for extension in ("sh", "ps1")]
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as stream:
        for path in paths:
            stream.write(path, path.relative_to(REPO).as_posix())
        stream.writestr("online-version.json", json.dumps({"format": 1, "version": VERSION}))
    checksum(archive)
    for name in ("install.sh", "install.ps1", "update.sh", "update.ps1", "uninstall.sh", "uninstall.ps1"):
        target = output / name
        shutil.copy2(REPO / name, target)
        checksum(target)
    # ASCII 启动入口可被 PowerShell 5.1 的 irm 正确读取。
    template = (REPO / "scripts/windows-install-bootstrap.ps1.in").read_text(encoding="ascii")
    bootstrap = output / "get.ps1"
    bootstrap.write_text(template.replace("@K3_VERSION@", VERSION), encoding="ascii")
    checksum(bootstrap)
    return archive


def programs(platform: str, machine: str, binary: Path, output: Path,
             gui_binary: Path | None = None) -> Path:
    gui, runtime = CAPABILITIES[platform, machine]
    extension = ".exe" if platform == "windows" else ""
    version = subprocess.check_output([str(binary.resolve()), "--version"], text=True).strip()
    if version != f"k3 {VERSION}":
        raise ValueError(f"Native program version {version!r} does not match source version {VERSION!r}")
    sources = {"k3": binary}
    if runtime:
        sources["k3-separator"] = binary.with_name("k3-separator" + extension)
    if gui:
        sources["k3-gui"] = gui_binary or binary.with_name("k3-gui" + extension)
    if any(not path.is_file() for path in sources.values()):
        raise ValueError("Required platform CLI, GUI or separation launcher is missing")
    output.mkdir(parents=True, exist_ok=True)
    files = []
    for command, source in sources.items():
        target = output / f"{command}-{platform}-{machine}{extension}"
        shutil.copy2(source, target)
        files.append({"asset": target.name, "path": command + extension,
                      "size": target.stat().st_size, "sha256": checksum(target)})
    manifest = output / f"k3-{platform}-{machine}.json"
    manifest.write_text(json.dumps({"format": 1, "version": VERSION, "platform": platform,
                                   "machine": machine, "gui": gui, "runtime": runtime,
                                   "files": files}, ensure_ascii=False, indent=2) + "\n",
                        encoding="utf-8")
    checksum(manifest)
    return manifest


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    shared = commands.add_parser("support", help="Generate shared installation scripts and support files")
    shared.add_argument("--output", type=Path, default=REPO / "dist/online/support")
    native = commands.add_parser("programs", help="Generate standalone programs and manifest for the current platform")
    native.add_argument("platform", choices=("linux", "windows", "macos"))
    native.add_argument("machine", choices=("x86_64", "aarch64"))
    native.add_argument("binary", type=Path)
    native.add_argument("--gui-binary", type=Path)
    native.add_argument("--output", type=Path, default=REPO / "dist/online/platform")
    args = parser.parse_args()
    if args.command == "support":
        print(support(args.output))
    else:
        print(programs(args.platform, args.machine, args.binary, args.output, args.gui_binary))
