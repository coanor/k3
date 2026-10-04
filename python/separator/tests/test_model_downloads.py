"""Default model installation must work when Hugging Face is unreachable."""

from dataclasses import replace
import hashlib
import http.client
import io
import os
from pathlib import Path
import sys
import tempfile
from types import ModuleType, SimpleNamespace
import unittest
from unittest.mock import patch
import urllib.error
import urllib.parse

from k3_separator.errors import WorkerError
from k3_separator import bundle
from k3_separator.models import ModelRegistry
from k3_separator.runtime import AudioSeparatorRuntime


class DefaultModelDownloadTests(unittest.TestCase):
    def test_bundle_cache_verifies_new_hashes_without_modifying_readonly_sources(self):
        defaults = ModelRegistry.load()
        models = tuple(defaults.select(profile) for profile in ("fast", "balanced", "quality"))
        original = {model.filename: ("cached " + model.id).encode() for model in models}
        required = dict(original)
        changed = models[-1].filename
        required[changed] = b"new release checkpoint"
        registry = ModelRegistry(tuple(replace(model, expected_sha256=hashlib.sha256(required[model.filename]).hexdigest()) for model in models))
        imageio = ModuleType("imageio_ffmpeg")
        separator = ModuleType("audio_separator.separator")
        separator.Separator = lambda **_kwargs: SimpleNamespace(download_model_and_data=lambda _name: None)
        parent = ModuleType("audio_separator")
        parent.separator = separator
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cache = root / "readonly cache"
            cache.mkdir()
            for name, payload in original.items():
                (cache / name).write_bytes(payload)
                (cache / name).chmod(0o444)
            (cache / "private-settings.json").write_text("personal settings")
            ffmpeg = root / ("ffmpeg.exe" if os.name == "nt" else "ffmpeg")
            ffmpeg.write_bytes(b"fixture ffmpeg")
            ffmpeg.chmod(0o755)
            imageio.get_ffmpeg_exe = lambda: str(ffmpeg)
            output = root / "new runtime"
            try:
                with (
                    patch.object(bundle.ModelRegistry, "load", return_value=registry),
                    patch.object(bundle.importlib.metadata, "distributions", return_value=[]),
                    patch.dict(sys.modules, {"imageio_ffmpeg": imageio, "audio_separator": parent,
                                             "audio_separator.separator": separator}),
                    patch.dict(os.environ),
                    patch("urllib.request.urlopen", return_value=io.BytesIO(required[changed])) as download,
                ):
                    bundle.prepare(output, cache, [])
                self.assertEqual(download.call_count, 1)
                self.assertEqual(download.call_args.args[0].full_url, models[-1].download_url)
                for name, payload in required.items():
                    self.assertEqual((output / "models" / name).read_bytes(), payload)
                    self.assertEqual((cache / name).read_bytes(), original[name])
                self.assertFalse((output / "models/private-settings.json").exists())
            finally:
                for name in original:
                    (cache / name).chmod(0o644)

    def test_default_models_have_pinned_direct_github_downloads(self):
        registry = ModelRegistry.load()
        for profile in ("fast", "balanced", "quality"):
            model = registry.select(profile)
            with self.subTest(profile=profile):
                self.assertEqual(model.download_url, "https://github.com/TRvlvr/model_repo/releases/download/all_public_uvr_models/" + model.filename)
                self.assertIsNotNone(model.expected_sha256)

    def test_interrupted_download_retries_without_committing_a_partial_checkpoint(self):
        payload = b"complete checkpoint fixture"
        model = replace(ModelRegistry.load().select("quality"), expected_sha256=hashlib.sha256(payload).hexdigest())

        class BrokenStream(io.BytesIO):
            def read(self, size):
                if self.tell():
                    raise http.client.IncompleteRead(b"partial", 100)
                return super().read(size)

        with tempfile.TemporaryDirectory() as directory, patch("time.sleep"), patch(
                "urllib.request.urlopen", side_effect=[BrokenStream(b"partial"), io.BytesIO(payload)]) as download:
            runtime = AudioSeparatorRuntime(Path(directory))
            runtime._prepare_primary_artifact(model)
            self.assertEqual((Path(directory) / model.filename).read_bytes(), payload)
            self.assertEqual(list(Path(directory).iterdir()), [Path(directory) / model.filename])
            self.assertEqual(download.call_count, 2)

    def test_short_response_retries_even_when_the_stream_returns_eof(self):
        payload = b"complete checkpoint fixture"
        model = replace(ModelRegistry.load().select("quality"), expected_sha256=hashlib.sha256(payload).hexdigest())
        partial = io.BytesIO(b"partial")
        partial.headers = {"Content-Length": str(len(payload))}
        with tempfile.TemporaryDirectory() as directory, patch("time.sleep"), patch(
                "urllib.request.urlopen", side_effect=[partial, io.BytesIO(payload)]) as download:
            AudioSeparatorRuntime(Path(directory))._prepare_primary_artifact(model)
            self.assertEqual((Path(directory) / model.filename).read_bytes(), payload)
            self.assertEqual(download.call_count, 2)

    def test_retries_are_bounded_and_permanent_http_errors_are_not_retried(self):
        model = ModelRegistry.load().select("quality")
        for error, attempts in ((urllib.error.URLError(TimeoutError("timed out")), 3),
                                (urllib.error.HTTPError(model.download_url, 503, "Unavailable", {}, None), 3),
                                (urllib.error.HTTPError(model.download_url, 404, "Not Found", {}, None), 1)):
            with self.subTest(attempts=attempts, error=type(error).__name__), tempfile.TemporaryDirectory() as directory, patch(
                    "time.sleep"), patch("urllib.request.urlopen", side_effect=error) as download:
                with self.assertRaises(WorkerError) as result:
                    AudioSeparatorRuntime(Path(directory))._prepare_primary_artifact(model)
                self.assertEqual(result.exception.code, "checkpoint_download_failed")
                self.assertEqual(download.call_count, attempts)
                self.assertEqual(list(Path(directory).iterdir()), [])

    def test_quality_checkpoint_download_does_not_require_hugging_face(self):
        model = ModelRegistry.load().select("quality")
        self.assertEqual(model.expected_sha256, "5b84f37e8d444c8cb30c79d77f613a41c05868ff9c9ac6c7049c00aefae115aa")
        payload = b"pinned checkpoint fixture"
        model = replace(model, expected_sha256=hashlib.sha256(payload).hexdigest())
        requests = []

        def download(request, timeout):
            requests.append(request.full_url)
            if urllib.parse.urlsplit(request.full_url).hostname == "huggingface.co":
                raise urllib.error.URLError(TimeoutError("timed out"))
            self.assertEqual(request.full_url, "https://github.com/TRvlvr/model_repo/releases/download/all_public_uvr_models/" + model.filename)
            return io.BytesIO(payload)

        with tempfile.TemporaryDirectory() as directory, patch("urllib.request.urlopen", side_effect=download):
            runtime = AudioSeparatorRuntime(Path(directory))
            runtime._prepare_primary_artifact(model)
            self.assertEqual((Path(directory) / model.filename).read_bytes(), payload)
            # A verified cache needs no new request, including on an offline rerun.
            runtime._prepare_primary_artifact(model)
            self.assertEqual(len(requests), 1)

    def test_changed_upstream_checkpoint_is_rejected_and_partial_files_are_removed(self):
        model = ModelRegistry.load().select("quality")
        with tempfile.TemporaryDirectory() as directory, patch("urllib.request.urlopen", return_value=io.BytesIO(b"wrong checkpoint")) as download:
            runtime = AudioSeparatorRuntime(Path(directory))
            with self.assertRaises(WorkerError) as error:
                runtime._prepare_primary_artifact(model)
            self.assertEqual(error.exception.code, "checkpoint_mismatch")
            self.assertEqual(download.call_count, 1)
            self.assertEqual(list(Path(directory).iterdir()), [])


if __name__ == "__main__":
    unittest.main()
