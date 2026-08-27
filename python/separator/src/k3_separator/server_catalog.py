"""Model discovery, readiness, and immutable job-spec resolution."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any

from fastapi import status

from .server_contracts import JobRequest, api_error
from .server_runtime import runtime_fingerprint
from .server_utils import hash_file


@dataclass(frozen=True)
class CatalogConfig:
    backend: str
    data_dir: Path
    model_dir: Path | None
    registry_path: Path | None


class ModelCatalog:
    """Resolves public model names into validated, reproducible execution specs."""

    def __init__(self, config: CatalogConfig) -> None:
        self._config = config

    def models(self) -> list[dict[str, Any]]:
        if self._config.backend == "fake":
            return [
                {
                    "id": "fake-separator",
                    "display_name": "Fake separator",
                    "provider": "k3-test",
                    "architecture": "fake",
                    "checkpoint_sha256": "0" * 64,
                    "status": "ready",
                    "backends": ["fake"],
                    "presets": ["fast", "balanced", "quality"],
                    "output_layouts": ["two_stem", "karaoke"],
                }
            ]
        from .models import ModelRegistry

        return [
            {
                **model,
                "display_name": model["id"],
                "checkpoint_sha256": model.get("expected_sha256"),
                "status": self._model_status(model),
                "backends": [self._config.backend],
                "presets": model["profiles"],
                "output_layouts": ["two_stem", "karaoke"],
            }
            for model in ModelRegistry.load(self._config.registry_path).list()
        ]

    def readiness(self) -> dict[str, Any]:
        if self._config.backend == "fake":
            return {"status": "ready", "backend": "fake"}
        from .runtime import AudioSeparatorRuntime

        runtime_status = AudioSeparatorRuntime(
            self._models_dir(), backend=self._config.backend
        ).status()
        if not runtime_status["audio_separator_installed"]:
            raise api_error(
                status.HTTP_503_SERVICE_UNAVAILABLE,
                "runtime_unavailable",
                "audio-separator runtime is unavailable",
                retryable=True,
            )
        return {
            "status": "ready",
            "backend": self._config.backend,
            "device": runtime_status["device"],
        }

    def resolve(self, request: JobRequest) -> dict[str, Any]:
        if self._config.backend == "fake":
            if request.model_id != "fake-separator":
                raise api_error(
                    status.HTTP_404_NOT_FOUND,
                    "model_not_found",
                    "model not found",
                )
            return self._fake_spec(request)

        from .errors import WorkerError
        from .models import BACKING_VOCALS_MODEL_ID, ModelRegistry

        try:
            registry = ModelRegistry.load(self._config.registry_path)
            primary = registry.select(request.preset, request.model_id)
            backing = (
                registry.select("fast", BACKING_VOCALS_MODEL_ID)
                if request.output_layout == "karaoke"
                else None
            )
        except WorkerError as error:
            status_code = (
                status.HTTP_404_NOT_FOUND
                if error.code == "model_not_found"
                else status.HTTP_422_UNPROCESSABLE_CONTENT
            )
            error_code = (
                "model_not_found"
                if error.code == "model_not_found"
                else "unsupported_preset"
            )
            raise api_error(status_code, error_code, error.message) from error
        self._require_ready(primary.public_dict(), primary.id)
        if backing is not None:
            self._require_ready(backing.public_dict(), backing.id)
        return {
            "primary": {
                "model_id": primary.id,
                "checkpoint_sha256": self._checkpoint_identity(primary.public_dict()),
                "filename": primary.filename,
                "options": primary.options_for(request.preset),
            },
            "backing_vocals": (
                {
                    "model_id": backing.id,
                    "checkpoint_sha256": self._checkpoint_identity(
                        backing.public_dict()
                    ),
                    "filename": backing.filename,
                    "options": backing.options_for("fast"),
                }
                if backing is not None
                else None
            ),
            "backend": self._config.backend,
            "output_layout": request.output_layout,
            "pipeline_version": 1,
            "runtime": runtime_fingerprint(self._config.backend),
            "seed": 0,
        }

    def _fake_spec(self, request: JobRequest) -> dict[str, Any]:
        return {
            "primary": {
                "model_id": request.model_id,
                "checkpoint_sha256": "0" * 64,
                "options": {"preset": request.preset},
            },
            "backing_vocals": (
                {
                    "model_id": "fake-backing-vocals",
                    "checkpoint_sha256": "1" * 64,
                    "options": {},
                }
                if request.output_layout == "karaoke"
                else None
            ),
            "backend": self._config.backend,
            "output_layout": request.output_layout,
            "pipeline_version": 1,
            "runtime": runtime_fingerprint(self._config.backend),
            "seed": 0,
        }

    def _models_dir(self) -> Path:
        return self._config.model_dir or (self._config.data_dir / "models")

    def _model_status(self, model: dict[str, Any]) -> str:
        checkpoint = self._models_dir().expanduser().resolve() / model["filename"]
        if not checkpoint.is_file():
            return "not_installed"
        expected = model.get("expected_sha256")
        if expected is None:
            return "ready"
        return "ready" if hash_file(checkpoint) == expected else "invalid"

    def _checkpoint_identity(self, model: dict[str, Any]) -> str | None:
        expected = model.get("expected_sha256")
        if expected is not None:
            return str(expected)
        checkpoint = self._models_dir().expanduser().resolve() / model["filename"]
        return hash_file(checkpoint) if checkpoint.is_file() else None

    def _require_ready(self, model: dict[str, Any], model_id: str) -> None:
        model_status = self._model_status(model)
        if model_status != "ready":
            raise api_error(
                status.HTTP_409_CONFLICT,
                "model_not_installed",
                f"model is {model_status}: {model_id}",
            )
