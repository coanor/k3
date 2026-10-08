from dataclasses import replace
import hashlib
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from k3_separator.errors import WorkerError
from k3_separator.models import BUILTIN_MODELS, ModelRegistry
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

    def test_hardware_defaults_and_explicit_preferences(self) -> None:
        defaults = {"profile": "fast", "model": "uvr-mdx-karaoke-2", "segment_size": 128,
                    "autocast": False, "preserve_backing_vocals": False}
        service = SeparationService(ModelRegistry.load(), FakeRuntime(), defaults=defaults, accelerator=True)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "song.wav"
            source.write_bytes(b"source")
            for name, settings in (("automatic", {}), ("explicit", {
                    "profile": "quality", "model_id": "mel-band-roformer-kim-vocal-2",
                    "options": {"segment_size": 64, "autocast": True},
                    "preserve_backing_vocals": True}), ("balanced", {
                    "profile": "balanced", "preserve_backing_vocals": False})):
                result = service.handle({"method": "separate", "params": {
                    "input_path": str(source), "output_dir": str(root / name), **settings}})
                provenance = result["provenance"]
                if name == "automatic":
                    self.assertEqual("fast", provenance["profile"])
                    self.assertEqual(128, provenance["runtime_options"]["segment_size"])
                    self.assertFalse(provenance["runtime_options"]["autocast"])
                    self.assertNotIn("backing_vocals", result)
                elif name == "explicit":
                    self.assertEqual("mel-band-roformer-kim-vocal-2", provenance["checkpoint_id"])
                    self.assertEqual(64, provenance["runtime_options"]["segment_size"])
                    self.assertTrue(provenance["runtime_options"]["autocast"])
                    self.assertIn("backing_vocals", result)
                else:
                    self.assertEqual(128, provenance["runtime_options"]["segment_size"])

    def test_hardware_defaults_preserve_registry_overrides(self) -> None:
        defaults = {"profile": "fast", "model": "uvr-mdx-karaoke-2", "segment_size": 128,
                    "autocast": False, "preserve_backing_vocals": False}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "song.wav"
            source.write_bytes(b"audio")
            for architecture, segment in (("mdx-net", 64), ("htdemucs", 10)):
                with self.subTest(architecture=architecture):
                    custom = replace(BUILTIN_MODELS[0], architecture=architecture,
                                     runtime_options={"segment_size": segment, "autocast": True})
                    registry = ModelRegistry((custom, *BUILTIN_MODELS[1:]))
                    service = SeparationService(registry, FakeRuntime(), defaults=defaults, accelerator=True)
                    result = service.handle({"method": "separate", "params": {
                        "input_path": str(source), "output_dir": str(root / architecture),
                        "preserve_backing_vocals": False}})
                    options = result["provenance"]["runtime_options"]
                    self.assertEqual(segment, options["segment_size"])
                    self.assertTrue(options["autocast"])
            custom = replace(BUILTIN_MODELS[0], id="custom-no-segment",
                             runtime_options={"autocast": False})
            result = SeparationService(ModelRegistry((custom,)), FakeRuntime(), defaults=defaults, accelerator=True).handle({
                "method": "separate", "params": {"input_path": str(source),
                "output_dir": str(root / "no-segment"), "preserve_backing_vocals": False}})
            self.assertEqual(128, result["provenance"]["runtime_options"]["segment_size"])
            self.assertFalse(result["provenance"]["runtime_options"]["autocast"])
            # A registry can also move the recommended checkpoint to another profile.
            changed = replace(BUILTIN_MODELS[0], profiles=("quality",))
            alternate = replace(BUILTIN_MODELS[0], id="custom-fast", runtime_options={"segment_size": 64})
            registry = ModelRegistry((changed, alternate, *BUILTIN_MODELS[1:]))
            result = SeparationService(registry, FakeRuntime(), defaults=defaults, accelerator=True).handle({
                "method": "separate", "params": {"input_path": str(source),
                "output_dir": str(root / "new-profile"), "preserve_backing_vocals": False}})
            self.assertEqual("custom-fast", result["provenance"]["checkpoint_id"])
            self.assertEqual(64, result["provenance"]["runtime_options"]["segment_size"])

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
            stems = root / "stems"
            stems.mkdir()
            (stems / "backing-vocals.wav").write_bytes(b"stale backing")

            result = self.service.handle(
                {
                    "method": "separate",
                    "params": {
                        "input_path": str(source),
                        "output_dir": str(stems),
                        "profile": "quality",
                        "preserve_backing_vocals": False,
                    },
                }
            )

            self.assertEqual(b"vocals", Path(result["vocals"]).read_bytes())
            self.assertNotIn("backing_vocals", result)
            self.assertNotIn("backing_vocals_model", result["provenance"])
            self.assertEqual(b"stale backing", (stems / "backing-vocals.wav").read_bytes())

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

    def test_overwrite_rolls_back_every_stem_when_commit_fails(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "song.wav"
            source.write_bytes(b"source")
            stems = root / "stems"
            stems.mkdir()
            previous = {
                "vocals.wav": b"old vocals",
                "backing-vocals.wav": b"old backing",
                "accompaniment.wav": b"old accompaniment",
            }
            for name, content in previous.items():
                (stems / name).write_bytes(content)
            real_replace = os.replace

            def fail_during_commit(source_path, destination_path):
                source_path = Path(source_path)
                if (
                    source_path.name == "accompaniment.wav"
                    and source_path.parent.name.startswith(".k3-separate-")
                ):
                    raise OSError("simulated commit failure")
                real_replace(source_path, destination_path)

            with (
                patch("k3_separator.service.os.replace", side_effect=fail_during_commit),
                self.assertRaisesRegex(OSError, "simulated commit failure"),
            ):
                self.service.handle(
                    {
                        "method": "separate",
                        "params": {
                            "input_path": str(source),
                            "output_dir": str(stems),
                            "profile": "quality",
                            "model_id": "mel-band-roformer-kim-vocal-2",
                            "overwrite": True,
                        },
                    }
                )

            for name, content in previous.items():
                self.assertEqual(content, (stems / name).read_bytes())

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
