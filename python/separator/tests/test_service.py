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

    def separate(
        self, input_path, scratch_dir, model, options, backing_vocals_model=None
    ):
        vocals = scratch_dir / "vocals.wav"
        accompaniment = scratch_dir / "accompaniment.wav"
        if backing_vocals_model is None:
            vocals.write_bytes(b"vocals")
            accompaniment.write_bytes(b"accompaniment")
            return RuntimeResult(
                vocals, accompaniment, hashlib.sha256(b"model").hexdigest()
            )
        backing_vocals = scratch_dir / "backing-vocals.wav"
        vocals.write_bytes(b"lead vocals")
        backing_vocals.write_bytes(b"backing vocals")
        accompaniment.write_bytes(b"accompaniment + backing vocals")
        return RuntimeResult(
            vocals,
            accompaniment,
            hashlib.sha256(b"model").hexdigest(),
            backing_vocals,
            hashlib.sha256(b"backing model").hexdigest(),
        )


class SeparationServiceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.service = SeparationService(ModelRegistry.load(), FakeRuntime())

    def test_preserves_backing_vocals_by_default(self) -> None:
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

            self.assertEqual(b"lead vocals", Path(result["vocals"]).read_bytes())
            self.assertEqual(
                b"backing vocals", Path(result["backing_vocals"]).read_bytes()
            )
            self.assertEqual(
                b"accompaniment + backing vocals",
                Path(result["accompaniment"]).read_bytes(),
            )
            self.assertEqual("mel-band-roformer", result["provenance"]["architecture"])
            self.assertEqual(64, len(result["provenance"]["checkpoint_sha256"]))

    def test_preserves_backing_vocals_as_a_second_stage(self) -> None:
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
                        "preserve_backing_vocals": True,
                    },
                }
            )

            self.assertEqual(b"lead vocals", Path(result["vocals"]).read_bytes())
            self.assertEqual(
                b"backing vocals", Path(result["backing_vocals"]).read_bytes()
            )
            self.assertEqual(
                b"accompaniment + backing vocals",
                Path(result["accompaniment"]).read_bytes(),
            )
            self.assertEqual(
                "uvr-mdx-karaoke-2",
                result["provenance"]["backing_vocals_model"]["checkpoint_id"],
            )

    def test_can_disable_backing_vocal_preservation(self) -> None:
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
                        "preserve_backing_vocals": False,
                    },
                }
            )

            self.assertEqual(b"vocals", Path(result["vocals"]).read_bytes())
            self.assertNotIn("backing_vocals", result)
            self.assertNotIn("backing_vocals_model", result["provenance"])

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

    def test_missing_second_model_provenance_commits_no_stems(self) -> None:
        class MissingProvenanceRuntime(FakeRuntime):
            def separate(
                self, input_path, scratch_dir, model, options, backing_vocals_model=None
            ):
                result = super().separate(
                    input_path, scratch_dir, model, options, backing_vocals_model
                )
                return RuntimeResult(
                    result.vocals,
                    result.accompaniment,
                    result.checkpoint_sha256,
                    result.backing_vocals,
                    None,
                )

        service = SeparationService(ModelRegistry.load(), MissingProvenanceRuntime())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "song.wav"
            source.write_bytes(b"source")
            stems = root / "stems"

            with self.assertRaisesRegex(WorkerError, "omitted backing-vocal"):
                service.handle(
                    {
                        "method": "separate",
                        "params": {
                            "input_path": str(source),
                            "output_dir": str(stems),
                            "profile": "quality",
                            "preserve_backing_vocals": True,
                        },
                    }
                )

            self.assertEqual([], list(stems.glob("*.wav")))

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
