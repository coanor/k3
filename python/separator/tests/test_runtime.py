import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import patch

from k3_separator.models import SeparationModel
from k3_separator.runtime import AudioSeparatorRuntime


class AudioSeparatorRuntimeTests(unittest.TestCase):
    def test_status_reports_a_broken_audio_separator_import(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            runtime = AudioSeparatorRuntime(Path(directory) / "models")
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
            runtime = AudioSeparatorRuntime(root / "models")
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
            runtime = AudioSeparatorRuntime(models)
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
