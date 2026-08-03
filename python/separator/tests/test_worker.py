import io
import json
import unittest

from k3_separator.models import ModelRegistry
from k3_separator.service import SeparationService
from k3_separator.worker import serve


class StatusRuntime:
    def status(self):
        return {"cuda_available": False}

    def separate(self, input_path, scratch_dir, model, options):
        raise AssertionError("not called")


class WorkerProtocolTests(unittest.TestCase):
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

