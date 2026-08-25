import json
import tempfile
import unittest
from pathlib import Path

from k3_separator.errors import WorkerError
from k3_separator.models import ModelRegistry, SeparationModel


class ModelRegistryTests(unittest.TestCase):
    def test_one_model_can_expand_different_quality_presets(self) -> None:
        model = SeparationModel.from_dict(
            {
                "id": "multi-quality",
                "filename": "model.ckpt",
                "architecture": "bs-roformer",
                "profiles": ["fast", "quality"],
                "runtime_options": {"batch_size": 1},
                "preset_options": {
                    "fast": {"segment_size": 128, "overlap": 2},
                    "quality": {"segment_size": 512, "overlap": 8},
                },
            }
        )

        self.assertEqual(128, model.options_for("fast")["segment_size"])
        self.assertEqual(512, model.options_for("quality")["segment_size"])
        self.assertEqual(1, model.options_for("quality")["batch_size"])

    def test_selects_default_and_named_quality_models(self) -> None:
        registry = ModelRegistry.load()

        self.assertEqual("bs-roformer-viperx-1297", registry.select("quality").id)
        self.assertEqual(
            "mel-band-roformer-kim-vocal-2",
            registry.select("quality", "mel-band-roformer-kim-vocal-2").id,
        )
        self.assertEqual(
            ("Vocals", "Other"),
            registry.select("quality", "mel-band-roformer-kim-vocal-2").output_stems,
        )

    def test_rejects_model_outside_requested_profile(self) -> None:
        with self.assertRaisesRegex(WorkerError, "not registered for profile fast"):
            ModelRegistry.load().select("fast", "bs-roformer-viperx-1297")

    def test_custom_registry_can_add_a_model(self) -> None:
        document = {
            "models": [
                {
                    "id": "local-quality",
                    "filename": "local.ckpt",
                    "architecture": "bs-roformer",
                    "profiles": ["quality"],
                    "license": "MIT",
                }
            ]
        }
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "models.json"
            path.write_text(json.dumps(document), encoding="utf-8")
            registry = ModelRegistry.load(path)

        self.assertEqual(
            "local.ckpt", registry.select("quality", "local-quality").filename
        )


if __name__ == "__main__":
    unittest.main()
