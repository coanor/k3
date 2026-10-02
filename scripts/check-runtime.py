#!/usr/bin/env python3
"""使用包内 Python 校验模型；禁止网络，防止遗漏元数据后偷偷补下载。"""

from __future__ import annotations

import argparse
import gc
import hashlib
import json
import os
import sys
from pathlib import Path


def deny_network(event, _args) -> None:
    if event in {"socket.connect", "socket.getaddrinfo"}:
        raise RuntimeError("离线检查禁止访问网络")


def check_artifacts(root: Path, artifacts: dict[str, str]) -> None:
    for name, expected in artifacts.items():
        name = name.replace("\\", "/")
        # manifest 路径在 runtime 构建阶段记录，发行包将 bin 放到 runtime 下。
        path = root / ("runtime/" + name if name.startswith("bin/") else name)
        with path.open("rb") as stream:
            actual = hashlib.file_digest(stream, "sha256").hexdigest()
        if actual != expected:
            raise RuntimeError(f"文件摘要不匹配：{name}")


def check(root: Path) -> None:
    manifest = json.loads((root / "bundle-manifest.json").read_text(encoding="utf-8"))
    check_artifacts(root, manifest["artifacts"])
    os.environ["PATH"] = str(root / "runtime/bin") + os.pathsep + os.environ.get("PATH", "")
    sys.addaudithook(deny_network)
    from audio_separator.separator import Separator
    from k3_separator.models import ModelRegistry
    from k3_separator.runtime import AudioSeparatorRuntime

    registry = ModelRegistry.load()
    runtime = AudioSeparatorRuntime(root / "models")
    status = runtime.status()
    if not status["audio_separator_installed"] or not status["torch_installed"] or not status["ffmpeg"]:
        raise RuntimeError(f"runtime 健康检查失败：{status}")
    for entry in manifest["models"]:
        model = registry.select(entry["profiles"][0], entry["id"])
        runtime._prepare_primary_artifact(model)
        separator = Separator(model_file_dir=str(root / "models"), output_format="WAV")
        separator.load_model(model_filename=model.filename)
        print(f"离线模型载入通过：{model.id}", flush=True)
        del separator
        gc.collect()


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    check(parser.parse_args().root.resolve())
