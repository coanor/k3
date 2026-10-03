"""Prepare release models, configuration, FFmpeg and third-party component metadata."""

from __future__ import annotations

import argparse
import importlib.metadata
import json
import os
import shutil
from pathlib import Path

from .models import BACKING_VOCALS_MODEL_ID, ModelRegistry
from .runtime import AudioSeparatorRuntime, _hash_file


def prepare(output: Path, cache: Path | None, extra_models: list[str]) -> None:
    import imageio_ffmpeg
    from audio_separator.separator import Separator

    registry = ModelRegistry.load()
    selected = {registry.select(profile).id for profile in ("fast", "balanced", "quality")}
    selected.add(BACKING_VOCALS_MODEL_ID)
    selected.update(model["id"] for model in registry.list()
                    if "all" in extra_models or model["id"] in extra_models)
    unknown = set(extra_models) - {model["id"] for model in registry.list()} - {"all"}
    if unknown:
        raise ValueError(f"Unknown models: {', '.join(sorted(unknown))}")
    models = [registry.select(entry["profiles"][0], entry["id"])
              for entry in registry.list() if entry["id"] in selected]
    model_dir = output / "models"
    model_dir.mkdir(parents=True)
    if cache:
        filenames = {model.filename for model in models}
        for path in cache.iterdir():
            if path.is_file() and (path.name in filenames or path.suffix in {".json", ".yaml"}):
                shutil.copy2(path, model_dir / path.name)
    bin_dir = output / "bin"
    bin_dir.mkdir()
    source = Path(imageio_ffmpeg.get_ffmpeg_exe())
    ffmpeg = bin_dir / ("ffmpeg.exe" if source.suffix == ".exe" else "ffmpeg")
    shutil.copy2(source, ffmpeg)
    ffmpeg.chmod(0o755)
    os.environ["PATH"] = str(bin_dir) + os.pathsep + os.environ.get("PATH", "")
    runtime = AudioSeparatorRuntime(model_dir)
    separator = Separator(model_file_dir=str(model_dir), output_format="WAV")
    for model in models:
        print(f"Preparing model: {model.id}", flush=True)
        runtime._prepare_primary_artifact(model)
        separator.download_model_and_data(model.filename)
        # 在打包前拒绝上游下载失败残留的空文件。
        if not (model_dir / model.filename).stat().st_size:
            raise ValueError(f"Model file is empty: {model.filename}")
    artifacts = {path.relative_to(output).as_posix(): _hash_file(path)
                 for path in sorted(model_dir.rglob("*")) if path.is_file()}
    artifacts[ffmpeg.relative_to(output).as_posix()] = _hash_file(ffmpeg)
    packages = []
    for distribution in importlib.metadata.distributions():
        metadata = distribution.metadata
        packages.append({"name": metadata["Name"], "version": distribution.version,
                         "license": metadata.get("License-Expression") or metadata.get("License"),
                         "homepage": metadata.get("Home-page")})
    manifest = {"format": 1, "backend": "cpu", "models": [m.public_dict() for m in models],
                "artifacts": artifacts, "packages": sorted(packages, key=lambda p: p["name"].lower())}
    (output / "bundle-manifest.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--model-cache", type=Path)
    parser.add_argument("--model", action="append", default=[])
    args = parser.parse_args()
    prepare(args.output, args.model_cache, args.model)


if __name__ == "__main__":
    main()
