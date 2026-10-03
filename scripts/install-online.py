#!/usr/bin/env python3
"""Install native programs, standalone Python and default models on the selected disk."""

from __future__ import annotations

import argparse
import ctypes.util
from functools import lru_cache
import hashlib
import json
import os
import platform
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import urllib.request
from pathlib import Path

SUPPORT = Path(__file__).resolve().parent.parent
CAPABILITIES = {
    ("linux", "x86_64"): (True, True), ("linux", "aarch64"): (True, True),
    ("windows", "x86_64"): (True, True), ("windows", "aarch64"): (False, False),
    ("macos", "x86_64"): (False, False), ("macos", "aarch64"): (False, True),
}


class HTTPSRedirect(urllib.request.HTTPRedirectHandler):
    """下载跳转只允许 HTTPS，访问 CDN 时移除仓库凭据。"""

    def redirect_request(self, request, response, code, message, headers, url):
        if not url.startswith("https://"):
            raise RuntimeError("Download redirects must use HTTPS")
        redirected = super().redirect_request(request, response, code, message, headers, url)
        if redirected is not None:
            redirected.remove_header("Authorization")
        return redirected


def request(url: str, accept: str = "application/vnd.github+json"):
    headers = {"User-Agent": "K3-online-installer", "Accept": accept}
    req = urllib.request.Request(url, headers=headers)
    token = os.environ.get("GITHUB_TOKEN")
    if token:
        if len(token) > 1024 or not re.fullmatch(r"[A-Za-z0-9_.-]+", token):
            raise ValueError("Invalid GITHUB_TOKEN format")
        req.add_unredirected_header("Authorization", "Bearer " + token)
    return urllib.request.build_opener(HTTPSRedirect()).open(req, timeout=60)


@lru_cache(maxsize=4)
def release_assets(endpoint: str) -> dict:
    with request(endpoint) as response:
        body = response.read(2 * 1024**2 + 1)
    if len(body) > 2 * 1024**2:
        raise ValueError("Release metadata exceeds the size limit")
    release = json.loads(body)
    if release.get("tag_name") != endpoint.rsplit("/", 1)[-1]:
        raise ValueError("Release version does not match the requested tag")
    return {entry["name"]: entry for entry in release["assets"]}


def fetch(name: str, destination: Path, base_url: str, assets: Path | None,
          maximum: int) -> None:
    if not re.fullmatch(r"[A-Za-z0-9_.-]+", name) or name in {".", ".."}:
        raise ValueError("Invalid release asset name")
    if assets:
        if (assets / name).stat().st_size > maximum:
            raise ValueError(f"Release asset exceeds the size limit: {name}")
        shutil.copyfile(assets / name, destination)
    else:
        entry = release_assets(base_url).get(name)
        if not entry:
            raise ValueError(f"Release is missing asset: {name}")
        url = entry["url"]
        if not url.startswith(base_url.split("/releases/")[0] + "/releases/assets/"):
            raise ValueError("Invalid release asset URL")
        with request(url, "application/octet-stream") as response:
            with destination.open("wb") as output:
                written = 0
                while chunk := response.read(min(1024**2, maximum - written + 1)):
                    written += len(chunk)
                    if written > maximum:
                        raise ValueError(f"Release asset exceeds the size limit: {name}")
                    output.write(chunk)


def download(name: str, destination: Path, base_url: str, assets: Path | None,
             expected: str | None = None, size: int | None = None) -> None:
    fetch(name + ".sha256", destination.with_name(destination.name + ".sha256"), base_url, assets, 4096)
    fields = destination.with_name(destination.name + ".sha256").read_text(encoding="ascii").split()
    if len(fields) != 2 or fields[1] != name or not re.fullmatch(r"[0-9a-f]{64}", fields[0]):
        raise ValueError(f"Invalid checksum file: {name}")
    if expected is not None and fields[0] != expected:
        raise ValueError(f"Manifest and checksum file do not match: {name}")
    fetch(name, destination, base_url, assets, size if size is not None else 2 * 1024**2)
    with destination.open("rb") as stream:
        actual = hashlib.file_digest(stream, "sha256").hexdigest()
    if actual != fields[0] or (size is not None and destination.stat().st_size != size):
        raise ValueError(f"Release asset integrity check failed: {name}")
    destination.with_name(destination.name + ".sha256").unlink()


