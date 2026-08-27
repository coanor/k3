"""Inference worker, audio validation, and deterministic test audio helpers."""

from __future__ import annotations

import importlib.metadata
import json
import platform
import struct
import subprocess
from pathlib import Path
from typing import Any

from fastapi import status

from .server_contracts import api_error


def execute_inference(config: dict[str, Any]) -> dict[str, Any]:
    """Run one model request without touching SQLite or publishing artifacts."""

    from .models import ModelRegistry
    from .runtime import AudioSeparatorRuntime
    from .service import SeparationService

    registry_path = (
        Path(config["registry_path"]) if config["registry_path"] is not None else None
    )
    if config["backend"] == "cuda":
        import torch

        torch.cuda.reset_peak_memory_stats()
    service = SeparationService(
        ModelRegistry.load(registry_path),
        AudioSeparatorRuntime(Path(config["model_dir"]), backend=config["backend"]),
    )
    result = service.handle(
        {
            "method": "separate",
            "params": {
                "input_path": config["input_path"],
                "output_dir": config["output_dir"],
                "profile": config["profile"],
                "model_id": config["model_id"],
                "overwrite": True,
                "preserve_backing_vocals": config["preserve_backing_vocals"],
                "options": {"autocast": config["backend"] == "cuda"},
            },
        }
    )
    if config["backend"] == "cuda":
        result["_worker_metrics"] = {
            "peak_cuda_bytes": int(torch.cuda.max_memory_allocated())
        }
    return result


def progress_message(stage: str) -> str:
    return stage.replace("_", " ").capitalize()


def runtime_fingerprint(backend: str) -> dict[str, Any]:
    versions: dict[str, str | None] = {}
    for distribution in ("audio-separator", "torch", "onnxruntime"):
        try:
            versions[distribution] = importlib.metadata.version(distribution)
        except importlib.metadata.PackageNotFoundError:
            versions[distribution] = None
    return {
        "k3_separator": "0.1.0",
        "python": platform.python_version(),
        "dependencies": versions,
        "precision": "float16" if backend == "cuda" else "float32",
    }


def probe_audio(path: Path, max_duration_seconds: int, max_channels: int) -> None:
    try:
        completed = subprocess.run(
            [
                "ffprobe",
                "-v",
                "error",
                "-show_entries",
                "format=duration:stream=codec_type,channels",
                "-of",
                "json",
                str(path),
            ],
            check=True,
            capture_output=True,
            text=True,
            timeout=30,
        )
        document = json.loads(completed.stdout)
        duration = float(document.get("format", {}).get("duration", 0))
        audio_streams = [
            stream
            for stream in document.get("streams", [])
            if stream.get("codec_type") == "audio"
        ]
        channels = max(
            (int(stream.get("channels", 0)) for stream in audio_streams), default=0
        )
    except (
        OSError,
        ValueError,
        json.JSONDecodeError,
        subprocess.SubprocessError,
    ) as error:
        raise api_error(
            status.HTTP_422_UNPROCESSABLE_CONTENT,
            "input_rejected",
            "uploaded content is not a supported audio file",
        ) from error
    if not audio_streams or duration <= 0:
        raise api_error(
            status.HTTP_422_UNPROCESSABLE_CONTENT,
            "input_rejected",
            "uploaded content has no usable audio stream",
        )
    if duration > max_duration_seconds:
        raise api_error(
            status.HTTP_422_UNPROCESSABLE_CONTENT,
            "input_rejected",
            "uploaded audio exceeds the configured duration limit",
        )
    if channels > max_channels:
        raise api_error(
            status.HTTP_422_UNPROCESSABLE_CONTENT,
            "input_rejected",
            "uploaded audio exceeds the configured channel limit",
        )


def validate_output_wavs(role_paths: dict[str, Path]) -> None:
    try:
        import numpy as np
        import soundfile as sf
    except ImportError as error:
        raise RuntimeError(
            "numpy and soundfile are required to validate separator outputs"
        ) from error

    expected_frames: int | None = None
    for role, path in role_paths.items():
        try:
            with sf.SoundFile(path) as audio:
                valid_format = (
                    audio.format == "WAV"
                    and audio.subtype == "FLOAT"
                    and audio.samplerate == 44_100
                    and audio.channels == 2
                    and audio.frames > 0
                )
                if not valid_format:
                    raise RuntimeError(
                        f"{role} must be a non-empty 44.1 kHz stereo 32-bit float WAV"
                    )
                frames = int(audio.frames)
                for block in audio.blocks(blocksize=65_536, dtype="float32"):
                    if not np.isfinite(block).all():
                        raise RuntimeError(f"{role} contains non-finite samples")
        except (OSError, sf.LibsndfileError) as error:
            raise RuntimeError(f"{role} is not a readable WAV: {error}") from error
        if expected_frames is not None and frames != expected_frames:
            raise RuntimeError("separator outputs have different frame counts")
        expected_frames = frames


def fake_float_wav(level: float) -> bytes:
    channels = 2
    sample_rate = 44_100
    frames = 441
    samples = struct.pack(f"<{frames * channels}f", *([level] * frames * channels))
    byte_rate = sample_rate * channels * 4
    block_align = channels * 4
    header = struct.pack(
        "<4sI4s4sIHHIIHH4sI",
        b"RIFF",
        36 + len(samples),
        b"WAVE",
        b"fmt ",
        16,
        3,
        channels,
        sample_rate,
        byte_rate,
        block_align,
        32,
        b"data",
        len(samples),
    )
    return header + samples
