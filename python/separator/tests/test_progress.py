import io
import json
import tempfile
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

from k3_separator.models import ModelRegistry
from k3_separator.progress import ProgressReporter
from k3_separator.runtime import RuntimeResult
from k3_separator.service import SeparationService
from k3_separator.worker import serve


class ProgressTests(unittest.TestCase):
    def test_inference_progress_handles_split_writes_and_preserves_stderr(self):
        chunks = ["\r  0%|          |", "\r 3", "7%|███       | 37/100", "\r100%|██████████|\n"]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "progress.json"
            reporter = ProgressReporter(path)
            observed = []
            logs = io.StringIO()
            reporter.stage("separating_vocals")
            with patch("sys.stderr", logs), reporter.watch_inference():
                for chunk in chunks:
                    sys.stderr.write(chunk)
                    observed.append(json.loads(path.read_text())["fraction"])
            self.assertEqual([0, 0, 0.37, 1], observed)
            self.assertEqual("".join(chunks), logs.getvalue())
            self.assertFalse(path.with_suffix(".tmp").exists())

    def test_stage_switch_clears_previous_pass_percentage(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "progress.json"
            reporter = ProgressReporter(path)
            reporter.stage("separating_vocals")
            reporter.fraction(1)
            reporter.stage("loading_backing_vocals")
            document = json.loads(path.read_text())
            self.assertEqual("loading_backing_vocals", document["phase"])
            self.assertIsNone(document["fraction"])

    def test_ordinary_logs_do_not_create_a_percentage(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "progress.json"
            reporter = ProgressReporter(path)
            reporter.stage("separating_vocals")
            with patch("sys.stderr", io.StringIO()), reporter.watch_inference():
                sys.stderr.write("volume 70% and 37 files\n")
                sys.stderr.write("\r150%|invalid\n")
            self.assertIsNone(json.loads(path.read_text())["fraction"])

    def test_progress_write_failure_does_not_fail_separation(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "progress.json"
            path.mkdir()
            reporter = ProgressReporter(path)
            logs = io.StringIO()
            with patch("sys.stderr", logs):
                reporter.stage("preparing")
                reporter.fraction(1)
            self.assertTrue(path.is_dir())
            self.assertIn("k3 progress unavailable", logs.getvalue())
            self.assertFalse(path.with_suffix(".tmp").exists())

    def test_progress_does_not_add_lines_to_worker_json_protocol(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "歌曲.wav"
            source.write_bytes(b"input")
            reporter = ProgressReporter(root / "progress.json")

            class Runtime:
                def separate(self, input_path, scratch_dir, model, options, backing_vocals_model):
                    reporter.stage("separating_vocals")
                    with reporter.watch_inference():
                                sys.stderr.write("\r100%|██████████|\n")
                    vocals = scratch_dir / "vocals.wav"
                    backing = scratch_dir / "backing.wav"
                    vocals.write_bytes(b"vocals")
                    backing.write_bytes(b"backing")
                    return RuntimeResult(vocals, backing, "a" * 64)

            service = SeparationService(ModelRegistry.load(), Runtime(), reporter)
            request = {"id": 1, "method": "separate", "params": {
                "input_path": str(source), "output_dir": str(root / "stems"),
                "profile": "fast", "preserve_backing_vocals": False,
            }}
            output = io.StringIO()
            with patch("sys.stderr", io.StringIO()):
                serve(service, io.StringIO(json.dumps(request) + "\n"), output)
            self.assertEqual(1, len(output.getvalue().splitlines()))
            self.assertTrue(json.loads(output.getvalue())["ok"])
            self.assertEqual("saving_project", json.loads((root / "progress.json").read_text())["phase"])