def validate_manifest(manifest: dict, version: str, system: str, machine: str) -> None:
    gui, runtime = CAPABILITIES[system, machine]
    if (manifest.get("format"), manifest.get("version"), manifest.get("platform"),
            manifest.get("machine"), manifest.get("gui"), manifest.get("runtime")) != (
            1, version, system, machine, gui, runtime):
        raise ValueError("Release manifest version, platform, architecture or capabilities do not match")
    extension = ".exe" if system == "windows" else ""
    required = {"k3" + extension}
    if gui:
        required.add("k3-gui" + extension)
    if runtime:
        required.add("k3-separator" + extension)
    files = manifest.get("files", [])
    if len(files) != len(required) or {entry.get("path") for entry in files} != required:
        raise ValueError("Release manifest is missing required programs or contains unexpected paths")
    for entry in files:
        command = entry["path"].removesuffix(extension) if extension else entry["path"]
        if (entry.get("asset") != f"{command}-{system}-{machine}{extension}"
                or not isinstance(entry.get("size"), int) or not 0 < entry["size"] < 512 * 1024**2
                or not re.fullmatch(r"[0-9a-f]{64}", entry.get("sha256", ""))):
            raise ValueError("Invalid program file metadata")


def run(*args: str | Path, **kwargs) -> subprocess.CompletedProcess:
    return subprocess.run([str(arg) for arg in args], check=True, timeout=3600, **kwargs)


def check(root: Path, version: str, runtime: bool) -> None:
    extension = ".exe" if os.name == "nt" else ""
    binary = root / ("k3" + extension)
    environment = {name: value for name, value in os.environ.items()
                   if name not in {"GITHUB_TOKEN", "GH_TOKEN", "PYTHONHOME", "PYTHONPATH"}}
    actual = run(binary, "--version", text=True, capture_output=True, env=environment).stdout.strip()
    if actual != "k3 " + version:
        raise RuntimeError("Downloaded program version does not match the release manifest")
    run(binary, "--help", stdout=subprocess.DEVNULL, env=environment)
    if not runtime:
        return
    response = run(root / ("k3-separator" + extension), input='{"id":"install","method":"health"}\n',
                   text=True, encoding="utf-8", capture_output=True, env=environment)
    health = json.loads(response.stdout)
    status = health.get("result", {}).get("runtime", {})
    if not health.get("ok") or not all(status.get(key) for key in
                                       ("audio_separator_installed", "torch_installed", "ffmpeg")):
        raise RuntimeError("Separation runtime health check failed")
    python = root / ("runtime/python/python.exe" if os.name == "nt" else "runtime/python/bin/python3")
    run(python, "-I", SUPPORT / "scripts/check-runtime.py", root, env=environment)


