import os
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from k3_separator.models import SeparationModel
from k3_separator.runtime import AudioSeparatorRuntime
from k3_separator.errors import WorkerError


class AudioSeparatorRuntimeTests(unittest.TestCase):
    def test_device_choice_enforces_cpu_or_gpu_and_auto_can_fall_back(self):
        import torch  # Preserve native imports before patch.dict snapshots modules.

        fake_torch = types.ModuleType("torch")
        fake_torch.cuda = types.SimpleNamespace(is_available=Mock(return_value=True))
        fake_torch.backends = types.SimpleNamespace(mps=types.SimpleNamespace(is_available=Mock(return_value=False)))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with (patch.dict(sys.modules, {"torch": fake_torch}),
                  patch("k3_separator.runtime.read_plan", return_value=None),
                  patch("k3_separator.runtime.check_accelerator", side_effect=RuntimeError("no kernel image")) as probe):
                cpu = AudioSeparatorRuntime(root / "cpu", device="cpu")
                self.assertEqual("cpu", cpu._inference_backend())
                probe.assert_not_called()
                fake_torch.cuda.is_available.assert_not_called()
                automatic = AudioSeparatorRuntime(root / "auto", device="auto")
                self.assertEqual("cpu", automatic._inference_backend())
                gpu = AudioSeparatorRuntime(root / "gpu", device="gpu")
                with self.assertRaises(WorkerError) as error:
                    gpu._inference_backend()
                self.assertEqual("gpu_unavailable", error.exception.code)
                self.assertIsNone(gpu._backend)
            with (patch.dict(sys.modules, {"torch": fake_torch}),
                  patch("k3_separator.runtime.check_accelerator") as probe):
                gpu = AudioSeparatorRuntime(root / "available", device="gpu")
                self.assertEqual("cuda", gpu._inference_backend())
                self.assertEqual("cuda", gpu._inference_backend())
                probe.assert_called_once_with("cuda")
            with (patch.dict(sys.modules, {"torch": fake_torch}),
                  patch("k3_separator.runtime.check_accelerator") as probe):
                fake_torch.cuda.is_available.return_value = False
                gpu = AudioSeparatorRuntime(root / "missing", device="gpu")
                with self.assertRaises(WorkerError) as error:
                    gpu._inference_backend()
                self.assertEqual("gpu_unavailable", error.exception.code)
                probe.assert_not_called()

    def test_gpu_only_rejects_cpu_onnx_inference(self):
        import torch

        class FakeSeparator:
            def __init__(self, **options):
                self.segment_size = options.get("mdx_params", {}).get("segment_size", 128)

            def load_model(self, model_filename):
                self.model_instance = types.SimpleNamespace(segment_size=self.segment_size, dim_t=128, torch_device=self.torch_device)

            def separate(self, *args):
                raise AssertionError("CPU ONNX inference must not run in GPU-only mode")

        module = types.ModuleType("audio_separator.separator")
        module.Separator = FakeSeparator
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            scratch = root / "scratch"
            scratch.mkdir()
            for architecture in ("mdx-net", "bs-roformer"):
                with self.subTest(registry_architecture=architecture):
                    model = SeparationModel(id="custom", filename="custom.onnx", architecture=architecture, profiles=("fast",))
                    with (patch.dict(sys.modules, {"audio_separator.separator": module}),
                          patch("k3_separator.runtime.AudioSeparatorRuntime._inference_backend", return_value="cuda")):
                        runtime = AudioSeparatorRuntime(root / "models", device="gpu")
                        with self.assertRaises(WorkerError) as error:
                            runtime.separate(root / "source.wav", scratch, model, {"segment_size": 128})
                        self.assertEqual("invalid_request", error.exception.code)
                        self.assertFalse(list(scratch.glob("*.wav")))

    def test_gpu_only_rejects_a_model_loaded_on_cpu(self):
        import torch

        class FakeSeparator:
            def __init__(self, **options):
                pass

            def load_model(self, model_filename):
                self.model_instance = types.SimpleNamespace(torch_device=torch.device("cpu"))

            def separate(self, *args):
                raise AssertionError("GPU-only mode must reject a CPU model before inference")

        module = types.ModuleType("audio_separator.separator")
        module.Separator = FakeSeparator
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            model = SeparationModel(id="custom", filename="custom.ckpt", architecture="bs-roformer", profiles=("quality",))
            with (patch.dict(sys.modules, {"audio_separator.separator": module}),
                  patch("k3_separator.runtime.AudioSeparatorRuntime._inference_backend", return_value="cuda")):
                runtime = AudioSeparatorRuntime(root / "models", device="gpu")
                with self.assertRaises(WorkerError) as error:
                    runtime.separate(root / "source.wav", root, model, {})
                self.assertEqual("gpu_unavailable", error.exception.code)
                self.assertFalse(list(root.glob("*.wav")))

    def test_gpu_quality_backing_pass_uses_torch_and_reports_actual_options(self):
        import torch

        observed = []

        class FakeSeparator:
            def __init__(self, **options):
                self.options = options
                self.output_dir = Path(options["output_dir"])
                self.model_dir = Path(options["model_file_dir"])

            def load_model(self, model_filename):
                self.model_instance = types.SimpleNamespace(torch_device=self.torch_device)
                if model_filename.endswith(".onnx"):
                    self.model_instance.dim_t = 256
                    self.model_instance.segment_size = self.options["mdx_params"]["segment_size"]
                (self.model_dir / model_filename).write_bytes(b"model")

            def separate(self, _input_path, output_names):
                observed.append((str(self.torch_device), self.options))
                for filename in output_names.values():
                    (self.output_dir / f"{filename}.wav").write_bytes(b"audio")

        module = types.ModuleType("audio_separator.separator")
        module.Separator = FakeSeparator
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            scratch = root / "scratch"
            scratch.mkdir()
            primary = SeparationModel(id="primary", filename="primary.ckpt", architecture="bs-roformer", profiles=("quality",))
            backing = SeparationModel(id="backing", filename="backing.onnx", architecture="mdx-net", profiles=("fast",))
            with (patch.dict(sys.modules, {"audio_separator.separator": module}),
                  patch("k3_separator.runtime.AudioSeparatorRuntime._inference_backend", return_value="cuda"),
                  patch("k3_separator.runtime.AudioSeparatorRuntime._sum_audio", side_effect=lambda inputs, dest: dest.write_bytes(b"mixed"))):
                result = AudioSeparatorRuntime(root / "models", device="gpu").separate(
                    root / "input.wav", scratch, primary, {"segment_size": 256}, backing)
        self.assertEqual(["cuda", "cuda"], [entry[0] for entry in observed])
        self.assertEqual(256, observed[0][1]["mdxc_params"]["segment_size"])
        self.assertEqual(128, observed[1][1]["mdx_params"]["segment_size"])
        self.assertEqual(128, result.backing_vocals_runtime_options["segment_size"])

    @unittest.skipIf(os.name == "nt" or (hasattr(os, "geteuid") and os.geteuid() == 0),
                     "只读模型转换的权限问题需要 Unix 普通用户")
    def test_readonly_models_allow_converter_temporary_files(self) -> None:
        class FakeSeparator:
            def __init__(self, **options):
                self.output_dir = Path(options["output_dir"])
                self.model_dir = Path(options["model_file_dir"])

            def load_model(self, model_filename):
                model_path = self.model_dir / model_filename
                self.model = model_path.read_bytes()
                # onnx2torch 的路径输入会在 checkpoint 旁创建临时文件。
                with tempfile.NamedTemporaryFile(dir=model_path.parent):
                    pass

            def separate(self, _input_path, output_names):
                for filename in output_names.values():
                    (self.output_dir / f"{filename}.wav").write_bytes(b"audio")

        module = types.ModuleType("audio_separator.separator")
        module.Separator = FakeSeparator
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            models = root / "models"
            models.mkdir()
            (models / "fake.onnx").write_bytes(b"model")
            models.chmod(0o555)
            scratch = root / "scratch"
            scratch.mkdir()
            try:
                runtime = AudioSeparatorRuntime(models, device="cpu")
                model = SeparationModel(id="fake-mdx", filename="fake.onnx",
                                        architecture="mdx-net", profiles=("fast",))
                with patch.dict(sys.modules, {"audio_separator.separator": module}):
                    result = runtime.separate(root / "input.wav", scratch, model, {})
                self.assertEqual(result.vocals.read_bytes(), b"audio")
                self.assertEqual([path.name for path in models.iterdir()], ["fake.onnx"])
                self.assertEqual((models / "fake.onnx").read_bytes(), b"model")
                self.assertFalse(list(scratch.glob(".k3-models-*")))
            finally:
                models.chmod(0o755)

    def test_windows_runtime_materializes_an_executable_ffmpeg_fallback(self) -> None:
        fake_imageio = types.ModuleType("imageio_ffmpeg")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bundled_ffmpeg = root / "imageio-ffmpeg.exe"
            bundled_ffmpeg.write_bytes(b"windows ffmpeg")
            destination = root / "bin" / "ffmpeg.exe"
            fake_imageio.get_ffmpeg_exe = lambda: str(bundled_ffmpeg)

            def find_ffmpeg(_name):
                return str(destination) if destination.is_file() else None

            with (
                patch("k3_separator.runtime.sys.platform", "win32"),
                patch("k3_separator.runtime.shutil.which", side_effect=find_ffmpeg),
                patch.dict(sys.modules, {"imageio_ffmpeg": fake_imageio}),
                patch.dict("os.environ", {"PATH": "windows-path"}, clear=False),
            ):
                status = AudioSeparatorRuntime(root / "models", device="cpu").status()

            self.assertEqual(b"windows ffmpeg", destination.read_bytes())
            self.assertEqual(str(destination), status["ffmpeg"])

    def test_status_reports_a_broken_audio_separator_import(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            runtime = AudioSeparatorRuntime(Path(directory) / "models", device="cpu")
            with patch.dict(sys.modules, {"audio_separator.separator": None}):
                status = runtime.status()

        self.assertFalse(status["audio_separator_installed"])
        self.assertIn("ModuleNotFoundError", status["audio_separator_error"])

    def test_mp3_safe_wav_export_uses_ffmpeg_path(self) -> None:
        observed = {}

        class FakeSeparator:
            def __init__(self, **options):
                observed.update(options)

            def load_model(self, model_filename):
                (Path(observed["model_file_dir"]) / model_filename).write_bytes(b"model")

            def separate(self, _input_path, output_names):
                output_dir = Path(observed["output_dir"])
                for filename in output_names.values():
                    (output_dir / f"{filename}.wav").write_bytes(b"audio")

        package = types.ModuleType("audio_separator")
        module = types.ModuleType("audio_separator.separator")
        module.Separator = FakeSeparator
        package.separator = module

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "input.mp3"
            source.write_bytes(b"mp3")
            scratch = root / "scratch"
            scratch.mkdir()
            runtime = AudioSeparatorRuntime(root / "models", device="cpu")
            model = SeparationModel(
                id="fake-mdx",
                filename="fake.onnx",
                architecture="mdx-net",
                profiles=("fast",),
            )
            with patch.dict(
                sys.modules,
                {"audio_separator": package, "audio_separator.separator": module},
            ):
                runtime.separate(source, scratch, model, {})

        self.assertFalse(observed["use_soundfile"])

    def test_cpu_plan_disables_native_acceleration_before_loading_model(self) -> None:
        # Load native dependencies before patch.dict snapshots sys.modules.
        import torch

        observed = {}

        class FakeSeparator:
            def __init__(self, **options):
                self.output_dir = Path(options["output_dir"])
                self.model_dir = Path(options["model_file_dir"])
                self.torch_device = "mps"
                self.torch_device_mps = "mps"
                self.onnx_execution_provider = ["CoreMLExecutionProvider"]

            def load_model(self, model_filename):
                observed.update(device=str(self.torch_device), mps=self.torch_device_mps,
                                providers=self.onnx_execution_provider)
                (self.model_dir / model_filename).write_bytes(b"model")

            def separate(self, _input_path, output_names):
                for filename in output_names.values():
                    (self.output_dir / f"{filename}.wav").write_bytes(b"audio")

        module = types.ModuleType("audio_separator.separator")
        module.Separator = FakeSeparator
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            scratch = root / "scratch"
            scratch.mkdir()
            model = SeparationModel(id="fake", filename="fake.onnx", architecture="mdx-net", profiles=("fast",))
            with (patch.dict(sys.modules, {"audio_separator.separator": module}),
                  patch("k3_separator.runtime.read_plan", return_value={"backend": "cpu"})):
                AudioSeparatorRuntime(root / "models", device="cpu").separate(root / "input.wav", scratch, model, {})
        self.assertEqual({"device": str(torch.device("cpu")), "mps": None,
                          "providers": ["CPUExecutionProvider"]}, observed)

    def test_preserves_backing_vocals_with_a_second_model_pass(self) -> None:
        import numpy as np
        import soundfile as sf

        calls = []

        class FakeSeparator:
            def __init__(self, **options):
                self.output_dir = Path(options["output_dir"])

            def load_model(self, model_filename):
                self.model_filename = model_filename
                (models / model_filename).write_bytes(model_filename.encode())

            def separate(self, input_path, output_names):
                calls.append((Path(input_path), self.model_filename))
                levels = (
                    {"Vocals": 0.3, "Instrumental": 0.1}
                    if len(calls) == 1
                    else {"Vocals": 0.2, "Instrumental": 0.1}
                )
                for stem, filename in output_names.items():
                    sf.write(
                        self.output_dir / f"{filename}.wav",
                        np.full((8, 2), levels[stem], dtype=np.float32),
                        44_100,
                        subtype="FLOAT",
                    )

        package = types.ModuleType("audio_separator")
        module = types.ModuleType("audio_separator.separator")
        module.Separator = FakeSeparator
        package.separator = module

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            models = root / "models"
            source = root / "input.wav"
            source.write_bytes(b"source")
            scratch = root / "scratch"
            scratch.mkdir()
            runtime = AudioSeparatorRuntime(models, device="cpu")
            primary = SeparationModel(
                id="primary",
                filename="primary.onnx",
                architecture="mdx-net",
                profiles=("quality",),
            )
            karaoke = SeparationModel(
                id="karaoke",
                filename="karaoke.onnx",
                architecture="mdx-net",
                profiles=("fast",),
            )
            with patch.dict(
                sys.modules,
                {"audio_separator": package, "audio_separator.separator": module},
            ):
                result = runtime.separate(source, scratch, primary, {}, karaoke)

            lead, _ = sf.read(result.vocals, always_2d=True, dtype="float32")
            backing, _ = sf.read(
                result.backing_vocals, always_2d=True, dtype="float32"
            )
            accompaniment, _ = sf.read(
                result.accompaniment, always_2d=True, dtype="float32"
            )

        self.assertEqual(2, len(calls))
        np.testing.assert_allclose(lead, 0.2)
        np.testing.assert_allclose(backing, 0.1)
        np.testing.assert_allclose(accompaniment, 0.2)
        self.assertIsNotNone(result.backing_vocals_checkpoint_sha256)


if __name__ == "__main__":
    unittest.main()
