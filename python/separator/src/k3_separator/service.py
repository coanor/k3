"""Protocol-independent worker behaviour."""

from __future__ import annotations

import os
import shutil
import tempfile
from pathlib import Path
from typing import Any

from .errors import WorkerError
from .models import BACKING_VOCALS_MODEL_ID, ModelRegistry
from .runtime import SeparationRuntime

ALLOWED_OPTIONS = {
    "segment_size",
    "overlap",
    "batch_size",
    "autocast",
    "pitch_shift",
    "shifts",
    "hop_length",
    "enable_denoise",
}


class SeparationService:
    """Deep module exposing model discovery, health, and atomic two-stem separation."""

    def __init__(self, registry: ModelRegistry, runtime: SeparationRuntime) -> None:
        self._registry = registry
        self._runtime = runtime

    def handle(self, request: Any) -> dict[str, Any]:
        if not isinstance(request, dict):
            raise WorkerError("invalid_request", "request must be a JSON object")
        method = request.get("method")
        params = request.get("params", {})
        if not isinstance(params, dict):
            raise WorkerError("invalid_request", "params must be a JSON object")
        if method == "health":
            return {"protocol_version": 1, "runtime": self._runtime.status()}
        if method == "list_models":
            return {"models": self._registry.list()}
        if method == "separate":
            return self._separate(params)
        raise WorkerError("method_not_found", f"unknown method: {method}")

    def _separate(self, params: dict[str, Any]) -> dict[str, Any]:
        input_path = _required_path(params, "input_path")
        if not input_path.is_file():
            raise WorkerError("input_not_found", f"input is not a file: {input_path}")
        output_dir = _required_path(params, "output_dir")
        profile = params.get("profile", "balanced")
        model_id = params.get("model_id")
        if not isinstance(profile, str) or (model_id is not None and not isinstance(model_id, str)):
            raise WorkerError("invalid_request", "profile and model_id must be strings")
        options = _validated_options(params.get("options", {}))
        overwrite = params.get("overwrite", False)
        if not isinstance(overwrite, bool):
            raise WorkerError("invalid_request", "overwrite must be a boolean")
        preserve_backing_vocals = params.get("preserve_backing_vocals", True)
        if not isinstance(preserve_backing_vocals, bool):
            raise WorkerError(
                "invalid_request", "preserve_backing_vocals must be a boolean"
            )

        model = self._registry.select(profile, model_id)
        backing_vocals_model = (
            self._registry.select("fast", BACKING_VOCALS_MODEL_ID)
            if preserve_backing_vocals
            else None
        )
        output_dir.mkdir(parents=True, exist_ok=True)
        destinations = {
            "vocals": output_dir / "vocals.wav",
            "accompaniment": output_dir / "accompaniment.wav",
        }
        if preserve_backing_vocals:
            destinations["backing_vocals"] = output_dir / "backing-vocals.wav"
        existing = [str(path) for path in destinations.values() if path.exists()]
        if existing and not overwrite:
            raise WorkerError("output_exists", f"refusing to overwrite: {', '.join(existing)}")

        scratch = Path(tempfile.mkdtemp(prefix=".k3-separate-", dir=output_dir))
        moved: list[Path] = []
        try:
            result = self._runtime.separate(
                input_path, scratch, model, options, backing_vocals_model
            )
            if (
                backing_vocals_model is not None
                and result.backing_vocals_checkpoint_sha256 is None
            ):
                raise WorkerError(
                    "separation_failed", "runtime omitted backing-vocal model provenance"
                )
            sources = {
                "vocals": result.vocals,
                "accompaniment": result.accompaniment,
            }
            if preserve_backing_vocals:
                if result.backing_vocals is None:
                    raise WorkerError(
                        "separation_failed", "runtime did not produce backing vocals"
                    )
                sources["backing_vocals"] = result.backing_vocals
            for name, destination in destinations.items():
                source = sources[name]
                if not source.is_file() or source.stat().st_size == 0:
                    raise WorkerError("separation_failed", f"missing output: {source}")
                os.replace(source, destination)
                moved.append(destination)
        except Exception:
            if not overwrite:
                for path in moved:
                    path.unlink(missing_ok=True)
            raise
        finally:
            shutil.rmtree(scratch, ignore_errors=True)

        provenance = {
            "provider": model.provider,
            "architecture": model.architecture,
            "checkpoint_id": model.id,
            "checkpoint_sha256": result.checkpoint_sha256,
            "profile": profile,
            "license": model.license,
            "source_url": model.source_url,
            "runtime_options": {**model.runtime_options, **options},
        }
        if backing_vocals_model is not None:
            provenance["backing_vocals_model"] = {
                "provider": backing_vocals_model.provider,
                "architecture": backing_vocals_model.architecture,
                "checkpoint_id": backing_vocals_model.id,
                "checkpoint_sha256": result.backing_vocals_checkpoint_sha256,
                "license": backing_vocals_model.license,
                "source_url": backing_vocals_model.source_url,
                "runtime_options": {
                    **backing_vocals_model.runtime_options,
                    **options,
                },
            }
        response = {
            "vocals": str(destinations["vocals"]),
            "accompaniment": str(destinations["accompaniment"]),
            "provenance": provenance,
        }
        if preserve_backing_vocals:
            response["backing_vocals"] = str(destinations["backing_vocals"])
        return response


def _required_path(params: dict[str, Any], name: str) -> Path:
    value = params.get(name)
    if not isinstance(value, str) or not value.strip():
        raise WorkerError("invalid_request", f"{name} must be a non-empty string")
    return Path(value).expanduser().resolve()


def _validated_options(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise WorkerError("invalid_request", "options must be a JSON object")
    unknown = sorted(set(value) - ALLOWED_OPTIONS)
    if unknown:
        raise WorkerError("invalid_request", f"unknown options: {', '.join(unknown)}")
    if "batch_size" in value and (
        not isinstance(value["batch_size"], int)
        or isinstance(value["batch_size"], bool)
        or not 1 <= value["batch_size"] <= 16
    ):
        raise WorkerError("invalid_request", "batch_size must be an integer from 1 to 16")
    if "segment_size" in value and (
        not isinstance(value["segment_size"], int)
        or isinstance(value["segment_size"], bool)
        or not 1 <= value["segment_size"] <= 4096
    ):
        raise WorkerError("invalid_request", "segment_size must be an integer from 1 to 4096")
    for name in ("autocast", "enable_denoise"):
        if name in value and not isinstance(value[name], bool):
            raise WorkerError("invalid_request", f"{name} must be a boolean")
    return dict(value)
