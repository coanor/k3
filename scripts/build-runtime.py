#!/usr/bin/env python3
"""在目标平台准备可移动的离线 CPU runtime；构建机器需要 uv 和网络。"""

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


def run(*args: str | Path, **kwargs) -> subprocess.CompletedProcess:
    print("执行：", " ".join(str(arg) for arg in args), flush=True)
    return subprocess.run([str(arg) for arg in args], check=True, **kwargs)


def build(output: Path, model_cache: Path | None, models: list[str]) -> None:
    if platform.system() == "Darwin" and platform.machine() == "x86_64":
        raise RuntimeError("Intel macOS 不提供完整 runtime；请构建 CLI 包或使用 Apple Silicon")
    if output.exists():
        raise RuntimeError(f"输出目录已存在，请指定新目录：{output}")
    uv = shutil.which("uv")
    if uv is None:
        raise RuntimeError("构建机器需要 uv：https://docs.astral.sh/uv/")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="k3-runtime-", dir=output.parent) as directory:
        staging = Path(directory)
        downloads = staging / "downloads"
        run(uv, "python", "install", PYTHON_VERSION, "--install-dir", downloads,
            "--no-bin", "--no-registry")
        installations = list(downloads.glob(f"cpython-{PYTHON_VERSION}-*"))
        if len(installations) != 1:
            raise RuntimeError(f"无法确定独立 Python 目录：{installations}")
        bundle = staging / "bundle"
        bundle.mkdir()
        python_root = bundle / "python"
        shutil.move(str(installations[0]), python_root)
        python = python_root / ("python.exe" if os.name == "nt" else "bin/python3")
        pip = (uv, "pip", "install", "--python", python, "--no-config", "--break-system-packages",
               "--link-mode", "copy")
        torch_args = ["torch==2.11.0", "torchvision==0.26.0", "torchaudio==2.11.0"]
        if sys.platform != "darwin":
            torch_args += ["--index-url", "https://download.pytorch.org/whl/cpu"]
        run(*pip, *torch_args)
        # 上游额外依赖包含不使用的 diffq；沿用项目已有的受控安装方式。
        run(*pip, "audio-separator==0.44.5", "--no-deps")
        constraints = staging / "constraints.txt"
        constraints.write_text("torch==2.11.0\ntorchvision==0.26.0\ntorchaudio==2.11.0\n",
                               encoding="utf-8")
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
        (bundle / "bundle-manifest.json").write_text(
            json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        shutil.move(str(bundle), output)
    print(f"离线 runtime 已生成：{output}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="新建 runtime 输出目录")
    parser.add_argument("--model-cache", type=Path, help="复用已有模型缓存，仍会校验固定摘要")
    parser.add_argument("--model", action="append", default=[],
                        help="额外捆绑的模型 ID，可重复指定；all 表示全部内置模型")
    args = parser.parse_args()
    build(args.output.resolve(), args.model_cache, args.model)


if __name__ == "__main__":
    main()
