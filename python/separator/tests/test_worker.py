import io
import json
import tempfile
import unittest
from pathlib import Path

from k3_separator.models import ModelRegistry
from k3_separator.service import SeparationService
from k3_separator.worker import serve


class StatusRuntime:
    def status(self):
        return {"cuda_available": False}

    def separate(self, input_path, scratch_dir, model, options):
        raise AssertionError("not called")


class WorkerProtocolTests(unittest.TestCase):
    def test_protocol_forces_utf8_for_windows_console_streams(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source_path = root / "中文歌曲.wav"
            source_path.write_bytes(b"audio")
            request = {
                "id": "utf8-path",
                "method": "separate",
                "params": {
                    "input_path": str(source_path),
                    "output_dir": str(root / "stems"),
                    "profile": "fast",
                    "model_id": "missing-model",
                    "preserve_backing_vocals": False,
                    "options": {},
                },
            }
            wire_input = io.BytesIO(
                (json.dumps(request, ensure_ascii=False) + "\n").encode("utf-8")
            )
            source = io.TextIOWrapper(
                wire_input, encoding="cp1252", errors="surrogateescape"
            )
            wire_output = io.BytesIO()
            destination = io.TextIOWrapper(
                wire_output, encoding="cp1252", errors="surrogateescape"
            )
            service = SeparationService(ModelRegistry.load(), StatusRuntime())

            serve(service, source, destination)
            destination.flush()
            response = json.loads(wire_output.getvalue().decode("utf-8"))

        self.assertEqual("model_not_found", response["error"]["code"])
        self.assertIn("missing-model", response["error"]["message"])

    def test_emits_one_response_for_each_request(self) -> None:
        source = io.StringIO(
            '{"id":1,"method":"health"}\n'
            'not-json\n'
            '{"id":3,"method":"list_models"}\n'
        )
        destination = io.StringIO()
        service = SeparationService(ModelRegistry.load(), StatusRuntime())

        serve(service, source, destination)

        responses = [json.loads(line) for line in destination.getvalue().splitlines()]
        self.assertEqual([1, None, 3], [response["id"] for response in responses])
        self.assertTrue(responses[0]["ok"])
        self.assertEqual("invalid_json", responses[1]["error"]["code"])
        self.assertGreater(len(responses[2]["result"]["models"]), 1)


if __name__ == "__main__":
    unittest.main()
