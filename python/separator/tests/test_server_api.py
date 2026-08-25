import hashlib
import json
import sqlite3
import struct
import tempfile
import time
import unittest
from contextlib import closing
from pathlib import Path

from fastapi.testclient import TestClient
from k3_separator.server import (
    ServerConfig,
    _fake_float_wav,
    _validate_output_wavs,
    create_app,
)


class SeparatorServerApiTests(unittest.TestCase):
    def test_output_validation_rejects_non_finite_samples(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            vocals = root / "vocals.wav"
            accompaniment = root / "accompaniment.wav"
            vocals.write_bytes(_fake_float_wav(0.1))
            invalid = bytearray(_fake_float_wav(0.2))
            invalid[44:48] = struct.pack("<f", float("nan"))
            accompaniment.write_bytes(invalid)

            with self.assertRaisesRegex(RuntimeError, "non-finite"):
                _validate_output_wavs(
                    {"vocals": vocals, "accompaniment": accompaniment}
                )

    def test_openapi_matches_checked_in_contract(self) -> None:
        contract = (
            Path(__file__).parents[3] / "docs" / "contracts" / "separator-openapi.json"
        )
        with tempfile.TemporaryDirectory() as directory:
            app = create_app(
                ServerConfig(
                    data_dir=Path(directory),
                    tokens={"snapshot": "unused"},
                    runtime="fake",
                )
            )
            with TestClient(app):
                actual = app.openapi()

        self.assertEqual(json.loads(contract.read_text(encoding="utf-8")), actual)

    @staticmethod
    def upload_input(client: TestClient, source: bytes) -> str:
        response = client.post(
            "/v1/inputs",
            json={
                "filename": "song.flac",
                "size_bytes": len(source),
                "sha256": hashlib.sha256(source).hexdigest(),
            },
        )
        input_id = response.json()["input_id"]
        if response.json()["upload_required"]:
            uploaded = client.put(
                f"/v1/inputs/{input_id}/content",
                content=source,
                headers={"Content-Type": "audio/flac"},
            )
            if uploaded.status_code != 200:
                raise AssertionError(uploaded.text)
        return input_id

    @staticmethod
    def wait_for_job(client: TestClient, job_id: str) -> dict[str, object]:
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            response = client.get(f"/v1/jobs/{job_id}")
            if response.json()["status"] in {"completed", "failed", "cancelled"}:
                return response.json()
            time.sleep(0.01)
        raise AssertionError("fake separation did not finish")

    def test_capabilities_are_authenticated_and_server_id_survives_restart(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = ServerConfig(
                data_dir=Path(directory),
                tokens={"test-client": "secret-token"},
                runtime="fake",
            )
            first_app = create_app(config)
            with TestClient(first_app) as anonymous:
                rejected = anonymous.get("/v1/capabilities")
                self.assertEqual(401, rejected.status_code)
                self.assertEqual("unauthorized", rejected.json()["error"]["code"])
            with TestClient(
                create_app(config),
                headers={"Authorization": "Bearer secret-token"},
            ) as client:
                capabilities = client.get("/v1/capabilities")
                self.assertEqual(200, capabilities.status_code)
                first_server_id = capabilities.json()["server_id"]
                self.assertEqual(
                    {"major": 1, "minor": 0}, capabilities.json()["protocol"]
                )
                self.assertEqual("fake", capabilities.json()["backend"])
                self.assertEqual(
                    1, capabilities.json()["limits"]["max_concurrent_jobs"]
                )
                models = client.get("/v1/models")
                self.assertEqual(
                    ["fake-separator"], [item["id"] for item in models.json()["models"]]
                )
            with TestClient(
                create_app(config),
                headers={"Authorization": "Bearer secret-token"},
            ) as restarted:
                self.assertEqual(
                    first_server_id,
                    restarted.get("/v1/capabilities").json()["server_id"],
                )

    def test_models_endpoint_uses_the_configured_registry(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            data_dir = Path(directory)
            model_dir = data_dir / "models"
            model_dir.mkdir()
            checkpoint = model_dir / "custom.onnx"
            checkpoint.write_bytes(b"checkpoint")
            registry = data_dir / "registry.json"
            registry.write_text(
                json.dumps(
                    {
                        "models": [
                            {
                                "id": "custom-model",
                                "filename": checkpoint.name,
                                "architecture": "mdx-net",
                                "profiles": ["fast", "quality"],
                            }
                        ]
                    }
                ),
                encoding="utf-8",
            )
            app = create_app(
                ServerConfig(
                    data_dir=data_dir,
                    tokens={"test-client": "secret-token"},
                    runtime="cpu",
                    model_dir=model_dir,
                    registry_path=registry,
                )
            )

            with TestClient(
                app, headers={"Authorization": "Bearer secret-token"}
            ) as client:
                models = client.get("/v1/models")

            self.assertEqual(200, models.status_code)
            custom = next(
                model
                for model in models.json()["models"]
                if model["id"] == "custom-model"
            )
            self.assertEqual("ready", custom["status"])
            self.assertEqual(["fast", "quality"], custom["presets"])

    def test_authenticated_client_can_upload_separate_and_download_artifacts(
        self,
    ) -> None:
        source = b"fake encoded song"
        source_sha256 = hashlib.sha256(source).hexdigest()

        with tempfile.TemporaryDirectory() as directory:
            app = create_app(
                ServerConfig(
                    data_dir=Path(directory),
                    tokens={"test-client": "secret-token"},
                    runtime="fake",
                )
            )
            with TestClient(
                app,
                headers={"Authorization": "Bearer secret-token"},
            ) as client:
                created_input = client.post(
                    "/v1/inputs",
                    json={
                        "filename": "歌曲.flac",
                        "size_bytes": len(source),
                        "sha256": source_sha256,
                    },
                )
                self.assertEqual(201, created_input.status_code)
                input_id = created_input.json()["input_id"]

                uploaded = client.put(
                    f"/v1/inputs/{input_id}/content",
                    content=source,
                    headers={"Content-Type": "audio/flac"},
                )
                self.assertEqual(200, uploaded.status_code)
                self.assertEqual("ready", uploaded.json()["status"])

                created_job = client.post(
                    "/v1/jobs",
                    headers={"Idempotency-Key": "2abcb95e-62ed-4f5c-a921-5b7246e73949"},
                    json={
                        "input_id": input_id,
                        "model_id": "fake-separator",
                        "preset": "quality",
                        "output_layout": "two_stem",
                    },
                )
                self.assertEqual(202, created_job.status_code)
                job_id = created_job.json()["job_id"]

                deadline = time.monotonic() + 2
                while time.monotonic() < deadline:
                    job = client.get(f"/v1/jobs/{job_id}")
                    self.assertEqual(200, job.status_code)
                    if job.json()["status"] == "completed":
                        break
                    time.sleep(0.01)
                else:
                    self.fail("fake separation did not complete")

                artifacts = job.json()["result"]["artifacts"]
                self.assertEqual(
                    {"vocals", "accompaniment"},
                    {artifact["role"] for artifact in artifacts},
                )
                for artifact in artifacts:
                    response = client.get(f"/v1/artifacts/{artifact['artifact_id']}")
                    self.assertEqual(200, response.status_code)
                    self.assertEqual("audio/wav", response.headers["content-type"])
                    self.assertEqual(
                        artifact["sha256"], hashlib.sha256(response.content).hexdigest()
                    )
                    self.assertTrue(response.content.startswith(b"RIFF"))
                    partial = client.get(
                        f"/v1/artifacts/{artifact['artifact_id']}",
                        headers={"Range": "bytes=0-3"},
                    )
                    self.assertEqual(206, partial.status_code)
                    self.assertEqual(b"RIFF", partial.content)

    def test_resource_errors_use_stable_machine_readable_codes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            app = create_app(
                ServerConfig(
                    data_dir=Path(directory),
                    tokens={"test-client": "secret-token"},
                    runtime="fake",
                )
            )
            with TestClient(
                app, headers={"Authorization": "Bearer secret-token"}
            ) as client:
                job = client.get("/v1/jobs/missing")
                artifact = client.get("/v1/artifacts/missing")
                create_job = client.post(
                    "/v1/jobs",
                    headers={"Idempotency-Key": "bd9ec7c2-098a-47dc-93c2-39af14076591"},
                    json={
                        "input_id": "missing",
                        "model_id": "fake-separator",
                        "preset": "fast",
                        "output_layout": "two_stem",
                    },
                )

            self.assertEqual("job_not_found", job.json()["error"]["code"])
            self.assertEqual("artifact_unavailable", artifact.json()["error"]["code"])
            self.assertEqual("input_not_found", create_job.json()["error"]["code"])

    def test_identical_jobs_reuse_completed_artifacts(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            app = create_app(
                ServerConfig(
                    data_dir=Path(directory),
                    tokens={"test-client": "secret-token"},
                    runtime="fake",
                )
            )
            with TestClient(
                app, headers={"Authorization": "Bearer secret-token"}
            ) as client:
                input_id = self.upload_input(client, b"same source")
                request = {
                    "input_id": input_id,
                    "model_id": "fake-separator",
                    "preset": "balanced",
                    "output_layout": "two_stem",
                }
                first_id = client.post(
                    "/v1/jobs",
                    headers={"Idempotency-Key": "96363580-916d-436c-9a41-b1bca57f8a68"},
                    json=request,
                ).json()["job_id"]
                first = self.wait_for_job(client, first_id)
                self.assertEqual("completed", first["status"])

                repeated_key = client.post(
                    "/v1/jobs",
                    headers={"Idempotency-Key": "96363580-916d-436c-9a41-b1bca57f8a68"},
                    json=request,
                ).json()["job_id"]
                self.assertEqual(first_id, repeated_key)

                second_id = client.post(
                    "/v1/jobs",
                    headers={"Idempotency-Key": "b08605c3-ea3f-45d1-94c5-b99d66691b86"},
                    json=request,
                ).json()["job_id"]
                second = self.wait_for_job(client, second_id)
                self.assertEqual("completed", second["status"])
                self.assertTrue(second["result"]["provenance"]["cache_hit"])
                self.assertEqual(
                    [item["artifact_id"] for item in first["result"]["artifacts"]],
                    [item["artifact_id"] for item in second["result"]["artifacts"]],
                )

    def test_websocket_subscription_reports_authoritative_job_progress(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            app = create_app(
                ServerConfig(
                    data_dir=Path(directory),
                    tokens={"test-client": "secret-token"},
                    runtime="fake",
                )
            )
            with TestClient(
                app, headers={"Authorization": "Bearer secret-token"}
            ) as client:
                input_id = self.upload_input(client, b"websocket source")
                job_id = client.post(
                    "/v1/jobs",
                    headers={"Idempotency-Key": "da04c1a0-3bc7-476d-a83d-2528505ae17f"},
                    json={
                        "input_id": input_id,
                        "model_id": "fake-separator",
                        "preset": "fast",
                        "output_layout": "two_stem",
                    },
                ).json()["job_id"]
                with client.websocket_connect("/v1/events") as websocket:
                    websocket.send_json({"action": "subscribe", "job_ids": [job_id]})
                    event = websocket.receive_json()
                self.assertEqual(job_id, event["job_id"])
                self.assertGreaterEqual(event["sequence"], 1)
                self.assertIn(event["status"], {"queued", "running", "completed"})
                self.assertIn("stage", event)

    def test_queued_job_can_be_cancelled_without_running(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            app = create_app(
                ServerConfig(
                    data_dir=Path(directory),
                    tokens={"test-client": "secret-token"},
                    runtime="fake",
                    fake_job_delay=0.2,
                )
            )
            with TestClient(
                app, headers={"Authorization": "Bearer secret-token"}
            ) as client:
                input_id = self.upload_input(client, b"cancel source")
                request = {
                    "input_id": input_id,
                    "model_id": "fake-separator",
                    "preset": "quality",
                    "output_layout": "karaoke",
                    "force": True,
                }
                first = client.post(
                    "/v1/jobs",
                    headers={"Idempotency-Key": "7e58e90e-0321-460a-abaa-bd230397b759"},
                    json=request,
                ).json()
                second = client.post(
                    "/v1/jobs",
                    headers={"Idempotency-Key": "ed6beb4c-ab3f-4ce9-913d-b200313ce3eb"},
                    json=request,
                ).json()
                cancelled = client.post(f"/v1/jobs/{second['job_id']}/cancel")
                self.assertEqual(200, cancelled.status_code)
                self.assertEqual("cancelled", cancelled.json()["status"])
                self.assertEqual(
                    "cancelled",
                    client.get(f"/v1/jobs/{second['job_id']}").json()["status"],
                )
                self.assertEqual(
                    "completed", self.wait_for_job(client, first["job_id"])["status"]
                )

    def test_running_job_is_requeued_after_server_restart(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = ServerConfig(
                data_dir=Path(directory),
                tokens={"test-client": "secret-token"},
                runtime="fake",
            )
            with TestClient(
                create_app(config),
                headers={"Authorization": "Bearer secret-token"},
            ) as client:
                input_id = self.upload_input(client, b"restart source")
                job_id = client.post(
                    "/v1/jobs",
                    headers={"Idempotency-Key": "33f4444e-79d8-447c-a963-9ad098b84e26"},
                    json={
                        "input_id": input_id,
                        "model_id": "fake-separator",
                        "preset": "quality",
                        "output_layout": "two_stem",
                    },
                ).json()["job_id"]
                self.assertEqual(
                    "completed", self.wait_for_job(client, job_id)["status"]
                )
            with closing(sqlite3.connect(Path(directory) / "separator.db")) as database:
                database.execute(
                    "UPDATE jobs SET status = 'running', stage = 'encoding', result_json = NULL WHERE id = ?",
                    (job_id,),
                )
                database.commit()
            with TestClient(
                create_app(config),
                headers={"Authorization": "Bearer secret-token"},
            ) as restarted:
                recovered = self.wait_for_job(restarted, job_id)
                self.assertEqual("completed", recovered["status"])
                self.assertEqual(2, len(recovered["result"]["artifacts"]))

    def test_cancelling_job_remains_cancelled_after_server_restart(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = ServerConfig(
                data_dir=Path(directory),
                tokens={"test-client": "secret-token"},
                runtime="fake",
            )
            with TestClient(
                create_app(config),
                headers={"Authorization": "Bearer secret-token"},
            ) as client:
                input_id = self.upload_input(client, b"cancel recovery source")
                job_id = client.post(
                    "/v1/jobs",
                    headers={"Idempotency-Key": "39c866fe-61d3-49ea-aa22-9115f035b427"},
                    json={
                        "input_id": input_id,
                        "model_id": "fake-separator",
                        "preset": "quality",
                        "output_layout": "two_stem",
                    },
                ).json()["job_id"]
                self.assertEqual(
                    "completed", self.wait_for_job(client, job_id)["status"]
                )
            with closing(sqlite3.connect(Path(directory) / "separator.db")) as database:
                database.execute(
                    "UPDATE jobs SET status = 'cancelling', stage = 'cancelling', result_json = NULL WHERE id = ?",
                    (job_id,),
                )
                database.commit()

            with TestClient(
                create_app(config),
                headers={"Authorization": "Bearer secret-token"},
            ) as restarted:
                recovered = restarted.get(f"/v1/jobs/{job_id}").json()

            self.assertEqual("cancelled", recovered["status"])
            self.assertEqual("cancelled", recovered["stage"])


if __name__ == "__main__":
    unittest.main()
