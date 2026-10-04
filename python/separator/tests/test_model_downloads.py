"""Default model installation must work when Hugging Face is unreachable."""

from dataclasses import replace
import hashlib
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import urllib.error
import urllib.parse

from k3_separator.errors import WorkerError
from k3_separator.models import ModelRegistry
from k3_separator.runtime import AudioSeparatorRuntime


class DefaultModelDownloadTests(unittest.TestCase):
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
        with tempfile.TemporaryDirectory() as directory, patch("urllib.request.urlopen", return_value=io.BytesIO(b"wrong checkpoint")):
            runtime = AudioSeparatorRuntime(Path(directory))
            with self.assertRaises(WorkerError) as error:
                runtime._prepare_primary_artifact(model)
            self.assertEqual(error.exception.code, "checkpoint_mismatch")
            self.assertEqual(list(Path(directory).iterdir()), [])


if __name__ == "__main__":
    unittest.main()
