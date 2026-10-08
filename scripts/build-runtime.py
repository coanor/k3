#!/usr/bin/env python3
"""Prepare a portable runtime; offline builds default to CPU, online installs detect hardware."""

from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
PYTHON_VERSION = "3.13.15"
sys.path.insert(0, str(REPO / "python/separator/src"))
from k3_separator.setup_runtime import install_torch


def run(*args: str | Path, **kwargs) -> subprocess.CompletedProcess:
    print("Running: ", " ".join(str(arg) for arg in args), flush=True)
    kwargs.setdefault("env", dict(os.environ, PYTHONUTF8="1"))
    return subprocess.run([str(arg) for arg in args], check=True, **kwargs)


def build(output: Path, model_cache: Path | None, models: list[str], backend: str = "cpu") -> None:
    if platform.system() == "Darwin" and platform.machine() == "x86_64":
        raise RuntimeError("Full runtime is unavailable on Intel macOS; build a CLI package or use Apple Silicon")
    if output.exists():
        raise RuntimeError(f"Output directory already exists; choose a new directory: {output}")
    uv = shutil.which("uv")
    if uv is None:
        raise RuntimeError("The build machine requires uv: https://docs.astral.sh/uv/")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="k3-runtime-", dir=output.parent) as directory:
        staging = Path(directory)
        downloads = staging / "downloads"
        run(uv, "python", "install", PYTHON_VERSION, "--install-dir", downloads,
            "--no-bin", "--no-registry")
        installations = list(downloads.glob(f"cpython-{PYTHON_VERSION}-*"))
        if len(installations) != 1:
            raise RuntimeError(f"Could not identify the standalone Python directory: {installations}")
        bundle = staging / "bundle"
        bundle.mkdir()
        python_root = bundle / "python"
        shutil.move(str(installations[0]), python_root)
        python = python_root / ("python.exe" if os.name == "nt" else "bin/python3")
        pip = (uv, "pip", "install", "--python", python, "--no-config", "--break-system-packages",
               "--link-mode", "copy")
        plan = install_torch(python, uv, backend)
        # 上游额外依赖包含不使用的 diffq；沿用项目已有的受控安装方式。
        run(*pip, "audio-separator==0.44.5", "--no-deps")
        constraints = staging / "constraints.txt"
        constraints.write_text("\n".join(plan.torch_packages) + "\n", encoding="utf-8")
        run(*pip, "-r", REPO / "python/separator/requirements-runtime.txt",
            "onnxruntime==1.24.4", "-c", constraints)
        run(*pip, REPO / "python/separator", "--no-deps")
        args = [python, "-s", "-m", "k3_separator.bundle", "--output", bundle]
        if model_cache:
            args += ["--model-cache", model_cache]
        for model in models:
            args += ["--model", model]
        run(*args)
        with (bundle / "requirements-resolved.txt").open("w", encoding="utf-8") as stream:
            run(uv, "pip", "freeze", "--python", python, stdout=stream)
        # 不打包写死构建目录的 console-script；实际入口由 Rust launcher 提供。
        scripts = python_root / ("Scripts" if os.name == "nt" else "bin")
        for entry in scripts.glob("k3-separator*"):
            entry.unlink()
        for cache in python_root.rglob("__pycache__"):
            shutil.rmtree(cache)
        manifest = json.loads((bundle / "bundle-manifest.json").read_text(encoding="utf-8"))
        manifest["platform"] = sys.platform
        manifest["machine"] = platform.machine()
        manifest["python"] = PYTHON_VERSION
        manifest["backend"] = plan.backend
        manifest["torch_build"] = plan.torch_build
        (bundle / "bundle-manifest.json").write_text(
            json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        shutil.move(str(bundle), output)
    print(f"Offline runtime created: {output}")


def main() -> None:
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="New runtime output directory")
    parser.add_argument("--model-cache", type=Path, help="Reuse an existing model cache with pinned checksum verification")
    parser.add_argument("--model", action="append", default=[],
                        help="Additional model ID; repeat to add models, or use all for every built-in model")
    parser.add_argument("--backend", choices=("auto", "cpu", "gpu"), default="cpu",
                        help="Use auto for hardware detection; CPU keeps offline packages portable")
    args = parser.parse_args()
    build(args.output.resolve(), args.model_cache, args.model, args.backend)


if __name__ == "__main__":
    main()
