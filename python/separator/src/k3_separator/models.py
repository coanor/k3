"""Validated model registry and profile selection."""

from __future__ import annotations

import json
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

from .errors import WorkerError

PROFILES = ("fast", "balanced", "quality", "compatible")
BACKING_VOCALS_MODEL_ID = "uvr-mdx-karaoke-2"


@dataclass(frozen=True)
class SeparationModel:
    """Everything needed to select and audit one model checkpoint."""

    id: str
    filename: str
    architecture: str
    profiles: tuple[str, ...]
    output_stems: tuple[str, ...] = ("Vocals", "Instrumental")
    provider: str = "audio-separator"
    license: str = "NOASSERTION"
    source_url: str = ""
    download_url: str | None = None
    expected_sha256: str | None = None
    artifact_globs: tuple[str, ...] = ()
    runtime_options: dict[str, Any] = field(default_factory=dict)

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> SeparationModel:
        required = ("id", "filename", "architecture", "profiles")
        missing = [name for name in required if not value.get(name)]
        if missing:
            raise WorkerError(
                "invalid_registry", f"model is missing fields: {', '.join(missing)}"
            )
        filename = str(value["filename"])
        if Path(filename).name != filename:
            raise WorkerError("invalid_registry", "filename must not contain directories")
        profiles = tuple(value["profiles"])
        unknown = sorted(set(profiles) - set(PROFILES))
        if unknown:
            raise WorkerError(
                "invalid_registry", f"unknown profiles: {', '.join(unknown)}"
            )
        digest = value.get("expected_sha256")
        if digest is not None and (
            not isinstance(digest, str)
            or len(digest) != 64
            or not all(character in "0123456789abcdefABCDEF" for character in digest)
        ):
            raise WorkerError("invalid_registry", "expected_sha256 must be 64 hex characters")
        stems = tuple(value.get("output_stems", ("Vocals", "Instrumental")))
        if "Vocals" not in stems or len(stems) < 2:
            raise WorkerError(
                "invalid_registry", "output_stems must contain Vocals and another stem"
            )
        return cls(
            id=str(value["id"]),
            filename=filename,
            architecture=str(value["architecture"]),
            profiles=profiles,
            output_stems=stems,
            provider=str(value.get("provider", "audio-separator")),
            license=str(value.get("license", "NOASSERTION")),
            source_url=str(value.get("source_url", "")),
            download_url=(
                str(value["download_url"])
                if value.get("download_url") is not None
                else None
            ),
            expected_sha256=digest.lower() if digest else None,
            artifact_globs=tuple(value.get("artifact_globs", ())),
            runtime_options=dict(value.get("runtime_options", {})),
        )

    def public_dict(self) -> dict[str, Any]:
        value = asdict(self)
        value.pop("artifact_globs")
        value["profiles"] = list(self.profiles)
        value["output_stems"] = list(self.output_stems)
        return value


