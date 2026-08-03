import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import patch

from k3_separator.models import SeparationModel
from k3_separator.runtime import AudioSeparatorRuntime


class AudioSeparatorRuntimeTests(unittest.TestCase):
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


if __name__ == "__main__":
    unittest.main()
