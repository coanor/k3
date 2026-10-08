"""The audio-separator implementation hidden behind the worker seam."""

from __future__ import annotations

import hashlib
import http.client
import importlib.util
import logging
import os
import shutil
import sys
import tempfile
import time
import urllib.error
import urllib.request
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterator, Protocol

from .errors import WorkerError
from .models import SeparationModel
from .progress import ProgressReporter
from .hardware import check_accelerator, configure_device, read_plan


@dataclass(frozen=True)
class RuntimeResult:
    vocals: Path
    accompaniment: Path
    checkpoint_sha256: str
    backing_vocals: Path | None = None
    backing_vocals_checkpoint_sha256: str | None = None
    backing_vocals_runtime_options: dict[str, Any] | None = None


class SeparationRuntime(Protocol):
    def status(self) -> dict[str, Any]: ...

    def separate(
        self,
        input_path: Path,
        scratch_dir: Path,
        model: SeparationModel,
        options: dict[str, Any],
        backing_vocals_model: SeparationModel | None = None,
    ) -> RuntimeResult: ...


class AudioSeparatorRuntime:
    """Runs any allow-listed audio-separator checkpoint and emits two WAV stems."""

    def __init__(self, model_dir: Path, progress: ProgressReporter | None = None,
                 device: str | None = None) -> None:
        self._device = device if device is not None else os.environ.get("K3_DEVICE", "auto")
        if self._device not in {"auto", "cpu", "gpu"}:
            raise WorkerError("invalid_request", "device must be auto, cpu, or gpu")
        self._backend: str | None = None
        configure_device()
        self._progress = progress or ProgressReporter()
        self._model_dir = model_dir.expanduser().resolve()
        self._model_dir.mkdir(parents=True, exist_ok=True)
        os.environ.setdefault("TORCH_HOME", str(self._model_dir / "torch"))
        self._configure_ffmpeg()

    def status(self) -> dict[str, Any]:
        audio_separator_installed = False
        audio_separator_error = None
        if importlib.util.find_spec("audio_separator") is not None:
            try:
                from audio_separator.separator import Separator  # noqa: F401

                audio_separator_installed = True
            except Exception as error:  # health must expose broken transitive installs
                audio_separator_error = f"{type(error).__name__}: {error}"
        torch_installed = importlib.util.find_spec("torch") is not None
        cuda_available = False
        device = None
        if torch_installed:
            try:
                import torch

                cuda_available = bool(torch.cuda.is_available())
                if cuda_available:
                    device = torch.cuda.get_device_name(0)
            except Exception:  # status must remain usable with a broken GPU install
                pass
        return {
            "audio_separator_installed": audio_separator_installed,
            "audio_separator_error": audio_separator_error,
            "torch_installed": torch_installed,
            "cuda_available": cuda_available,
            "device": device,
            "ffmpeg": shutil.which("ffmpeg"),
            "model_dir": str(self._model_dir),
            "hardware": read_plan(),
            "device_selection": self._device,
            "selected_backend": self._backend,
        }

    def _inference_backend(self) -> str:
        if self._backend is not None:
            return self._backend
        plan = read_plan()
        if self._device == "cpu" or (self._device == "auto" and plan and plan["backend"] == "cpu"):
            self._backend = "cpu"
            logging.getLogger(__name__).info("Separation device selection: %s; backend: cpu", self._device)
            return self._backend
        try:
            import torch

            backend = ("cuda" if torch.cuda.is_available() else
                       "mps" if torch.backends.mps.is_available() else "cpu")
            if backend == "cpu" and self._device == "gpu":
                raise RuntimeError("No compatible CUDA/MPS GPU is available; install a compatible runtime or select CPU")
            if backend != "cpu":
                check_accelerator(backend)
        except Exception as error:
            if self._device == "gpu":
                raise WorkerError("gpu_unavailable", f"GPU-only separation is unavailable: {error}") from error
            logging.getLogger(__name__).warning("GPU validation failed; using CPU: %s", error)
            backend = "cpu"
        self._backend = backend
        logging.getLogger(__name__).info("Separation device selection: %s; backend: %s", self._device, backend)
        return backend

    def _configure_ffmpeg(self) -> None:
        if shutil.which("ffmpeg") is not None:
            return
        try:
            import imageio_ffmpeg
        except ImportError:
            return
        executable = Path(imageio_ffmpeg.get_ffmpeg_exe()).resolve()
        bin_dir = self._model_dir.parent / "bin"
        bin_dir.mkdir(parents=True, exist_ok=True)
        link = bin_dir / ("ffmpeg.exe" if sys.platform == "win32" else "ffmpeg")
        if sys.platform == "win32":
            self._copy_windows_ffmpeg(executable, link)
            os.environ["PATH"] = f"{bin_dir}{os.pathsep}{os.environ.get('PATH', '')}"
            return
        if link.is_symlink() and link.resolve() == executable:
            os.environ["PATH"] = f"{bin_dir}{os.pathsep}{os.environ.get('PATH', '')}"
            return
        temporary_link = bin_dir / f".ffmpeg.{os.getpid()}"
        temporary_link.unlink(missing_ok=True)
        temporary_link.symlink_to(executable)
        os.replace(temporary_link, link)
        os.environ["PATH"] = f"{bin_dir}{os.pathsep}{os.environ.get('PATH', '')}"

    @staticmethod
    def _copy_windows_ffmpeg(executable: Path, destination: Path) -> None:
        if (
            destination.is_file()
            and destination.stat().st_size == executable.stat().st_size
        ):
            return
        temporary = destination.with_name(f".ffmpeg.{os.getpid()}.exe")
        temporary.unlink(missing_ok=True)
        try:
            shutil.copy2(executable, temporary)
            os.replace(temporary, destination)
        finally:
            temporary.unlink(missing_ok=True)

    def separate(
        self,
        input_path: Path,
        scratch_dir: Path,
        model: SeparationModel,
        options: dict[str, Any],
        backing_vocals_model: SeparationModel | None = None,
    ) -> RuntimeResult:
        if backing_vocals_model is None:
            return self._separate_once(input_path, scratch_dir, model, options)

        primary_dir = scratch_dir / "primary"
        backing_dir = scratch_dir / "backing-vocals"
        primary_dir.mkdir()
        backing_dir.mkdir()
        primary = self._separate_once(input_path, primary_dir, model, options)
        backing_options = dict(options)
        if (self._inference_backend() != "cpu" and backing_vocals_model.architecture == "mdx-net"
                and backing_options.get("segment_size", 256) == 256):
            backing_options["segment_size"] = 128
        separated_vocals = self._separate_once(
            primary.vocals, backing_dir, backing_vocals_model, backing_options, "backing_vocals"
        )
        self._progress.stage("writing_audio")
        lead_vocals = scratch_dir / "vocals.wav"
        backing_vocals = scratch_dir / "backing-vocals.wav"
        accompaniment = scratch_dir / "accompaniment.wav"
        os.replace(separated_vocals.vocals, lead_vocals)
        os.replace(separated_vocals.accompaniment, backing_vocals)
        self._sum_audio((primary.accompaniment, backing_vocals), accompaniment)
        self._validate_audio(lead_vocals, "lead vocals")
        self._validate_audio(backing_vocals, "backing vocals")
        self._validate_audio(accompaniment, "accompaniment with backing vocals")
        return RuntimeResult(
            lead_vocals,
            accompaniment,
            primary.checkpoint_sha256,
            backing_vocals,
            separated_vocals.checkpoint_sha256,
            {**backing_vocals_model.runtime_options, **backing_options},
        )

    def _separate_once(
        self,
        input_path: Path,
        scratch_dir: Path,
        model: SeparationModel,
        options: dict[str, Any],
        pass_kind: str = "vocals",
    ) -> RuntimeResult:
        self._progress.stage(f"loading_{pass_kind}")
        backend = self._inference_backend()
        try:
            from audio_separator.separator import Separator
        except ImportError as error:
            raise WorkerError(
                "runtime_unavailable",
                "audio-separator is not installed; install k3-separator[gpu] or [cpu]",
            ) from error
        self._prepare_primary_artifact(model)

        settings = dict(model.runtime_options)
        settings.update(options)
        common: dict[str, Any] = {
            "output_dir": str(scratch_dir),
            "model_file_dir": str(self._model_dir),
            "output_format": "WAV",
            "sample_rate": 44_100,
            # SoundFile attempts to preserve an MP3 input subtype when writing
            # WAV, which libsndfile rejects. FFmpeg normalizes every supported
            # input format to a valid PCM WAV before outputs are committed.
            "use_soundfile": False,
            "use_autocast": bool(settings.pop("autocast", True)),
        }
        if model.architecture in {"bs-roformer", "mel-band-roformer"}:
            common["mdxc_params"] = {
                "segment_size": settings.pop("segment_size", 256),
                "override_model_segment_size": True,
                "overlap": settings.pop("overlap", 8),
                "batch_size": settings.pop("batch_size", 1),
                "pitch_shift": settings.pop("pitch_shift", 0),
            }
        elif model.architecture == "mdx-net":
            common["mdx_params"] = {
                "segment_size": settings.pop("segment_size", 256),
                "overlap": settings.pop("overlap", 0.25),
                "batch_size": settings.pop("batch_size", 1),
                "hop_length": settings.pop("hop_length", 1024),
                "enable_denoise": settings.pop("enable_denoise", False),
            }
        elif model.architecture == "htdemucs":
            common["demucs_params"] = {
                "segment_size": settings.pop("segment_size", 10),
                "shifts": settings.pop("shifts", 1),
                "overlap": settings.pop("overlap", 0.25),
                "segments_enabled": True,
            }
        if settings:
            raise WorkerError(
                "invalid_request", f"unsupported runtime options: {', '.join(sorted(settings))}"
            )

        try:
            with self._inference_model_dir(scratch_dir) as inference_models:
                common["model_file_dir"] = str(inference_models)
                separator = Separator(**common)
                import torch

                separator.torch_device = torch.device(backend)
                separator.torch_device_mps = torch.device("mps") if backend == "mps" else None
                if backend == "cpu":
                    separator.onnx_execution_provider = ["CPUExecutionProvider"]
                separator.load_model(model_filename=model.filename)
                if self._device == "gpu":
                    instance = separator.model_instance
                    actual_device = getattr(instance, "torch_device", None)
                    if getattr(actual_device, "type", None) != backend:
                        raise WorkerError("gpu_unavailable", f"GPU-only separation requires a model on {backend}; the loaded model uses {actual_device}")
                    # Check the loaded inference path, including checkpoints whose
                    # custom registry architecture does not match their real type.
                    dim_t = getattr(instance, "dim_t", None)
                    if dim_t is not None and getattr(instance, "segment_size", None) == dim_t:
                        alternative = 128 if dim_t != 128 else 256
                        raise WorkerError("invalid_request", f"GPU-only MDX separation requires a non-native segment size; try segment size {alternative}")
                output_names = {stem: self._output_name(stem) for stem in model.output_stems}
                self._progress.stage(f"separating_{pass_kind}")
                with self._progress.watch_inference():
                    separator.separate(str(input_path), output_names)
            self._progress.stage("writing_audio")
            vocals = scratch_dir / "vocals.wav"
            accompaniment = scratch_dir / "accompaniment.wav"
            if not accompaniment.is_file():
                self._mix_accompaniment(scratch_dir, model.output_stems, accompaniment)
            self._validate_audio(vocals, "vocals")
            self._validate_audio(accompaniment, "accompaniment")
            digest = self._checkpoint_digest(model)
            return RuntimeResult(vocals, accompaniment, digest)
        except WorkerError:
            raise
        except Exception as error:
            raise WorkerError("separation_failed", str(error)) from error

    @contextmanager
    def _inference_model_dir(self, scratch_dir: Path) -> Iterator[Path]:
        # onnx2torch 的路径输入在模型旁写入形状推断临时文件。
        # 系统安装目录只读时，在工程临时目录链接 checkpoint 与元数据；
        # 原始模型保持只读，大文件无需复制，临时文件随本次分离清理。
        if os.name == "nt" or os.access(self._model_dir, os.W_OK):
            yield self._model_dir
            return
        with tempfile.TemporaryDirectory(prefix=".k3-models-", dir=scratch_dir) as directory:
            working_models = Path(directory)
            for source in self._model_dir.iterdir():
                if source.is_file():
                    (working_models / source.name).symlink_to(source)
            yield working_models

    def _prepare_primary_artifact(self, model: SeparationModel) -> None:
        if model.expected_sha256 is None:
            return
        destination = self._model_dir / model.filename
        if destination.is_file():
            self._verify_primary_artifact(model, destination)
            return
        if model.download_url is None:
            raise WorkerError(
                "checkpoint_unavailable",
                f"place the pinned checkpoint at {destination} before running {model.id}",
            )
        for attempt in range(3):
            try:
                self._download_primary_artifact(model, destination)
                return
            except WorkerError as error:
                cause = error.__cause__
                retryable = isinstance(cause, (urllib.error.URLError, TimeoutError,
                                              ConnectionError, http.client.IncompleteRead))
                if isinstance(cause, urllib.error.HTTPError) and cause.code not in {429, 500, 502, 503, 504}:
                    retryable = False
                if error.code != "checkpoint_download_failed" or not retryable or attempt == 2:
                    raise
                logging.getLogger(__name__).warning(
                    "Model download interrupted for %s; retrying (%s/3)", model.id, attempt + 2
                )
                time.sleep(2 ** attempt)

    def _download_primary_artifact(self, model: SeparationModel, destination: Path) -> None:
        temporary_name = None
        try:
            request = urllib.request.Request(
                model.download_url, headers={"User-Agent": "k3-separator/0.1"}
            )
            with urllib.request.urlopen(request, timeout=60) as response:
                with tempfile.NamedTemporaryFile(
                    prefix=f".{model.filename}.", dir=self._model_dir, delete=False
                ) as temporary:
                    temporary_name = temporary.name
                    received = _copy_stream(response, temporary)
                    length = response.headers.get("Content-Length") if hasattr(response, "headers") else None
                    if length is not None and received != int(length):
                        raise ConnectionError(f"Incomplete model download: received {received} of {length} bytes")
                    temporary.flush()
                    os.fsync(temporary.fileno())
            temporary_path = Path(temporary_name)
            self._verify_primary_artifact(model, temporary_path)
            os.replace(temporary_path, destination)
        except WorkerError:
            raise
        except Exception as error:
            raise WorkerError(
                "checkpoint_download_failed", f"failed to download {model.id}: {error}"
            ) from error
        finally:
            if temporary_name is not None:
                Path(temporary_name).unlink(missing_ok=True)

    @staticmethod
    def _verify_primary_artifact(model: SeparationModel, path: Path) -> None:
        digest = _hash_file(path)
        if digest != model.expected_sha256:
            raise WorkerError(
                "checkpoint_mismatch",
                f"SHA-256 mismatch for {model.id}: expected {model.expected_sha256}, got {digest}",
            )

    @staticmethod
    def _output_name(stem: str) -> str:
        if stem == "Vocals":
            return "vocals"
        if stem == "Instrumental":
            return "accompaniment"
        return f"part-{stem.lower()}"

    @staticmethod
    def _validate_audio(path: Path, label: str) -> None:
        if not path.is_file() or path.stat().st_size == 0:
            raise WorkerError("separation_failed", f"runtime did not produce {label} WAV")

    @staticmethod
    def _mix_accompaniment(
        scratch_dir: Path, stems: tuple[str, ...], destination: Path
    ) -> None:
        inputs = tuple(
            scratch_dir / f"part-{stem.lower()}.wav"
            for stem in stems
            if stem != "Vocals"
        )
        AudioSeparatorRuntime._sum_audio(inputs, destination)

    @staticmethod
    def _sum_audio(inputs: tuple[Path, ...], destination: Path) -> None:
        try:
            import numpy as np
            import soundfile as sf
        except ImportError as error:
            raise WorkerError(
                "runtime_unavailable", "numpy and soundfile are required for multi-stem models"
            ) from error
        if not inputs or any(not path.is_file() for path in inputs):
            raise WorkerError(
                "separation_failed", "runtime did not produce all accompaniment stems"
            )
        if len(inputs) == 1:
            os.replace(inputs[0], destination)
            return
        audio_parts = []
        sample_rate = None
        shape = None
        for path in inputs:
            audio, current_rate = sf.read(path, always_2d=True, dtype="float32")
            if sample_rate is None:
                sample_rate, shape = current_rate, audio.shape
            elif current_rate != sample_rate or audio.shape != shape:
                raise WorkerError("separation_failed", "accompaniment stems do not align")
            audio_parts.append(audio)
        sf.write(destination, np.sum(audio_parts, axis=0), sample_rate, subtype="FLOAT")

    def _checkpoint_digest(self, model: SeparationModel) -> str:
        patterns = model.artifact_globs or (f"**/{model.filename}",)
        artifacts = sorted(
            {path for pattern in patterns for path in self._model_dir.glob(pattern) if path.is_file()}
        )
        if not artifacts:
            raise WorkerError(
                "checkpoint_unverifiable", f"cannot locate cached artifacts for {model.id}"
            )
        if len(artifacts) == 1:
            digest = _hash_file(artifacts[0])
        else:
            aggregate = hashlib.sha256()
            for path in artifacts:
                relative = path.relative_to(self._model_dir).as_posix().encode()
                aggregate.update(len(relative).to_bytes(4, "big"))
                aggregate.update(relative)
                aggregate.update(bytes.fromhex(_hash_file(path)))
            digest = aggregate.hexdigest()
        if model.expected_sha256 is not None and digest != model.expected_sha256:
            raise WorkerError(
                "checkpoint_mismatch",
                f"SHA-256 mismatch for {model.id}: expected {model.expected_sha256}, got {digest}",
            )
        return digest


def _hash_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _copy_stream(source: Any, destination: Any) -> int:
    received = 0
    while chunk := source.read(1024 * 1024):
        destination.write(chunk)
        received += len(chunk)
    return received