BUILTIN_MODELS = (
    SeparationModel(
        id="uvr-mdx-karaoke-2",
        filename="UVR_MDXNET_KARA_2.onnx",
        architecture="mdx-net",
        profiles=("fast",),
        license="NOASSERTION",
        source_url="https://github.com/Anjok07/ultimatevocalremovergui",
        download_url=(
            "https://github.com/TRvlvr/model_repo/releases/download/all_public_uvr_models/"
            "UVR_MDXNET_KARA_2.onnx"
        ),
        expected_sha256="bf32e15105a09c0f7dddd2b67346146334d6f3ecb399ed7638eba2ab07cbf5f4",
        runtime_options={"segment_size": 256, "overlap": 0.25, "batch_size": 1},
    ),
    SeparationModel(
        id="uvr-mdx-inst-hq-3",
        filename="UVR-MDX-NET-Inst_HQ_3.onnx",
        architecture="mdx-net",
        profiles=("balanced",),
        license="NOASSERTION",
        source_url="https://github.com/Anjok07/ultimatevocalremovergui",
        download_url=(
            "https://github.com/TRvlvr/model_repo/releases/download/all_public_uvr_models/"
            "UVR-MDX-NET-Inst_HQ_3.onnx"
        ),
        expected_sha256="317554b07fe1ea5279a77f2b1520a41ea4b93432560c4ffd08792c30fddf9adc",
        runtime_options={"segment_size": 256, "overlap": 0.25, "batch_size": 1},
    ),
    SeparationModel(
        id="bs-roformer-viperx-1297",
        filename="model_bs_roformer_ep_317_sdr_12.9755.ckpt",
        architecture="bs-roformer",
        profiles=("quality",),
        license="MIT",
        source_url="https://huggingface.co/Politrees/UVR_resources",
        download_url=(
            "https://github.com/TRvlvr/model_repo/releases/download/all_public_uvr_models/"
            "model_bs_roformer_ep_317_sdr_12.9755.ckpt"
        ),
        expected_sha256="5b84f37e8d444c8cb30c79d77f613a41c05868ff9c9ac6c7049c00aefae115aa",
        runtime_options={"segment_size": 256, "overlap": 8, "batch_size": 1},
    ),
    SeparationModel(
        id="mel-band-roformer-kim-vocal-2",
        filename="vocals_mel_band_roformer.ckpt",
        architecture="mel-band-roformer",
        profiles=("quality",),
        output_stems=("Vocals", "Other"),
        license="MIT",
        source_url="https://huggingface.co/KimberleyJSN/melbandroformer",
        download_url=(
            "https://huggingface.co/KimberleyJSN/melbandroformer/resolve/"
            "ac9b0614ab3cd7f77219e18ba494dfd93956c348/MelBandRoformer.ckpt"
        ),
        expected_sha256="87201f4d31afb5bc79993230fc49446918425574db48c01c405e44f365c7559e",
        runtime_options={"segment_size": 256, "overlap": 8, "batch_size": 1},
    ),
    SeparationModel(
        id="htdemucs-ft",
        filename="htdemucs_ft.yaml",
        architecture="htdemucs",
        profiles=("compatible",),
        output_stems=("Vocals", "Drums", "Bass", "Other"),
        license="MIT",
        source_url="https://github.com/facebookresearch/demucs",
        artifact_globs=(
            "**/f7e0c4bc*.th",
            "**/d12395a8*.th",
            "**/92cfc3b6*.th",
            "**/04573f0d*.th",
        ),
        runtime_options={"segment_size": 10, "overlap": 0.25, "shifts": 1},
    ),
)


class ModelRegistry:
    """Selects named models while hiding checkpoint filenames from callers."""

    def __init__(self, models: tuple[SeparationModel, ...] = BUILTIN_MODELS) -> None:
        self._models = {model.id: model for model in models}
        if len(self._models) != len(models):
            raise WorkerError("invalid_registry", "model IDs must be unique")

    @classmethod
    def load(cls, path: Path | None = None) -> ModelRegistry:
        models = {model.id: model for model in BUILTIN_MODELS}
        if path is not None:
            try:
                document = json.loads(path.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError) as error:
                raise WorkerError("invalid_registry", str(error)) from error
            entries = document.get("models") if isinstance(document, dict) else None
            if not isinstance(entries, list):
                raise WorkerError("invalid_registry", "registry must contain a models array")
            for entry in entries:
                if not isinstance(entry, dict):
                    raise WorkerError("invalid_registry", "each model must be an object")
                model = SeparationModel.from_dict(entry)
                models[model.id] = model
        return cls(tuple(models.values()))

    def list(self) -> list[dict[str, Any]]:
        return [self._models[key].public_dict() for key in sorted(self._models)]

    def select(self, profile: str, model_id: str | None = None) -> SeparationModel:
        if profile not in PROFILES:
            raise WorkerError("invalid_request", f"unknown profile: {profile}")
        if model_id is not None:
            model = self._models.get(model_id)
            if model is None:
                raise WorkerError("model_not_found", f"unknown model: {model_id}")
            if profile not in model.profiles:
                raise WorkerError(
                    "model_not_allowed",
                    f"model {model_id} is not registered for profile {profile}",
                )
            return model
        for model in self._models.values():
            if profile in model.profiles:
                return model
        raise WorkerError("model_not_found", f"no model registered for profile {profile}")