def install(prefix: Path, repo: str, uv: Path, assets: Path | None,
            cache: Path | None = None) -> None:
    if not re.fullmatch(r"[A-Za-z0-9_-]+/[A-Za-z0-9_.-]+", repo):
        raise ValueError("Release repository must use owner/repo format")
    if prefix.is_symlink() or (prefix.exists() and (not prefix.is_dir() or any(prefix.iterdir()))):
        raise ValueError("Installation directory must be absent or empty; existing files cannot be overwritten")
    prefix = prefix.absolute()
    prefix.parent.mkdir(parents=True, exist_ok=True)
    system = {"Linux": "linux", "Windows": "windows", "Darwin": "macos"}[platform.system()]
    native_machine = platform.machine().lower()
    machine = {"amd64": "x86_64", "arm64": "aarch64"}.get(native_machine, native_machine)
    gui, runtime = CAPABILITIES[system, machine]
    if system == "linux":
        missing = [name for name in ("asound", "fontconfig", "EGL", "GL")
                   if not ctypes.util.find_library(name)]
        if missing:
            raise RuntimeError("Missing system libraries: " + ", ".join(missing)
                               + ". On Ubuntu 24.04, install libasound2t64 libfontconfig1 "
                               "libxkbcommon-x11-0 libegl1 libgl1-mesa-dri")
        if not ctypes.util.find_library("xkbcommon-x11"):
            print("Note: the X11 GUI also requires libxkbcommon-x11-0. Wayland and CLI installation can continue.", flush=True)
    minimum = (6 * 1024**3 if runtime else 128 * 1024**2)
    if shutil.disk_usage(prefix.parent).free < minimum:
        raise RuntimeError("Insufficient free space on the selected disk")
    version = json.loads((SUPPORT / "online-version.json").read_text())["version"]
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError("Invalid release version")
    base = f"https://api.github.com/repos/{repo}/releases/tags/v{version}"
    with tempfile.TemporaryDirectory(prefix=".k3-install-", dir=prefix.parent) as directory:
        work = Path(directory)
        root = work / "K3"
        root.mkdir()
        manifest_path = work / "manifest.json"
        download(f"k3-{system}-{machine}.json", manifest_path, base, assets)
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        validate_manifest(manifest, version, system, machine)
        for entry in manifest["files"]:
            print(f"Downloading program: {entry['path']}", flush=True)
            destination = root / entry["path"]
            download(entry["asset"], destination, base, assets, entry["sha256"], entry["size"])
            destination.chmod(0o755)
        shutil.copytree(SUPPORT / "docs", root / "docs")
        shutil.copy2(SUPPORT / "LICENSE", root / "LICENSE")
        if gui:
            licenses = root / "licenses"
            licenses.mkdir()
            shutil.copy2(SUPPORT / "crates/k3-gui/assets/fonts/OFL.txt", licenses / "SourceHanSansCN-OFL.txt")
            shutil.copy2(SUPPORT / "crates/k3-gui/assets/licenses/LicenseRef-Slint-Royalty-free-2.0.md", licenses)
            shutil.copy2(SUPPORT / "crates/k3-gui/assets/k3.svg", root / "k3.svg")
        if runtime:
            print("Preparing standalone Python, CPU separation dependencies, FFmpeg and default models", flush=True)
            environment = dict(os.environ, PATH=str(uv.parent) + os.pathsep + os.environ.get("PATH", ""),
                               UV_CACHE_DIR=str(work / "cache"), UV_NO_CONFIG="1",
                               TMPDIR=str(work / "tmp"), TMP=str(work / "tmp"), TEMP=str(work / "tmp"),
                               PYTHONUTF8="1", PYTHONNOUSERSITE="1")
            for credential in ("GITHUB_TOKEN", "GH_TOKEN"):
                environment.pop(credential, None)
            (work / "tmp").mkdir()
            args = [sys.executable, "-I", SUPPORT / "scripts/build-runtime.py", "--output", root / "runtime"]
            if cache:
                args += ["--model-cache", cache]
            run(*args, env=environment)
            for name in ("models", "bundle-manifest.json", "requirements-resolved.txt"):
                shutil.move(root / "runtime" / name, root / name)
            batch = "separate.ps1" if system == "windows" else "separate.sh"
            if system != "macos":
                shutil.copy2(SUPPORT / batch, root / batch)
                (root / batch).chmod(0o755)
        print("Checking program startup, separation runtime and offline model loading", flush=True)
        check(root, version, runtime)
        (root / "install-manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2),
                                                    encoding="utf-8")
        # 检查全部通过后才提交安装目录；失败不会覆盖旧安装和用户文件。
        if prefix.exists():
            prefix.rmdir()
        root.rename(prefix)
    print(f"Installation complete: {prefix}", flush=True)
    command = str(prefix / ('k3.exe' if system == 'windows' else 'k3'))
    command = "& '" + command.replace("'", "''") + "'" if system == "windows" else shlex.quote(command)
    print(f"CLI: {command} --help", flush=True)
    if gui:
        print(f"GUI: {prefix / ('k3-gui.exe' if system == 'windows' else 'k3-gui')}", flush=True)


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prefix", type=Path, required=True)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--uv", type=Path, required=True)
    parser.add_argument("--assets-dir", type=Path, help="Use local platform release assets with checksum verification")
    parser.add_argument("--model-cache", type=Path, help="Reuse downloaded models with integrity checks")
    args = parser.parse_args()
    try:
        install(args.prefix, args.repo, args.uv, args.assets_dir, args.model_cache)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"Installation failed: {error}", file=sys.stderr)
        sys.exit(1)
