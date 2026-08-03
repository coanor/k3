import hashlib
import tempfile
import unittest
from pathlib import Path

from k3_separator.errors import WorkerError
from k3_separator.models import ModelRegistry
from k3_separator.runtime import RuntimeResult
from k3_separator.service import SeparationService


class FakeRuntime:
    def status(self):
        return {"cuda_available": True, "device": "Fake GPU"}

    def separate(self, input_path, scratch_dir, model, options):
        vocals = scratch_dir / "vocals.wav"
        accompaniment = scratch_dir / "accompaniment.wav"
        vocals.write_bytes(b"vocals")
        accompaniment.write_bytes(b"accompaniment")
        return RuntimeResult(vocals, accompaniment, hashlib.sha256(b"model").hexdigest())


class SeparationServiceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.service = SeparationService(ModelRegistry.load(), FakeRuntime())

    def test_separates_to_stable_names_with_provenance(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "song.wav"
            source.write_bytes(b"source")

            result = self.service.handle(
                {
                    "method": "separate",
                    "params": {
                        "input_path": str(source),
                        "output_dir": str(root / "stems"),
                        "profile": "quality",
                        "model_id": "mel-band-roformer-kim-vocal-2",
                    },
                }
            )

            self.assertEqual(b"vocals", Path(result["vocals"]).read_bytes())
            self.assertEqual(b"accompaniment", Path(result["accompaniment"]).read_bytes())
            self.assertEqual("mel-band-roformer", result["provenance"]["architecture"])
            self.assertEqual(64, len(result["provenance"]["checkpoint_sha256"]))

    def test_refuses_to_overwrite_existing_stem(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "song.wav"
            source.write_bytes(b"source")
            stems = root / "stems"
            stems.mkdir()
            (stems / "vocals.wav").write_bytes(b"keep")

            with self.assertRaisesRegex(WorkerError, "refusing to overwrite"):
                self.service.handle(
                    {
                        "method": "separate",
                        "params": {
                            "input_path": str(source),
                            "output_dir": str(stems),
                            "profile": "quality",
                        },
                    }
                )
            self.assertEqual(b"keep", (stems / "vocals.wav").read_bytes())

    def test_rejects_unknown_runtime_option(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "song.wav"
            source.write_bytes(b"source")
            with self.assertRaisesRegex(WorkerError, "unknown options"):
                self.service.handle(
                    {
                        "method": "separate",
                        "params": {
                            "input_path": str(source),
                            "output_dir": str(Path(directory) / "stems"),
                            "options": {"arbitrary_python": True},
                        },
                    }
                )


if __name__ == "__main__":
    unittest.main()

