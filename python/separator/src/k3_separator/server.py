"""Persistent HTTP control plane for remote K3 separation."""

from __future__ import annotations

import argparse
import asyncio
import concurrent.futures
import hashlib
import hmac
import importlib.metadata
import json
import multiprocessing
import os
import platform
import shutil
import sqlite3
import struct
import subprocess
import threading
import time
import uuid
from contextlib import asynccontextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Annotated, Any

from fastapi import (
    Depends,
    FastAPI,
    Header,
    HTTPException,
    Request,
    Response,
    WebSocket,
    WebSocketDisconnect,
    status,
)
from fastapi.exceptions import RequestValidationError
from fastapi.responses import FileResponse, JSONResponse, PlainTextResponse
from pydantic import BaseModel, Field


@dataclass(frozen=True)
class ServerConfig:
    """Everything required to run one private separator server."""

    data_dir: Path
    tokens: dict[str, str]
    runtime: str = "fake"
    max_concurrent_jobs: int = 1
    max_queued_jobs: int = 32
    max_upload_bytes: int = 2 * 1024 * 1024 * 1024
    fake_job_delay: float = 0.0
    model_dir: Path | None = None
    registry_path: Path | None = None
    max_duration_seconds: int = 4 * 60 * 60
    max_input_channels: int = 8
    min_free_bytes: int = 512 * 1024 * 1024


class InputRequest(BaseModel):
    filename: str = Field(min_length=1)
    size_bytes: int = Field(ge=1)
    sha256: str = Field(pattern=r"^[0-9a-f]{64}$")


class JobRequest(BaseModel):
    input_id: str = Field(min_length=1)
    model_id: str = Field(min_length=1)
    preset: str = Field(pattern=r"^(fast|balanced|quality)$")
    output_layout: str = Field(pattern=r"^(two_stem|karaoke)$")
    force: bool = False


def _api_error(
    status_code: int,
    code: str,
    message: str,
    *,
    retryable: bool = False,
) -> HTTPException:
    return HTTPException(
        status_code,
        {"code": code, "message": message, "retryable": retryable},
    )


class SeparatorServer:
    """Deep module owning input, job, and artifact lifecycles."""

    def __init__(self, config: ServerConfig) -> None:
        self._config = config
        self._token_hashes = {
            token_id: hashlib.sha256(token.encode()).digest()
            for token_id, token in config.tokens.items()
        }
        self._backend = config.runtime
        if config.runtime != "fake":
            from .runtime import AudioSeparatorRuntime

            self._backend = AudioSeparatorRuntime._resolve_backend(config.runtime)
        self._data_dir = config.data_dir.expanduser().resolve()
        self._inputs_dir = self._data_dir / "inputs" / "sha256"
        self._jobs_dir = self._data_dir / "jobs"
        self._artifacts_dir = self._data_dir / "artifacts" / "sha256"
        for directory in (
            self._data_dir,
            self._inputs_dir,
            self._jobs_dir,
            self._artifacts_dir,
        ):
            directory.mkdir(parents=True, exist_ok=True)
        self._database = sqlite3.connect(
            self._data_dir / "separator.db", check_same_thread=False
        )
        self._database.row_factory = sqlite3.Row
        self._lock = threading.RLock()
        self._condition = threading.Condition(self._lock)
        self._stopping = False
        if config.max_concurrent_jobs < 1:
            raise ValueError("max_concurrent_jobs must be at least one")
        self._executor: concurrent.futures.ProcessPoolExecutor | None = None
        if self._backend != "fake":
            self._executor = concurrent.futures.ProcessPoolExecutor(
                max_workers=config.max_concurrent_jobs,
                mp_context=multiprocessing.get_context("spawn"),
            )
        self._initialize_database()
        self._server_id = self._load_or_create_server_id()
        self._schedulers = [
            threading.Thread(
                target=self._run_scheduler,
                name=f"k3-separator-worker-{index + 1}",
                daemon=True,
            )
            for index in range(config.max_concurrent_jobs)
        ]

    def start(self) -> None:
        for scheduler in self._schedulers:
            scheduler.start()

    def close(self) -> None:
        with self._condition:
            self._stopping = True
            self._condition.notify_all()
        for scheduler in self._schedulers:
            scheduler.join()
        if self._executor is not None:
            self._executor.shutdown(wait=True, cancel_futures=True)
        with self._lock:
            self._database.close()

    def authenticate(self, authorization: str | None) -> str:
        if authorization is None or not authorization.startswith("Bearer "):
            raise _api_error(
                status.HTTP_401_UNAUTHORIZED,
                "unauthorized",
                "missing bearer token",
            )
        supplied = authorization.removeprefix("Bearer ")
        supplied_hash = hashlib.sha256(supplied.encode()).digest()
        for token_id, expected_hash in self._token_hashes.items():
            if hmac.compare_digest(supplied_hash, expected_hash):
                return token_id
        raise _api_error(
            status.HTTP_401_UNAUTHORIZED,
            "unauthorized",
            "invalid bearer token",
        )

    def capabilities(self) -> dict[str, Any]:
        return {
            "server_id": self._server_id,
            "protocol": {"major": 1, "minor": 0},
            "features": {
                "websocket_progress": True,
                "http_polling": True,
                "cancellation": True,
                "result_cache": True,
            },
            "backend": self._backend,
            "device": "fake" if self._backend == "fake" else None,
            "limits": {
                "max_concurrent_jobs": self._config.max_concurrent_jobs,
                "max_queued_jobs": self._config.max_queued_jobs,
                "max_upload_bytes": self._config.max_upload_bytes,
                "max_duration_seconds": self._config.max_duration_seconds,
                "max_input_channels": self._config.max_input_channels,
                "min_free_bytes": self._config.min_free_bytes,
            },
        }

    def models(self) -> list[dict[str, Any]]:
        if self._backend == "fake":
            return [
                {
                    "id": "fake-separator",
                    "display_name": "Fake separator",
                    "provider": "k3-test",
                    "architecture": "fake",
                    "checkpoint_sha256": "0" * 64,
                    "status": "ready",
                    "backends": ["fake"],
                    "presets": ["fast", "balanced", "quality"],
                    "output_layouts": ["two_stem", "karaoke"],
                }
            ]
        from .models import ModelRegistry

        return [
            {
                **model,
                "display_name": model["id"],
                "checkpoint_sha256": model.get("expected_sha256"),
                "status": self._model_status(model),
                "backends": [self._backend],
                "presets": model["profiles"],
                "output_layouts": ["two_stem", "karaoke"],
            }
            for model in ModelRegistry.load(self._config.registry_path).list()
        ]

    def _model_status(self, model: dict[str, Any]) -> str:
        model_dir = self._config.model_dir or (self._data_dir / "models")
        checkpoint = model_dir.expanduser().resolve() / model["filename"]
        if not checkpoint.is_file():
            return "not_installed"
        expected = model.get("expected_sha256")
        if expected is None:
            return "ready"
        return "ready" if _hash_file(checkpoint) == expected else "invalid"

    def _checkpoint_identity(self, model: dict[str, Any]) -> str | None:
        expected = model.get("expected_sha256")
        if expected is not None:
            return str(expected)
        model_dir = self._config.model_dir or (self._data_dir / "models")
        checkpoint = model_dir.expanduser().resolve() / model["filename"]
        return _hash_file(checkpoint) if checkpoint.is_file() else None

    def readiness(self) -> dict[str, Any]:
        if self._backend == "fake":
            return {"status": "ready", "backend": "fake"}
        from .runtime import AudioSeparatorRuntime

        model_dir = self._config.model_dir or (self._data_dir / "models")
        runtime_status = AudioSeparatorRuntime(
            model_dir, backend=self._backend
        ).status()
        if not runtime_status["audio_separator_installed"]:
            raise _api_error(
                status.HTTP_503_SERVICE_UNAVAILABLE,
                "runtime_unavailable",
                "audio-separator runtime is unavailable",
                retryable=True,
            )
        return {
            "status": "ready",
            "backend": self._backend,
            "device": runtime_status["device"],
        }

    def create_input(self, request: InputRequest) -> tuple[dict[str, Any], int]:
        if request.size_bytes > self._config.max_upload_bytes:
            raise _api_error(
                status.HTTP_413_CONTENT_TOO_LARGE,
                "input_rejected",
                "input exceeds the configured upload limit",
            )
        free_bytes = shutil.disk_usage(self._data_dir).free
        if free_bytes - request.size_bytes < self._config.min_free_bytes:
            raise _api_error(
                status.HTTP_507_INSUFFICIENT_STORAGE,
                "insufficient_storage",
                "insufficient free storage for the uploaded input",
            )
        with self._lock:
            existing = self._database.execute(
                "SELECT id, status FROM inputs WHERE sha256 = ? AND status = 'ready'",
                (request.sha256,),
            ).fetchone()
            if existing is not None:
                return (
                    {
                        "input_id": existing["id"],
                        "status": existing["status"],
                        "upload_required": False,
                    },
                    status.HTTP_200_OK,
                )
            input_id = _resource_id("input")
            self._database.execute(
                """
                INSERT INTO inputs(id, filename, size_bytes, sha256, status, path, created_at)
                VALUES (?, ?, ?, ?, 'uploading', NULL, ?)
                """,
                (
                    input_id,
                    request.filename,
                    request.size_bytes,
                    request.sha256,
                    time.time(),
                ),
            )
            self._database.commit()
        return (
            {"input_id": input_id, "status": "uploading", "upload_required": True},
            status.HTTP_201_CREATED,
        )

    async def upload_input(self, input_id: str, request: Request) -> dict[str, Any]:
        with self._lock:
            row = self._database.execute(
                "SELECT * FROM inputs WHERE id = ?", (input_id,)
            ).fetchone()
        if row is None:
            raise _api_error(
                status.HTTP_404_NOT_FOUND, "input_not_found", "input not found"
            )
        if row["status"] != "uploading":
            raise _api_error(
                status.HTTP_409_CONFLICT,
                "input_not_ready",
                "input is not awaiting upload",
            )

        temporary = self._inputs_dir / f".{input_id}.partial"
        temporary.unlink(missing_ok=True)
        digest = hashlib.sha256()
        received = 0
        try:
            with temporary.open("xb") as destination:
                async for chunk in request.stream():
                    received += len(chunk)
                    if received > row["size_bytes"]:
                        raise _api_error(
                            status.HTTP_422_UNPROCESSABLE_CONTENT,
                            "checksum_mismatch",
                            "uploaded content exceeds declared size",
                        )
                    digest.update(chunk)
                    destination.write(chunk)
                destination.flush()
                os.fsync(destination.fileno())
            if received != row["size_bytes"] or digest.hexdigest() != row["sha256"]:
                raise _api_error(
                    status.HTTP_422_UNPROCESSABLE_CONTENT,
                    "checksum_mismatch",
                    "uploaded content does not match declared size and SHA-256",
                )
            if self._backend != "fake":
                _probe_audio(
                    temporary,
                    self._config.max_duration_seconds,
                    self._config.max_input_channels,
                )
            relative = Path("inputs") / "sha256" / row["sha256"]
            final_path = self._data_dir / relative
            if final_path.exists():
                temporary.unlink()
            else:
                os.replace(temporary, final_path)
            with self._lock:
                self._database.execute(
                    "UPDATE inputs SET status = 'ready', path = ? WHERE id = ?",
                    (relative.as_posix(), input_id),
                )
                self._database.commit()
            return {"input_id": input_id, "status": "ready"}
        finally:
            temporary.unlink(missing_ok=True)

    def create_job(
        self, token_id: str, idempotency_key: str, request: JobRequest
    ) -> dict[str, Any]:
        try:
            uuid.UUID(idempotency_key)
        except ValueError as error:
            raise _api_error(
                status.HTTP_422_UNPROCESSABLE_CONTENT,
                "invalid_request",
                "Idempotency-Key must be a UUID",
            ) from error
        with self._condition:
            existing = self._database.execute(
                "SELECT id, status FROM jobs WHERE token_id = ? AND idempotency_key = ?",
                (token_id, idempotency_key),
            ).fetchone()
            if existing is not None:
                return {"job_id": existing["id"], "status": existing["status"]}
            input_row = self._database.execute(
                "SELECT status FROM inputs WHERE id = ?", (request.input_id,)
            ).fetchone()
            if input_row is None:
                raise _api_error(
                    status.HTTP_404_NOT_FOUND, "input_not_found", "input not found"
                )
            if input_row["status"] != "ready":
                raise _api_error(
                    status.HTTP_409_CONFLICT,
                    "input_not_ready",
                    "input is not ready",
                )
            if self._backend == "fake":
                if request.model_id != "fake-separator":
                    raise _api_error(
                        status.HTTP_404_NOT_FOUND,
                        "model_not_found",
                        "model not found",
                    )
                resolved_spec: dict[str, Any] = {
                    "primary": {
                        "model_id": request.model_id,
                        "checkpoint_sha256": "0" * 64,
                        "options": {"preset": request.preset},
                    },
                    "backing_vocals": (
                        {
                            "model_id": "fake-backing-vocals",
                            "checkpoint_sha256": "1" * 64,
                            "options": {},
                        }
                        if request.output_layout == "karaoke"
                        else None
                    ),
                    "backend": self._backend,
                    "output_layout": request.output_layout,
                    "pipeline_version": 1,
                    "runtime": _runtime_fingerprint(self._backend),
                    "seed": 0,
                }
            else:
                from .errors import WorkerError
                from .models import BACKING_VOCALS_MODEL_ID, ModelRegistry

                try:
                    registry = ModelRegistry.load(self._config.registry_path)
                    primary = registry.select(request.preset, request.model_id)
                    backing = (
                        registry.select("fast", BACKING_VOCALS_MODEL_ID)
                        if request.output_layout == "karaoke"
                        else None
                    )
                except WorkerError as error:
                    status_code = (
                        status.HTTP_404_NOT_FOUND
                        if error.code == "model_not_found"
                        else status.HTTP_422_UNPROCESSABLE_CONTENT
                    )
                    error_code = (
                        "model_not_found"
                        if error.code == "model_not_found"
                        else "unsupported_preset"
                    )
                    raise _api_error(status_code, error_code, error.message) from error
                primary_status = self._model_status(primary.public_dict())
                if primary_status != "ready":
                    raise _api_error(
                        status.HTTP_409_CONFLICT,
                        "model_not_installed",
                        f"model is {primary_status}: {primary.id}",
                    )
                if backing is not None:
                    backing_status = self._model_status(backing.public_dict())
                    if backing_status != "ready":
                        raise _api_error(
                            status.HTTP_409_CONFLICT,
                            "model_not_installed",
                            f"model is {backing_status}: {backing.id}",
                        )
                resolved_spec = {
                    "primary": {
                        "model_id": primary.id,
                        "checkpoint_sha256": self._checkpoint_identity(
                            primary.public_dict()
                        ),
                        "filename": primary.filename,
                        "options": primary.options_for(request.preset),
                    },
                    "backing_vocals": (
                        {
                            "model_id": backing.id,
                            "checkpoint_sha256": self._checkpoint_identity(
                                backing.public_dict()
                            ),
                            "filename": backing.filename,
                            "options": backing.options_for("fast"),
                        }
                        if backing is not None
                        else None
                    ),
                    "backend": self._backend,
                    "output_layout": request.output_layout,
                    "pipeline_version": 1,
                    "runtime": _runtime_fingerprint(self._backend),
                    "seed": 0,
                }
            cacheable = resolved_spec["primary"]["checkpoint_sha256"] is not None and (
                resolved_spec["backing_vocals"] is None
                or resolved_spec["backing_vocals"]["checkpoint_sha256"] is not None
            )
            cache_key = hashlib.sha256(
                json.dumps(
                    {
                        "input_sha256": self._database.execute(
                            "SELECT sha256 FROM inputs WHERE id = ?",
                            (request.input_id,),
                        ).fetchone()["sha256"],
                        "resolved_spec": resolved_spec,
                        "uncacheable_nonce": None if cacheable else uuid.uuid4().hex,
                    },
                    sort_keys=True,
                ).encode()
            ).hexdigest()
            job_id = _resource_id("job")
            now = time.time()
            if not request.force:
                cached = self._database.execute(
                    """
                    SELECT result_json FROM jobs
                    WHERE cache_key = ? AND status = 'completed' AND result_json IS NOT NULL
                    ORDER BY updated_at DESC LIMIT 1
                    """,
                    (cache_key,),
                ).fetchone()
                if cached is not None:
                    result = json.loads(cached["result_json"])
                    result["provenance"]["cache_hit"] = True
                    self._database.execute(
                        """
                        INSERT INTO jobs(
                            id, input_id, model_id, preset, output_layout, status, stage,
                            token_id, idempotency_key, cache_key, result_json, error_json,
                            resolved_spec_json, created_at, updated_at
                        ) VALUES (?, ?, ?, ?, ?, 'completed', 'completed', ?, ?, ?, ?, NULL, ?, ?, ?)
                        """,
                        (
                            job_id,
                            request.input_id,
                            request.model_id,
                            request.preset,
                            request.output_layout,
                            token_id,
                            idempotency_key,
                            cache_key,
                            json.dumps(result),
                            json.dumps(resolved_spec),
                            now,
                            now,
                        ),
                    )
                    for artifact in result["artifacts"]:
                        self._database.execute(
                            "INSERT INTO job_artifacts(job_id, artifact_id, role) VALUES (?, ?, ?)",
                            (job_id, artifact["artifact_id"], artifact["role"]),
                        )
                    self._database.commit()
                    return {"job_id": job_id, "status": "completed"}
            queued = self._database.execute(
                "SELECT COUNT(*) AS count FROM jobs WHERE status = 'queued'"
            ).fetchone()["count"]
            if queued >= self._config.max_queued_jobs:
                raise _api_error(
                    status.HTTP_429_TOO_MANY_REQUESTS,
                    "queue_full",
                    "job queue is full",
                )
            self._database.execute(
                """
                INSERT INTO jobs(
                    id, input_id, model_id, preset, output_layout, status, stage,
                    token_id, idempotency_key, cache_key, result_json, error_json,
                    resolved_spec_json, created_at, updated_at
                ) VALUES (?, ?, ?, ?, ?, 'queued', 'queued', ?, ?, ?, NULL, NULL, ?, ?, ?)
                """,
                (
                    job_id,
                    request.input_id,
                    request.model_id,
                    request.preset,
                    request.output_layout,
                    token_id,
                    idempotency_key,
                    cache_key,
                    json.dumps(resolved_spec),
                    now,
                    now,
                ),
            )
            self._database.commit()
            self._condition.notify_all()
            return {"job_id": job_id, "status": "queued"}

    def get_job(self, job_id: str) -> dict[str, Any]:
        with self._lock:
            row = self._database.execute(
                "SELECT * FROM jobs WHERE id = ?", (job_id,)
            ).fetchone()
        if row is None:
            raise _api_error(
                status.HTTP_404_NOT_FOUND, "job_not_found", "job not found"
            )
        response: dict[str, Any] = {
            "job_id": row["id"],
            "input_id": row["input_id"],
            "model_id": row["model_id"],
            "preset": row["preset"],
            "output_layout": row["output_layout"],
            "status": row["status"],
            "stage": row["stage"],
            "resolved_spec": json.loads(row["resolved_spec_json"]),
        }
        if row["result_json"] is not None:
            response["result"] = json.loads(row["result_json"])
        if row["error_json"] is not None:
            response["error"] = json.loads(row["error_json"])
        return response

    def list_jobs(self) -> list[dict[str, Any]]:
        with self._lock:
            identifiers = self._database.execute(
                "SELECT id FROM jobs ORDER BY created_at DESC"
            ).fetchall()
        return [self.get_job(row["id"]) for row in identifiers]

    def list_inputs(self) -> list[dict[str, Any]]:
        with self._lock:
            rows = self._database.execute(
                "SELECT id, size_bytes, sha256, status, created_at FROM inputs ORDER BY created_at DESC"
            ).fetchall()
        return [
            {
                "input_id": row["id"],
                "size_bytes": row["size_bytes"],
                "sha256": row["sha256"],
                "status": row["status"],
                "created_at": row["created_at"],
            }
            for row in rows
        ]

    def delete_input(self, input_id: str) -> None:
        with self._lock:
            row = self._database.execute(
                "SELECT path FROM inputs WHERE id = ?", (input_id,)
            ).fetchone()
            if row is None:
                raise _api_error(
                    status.HTTP_404_NOT_FOUND, "input_not_found", "input not found"
                )
            referenced = self._database.execute(
                "SELECT 1 FROM jobs WHERE input_id = ? LIMIT 1", (input_id,)
            ).fetchone()
            if referenced is not None:
                raise _api_error(
                    status.HTTP_409_CONFLICT,
                    "input_not_ready",
                    "input is referenced by a job",
                )
            self._database.execute("DELETE FROM inputs WHERE id = ?", (input_id,))
            self._database.commit()
            if row["path"] is not None:
                still_used = self._database.execute(
                    "SELECT 1 FROM inputs WHERE path = ? LIMIT 1", (row["path"],)
                ).fetchone()
                if still_used is None:
                    (self._data_dir / row["path"]).unlink(missing_ok=True)

    def delete_job_artifacts(self, job_id: str) -> None:
        with self._lock:
            job = self._database.execute(
                "SELECT status FROM jobs WHERE id = ?", (job_id,)
            ).fetchone()
            if job is None:
                raise _api_error(
                    status.HTTP_404_NOT_FOUND, "job_not_found", "job not found"
                )
            if job["status"] not in {"completed", "failed", "cancelled"}:
                raise _api_error(
                    status.HTTP_409_CONFLICT,
                    "artifact_unavailable",
                    "job artifacts cannot be deleted while active",
                )
            artifacts = self._database.execute(
                """
                SELECT artifacts.id, artifacts.path FROM artifacts
                JOIN job_artifacts ON job_artifacts.artifact_id = artifacts.id
                WHERE job_artifacts.job_id = ?
                """,
                (job_id,),
            ).fetchall()
            self._database.execute(
                "DELETE FROM job_artifacts WHERE job_id = ?", (job_id,)
            )
            self._database.execute(
                "UPDATE jobs SET cache_key = ?, result_json = NULL, updated_at = ? WHERE id = ?",
                (f"deleted:{job_id}", time.time(), job_id),
            )
            for artifact in artifacts:
                referenced = self._database.execute(
                    "SELECT 1 FROM job_artifacts WHERE artifact_id = ? LIMIT 1",
                    (artifact["id"],),
                ).fetchone()
                if referenced is None:
                    self._database.execute(
                        "DELETE FROM artifacts WHERE id = ?", (artifact["id"],)
                    )
                    (self._data_dir / artifact["path"]).unlink(missing_ok=True)
            self._database.commit()

    def metrics(self) -> str:
        with self._lock:
            rows = self._database.execute(
                "SELECT status, COUNT(*) AS count FROM jobs GROUP BY status"
            ).fetchall()
        counts = {row["status"]: row["count"] for row in rows}
        lines = [
            "# HELP k3_separator_jobs Number of separator jobs by durable status.",
            "# TYPE k3_separator_jobs gauge",
        ]
        for job_status in (
            "queued",
            "running",
            "cancelling",
            "cancelled",
            "completed",
            "failed",
        ):
            lines.append(
                f'k3_separator_jobs{{status="{job_status}"}} {counts.get(job_status, 0)}'
            )
        lines.extend(
            [
                "# HELP k3_separator_max_concurrent_jobs Configured inference workers.",
                "# TYPE k3_separator_max_concurrent_jobs gauge",
                f"k3_separator_max_concurrent_jobs {self._config.max_concurrent_jobs}",
            ]
        )
        return "\n".join(lines) + "\n"

    def cancel_job(self, job_id: str) -> dict[str, Any]:
        with self._condition:
            row = self._database.execute(
                "SELECT status FROM jobs WHERE id = ?", (job_id,)
            ).fetchone()
            if row is None:
                raise _api_error(
                    status.HTTP_404_NOT_FOUND, "job_not_found", "job not found"
                )
            current = row["status"]
            if current == "queued":
                new_status = "cancelled"
                stage = "cancelled"
            elif current == "running":
                new_status = "cancelling"
                stage = "cancelling"
            else:
                return {"job_id": job_id, "status": current}
            self._database.execute(
                "UPDATE jobs SET status = ?, stage = ?, updated_at = ? WHERE id = ?",
                (new_status, stage, time.time(), job_id),
            )
            self._database.commit()
            self._condition.notify_all()
            return {"job_id": job_id, "status": new_status}

    def artifact_path(self, artifact_id: str) -> tuple[Path, str]:
        with self._lock:
            row = self._database.execute(
                "SELECT path, media_type FROM artifacts WHERE id = ?", (artifact_id,)
            ).fetchone()
        if row is None:
            raise _api_error(
                status.HTTP_404_NOT_FOUND,
                "artifact_unavailable",
                "artifact not found",
            )
        path = (self._data_dir / row["path"]).resolve()
        if self._data_dir not in path.parents or not path.is_file():
            raise _api_error(
                status.HTTP_404_NOT_FOUND,
                "artifact_unavailable",
                "artifact unavailable",
            )
        return path, row["media_type"]

    def _initialize_database(self) -> None:
        with self._lock:
            self._database.execute("PRAGMA journal_mode = WAL")
            self._database.executescript(
                """
                CREATE TABLE IF NOT EXISTS inputs(
                    id TEXT PRIMARY KEY,
                    filename TEXT NOT NULL,
                    size_bytes INTEGER NOT NULL,
                    sha256 TEXT NOT NULL,
                    status TEXT NOT NULL,
                    path TEXT,
                    created_at REAL NOT NULL
                );
                CREATE INDEX IF NOT EXISTS inputs_sha256 ON inputs(sha256);
                CREATE TABLE IF NOT EXISTS jobs(
                    id TEXT PRIMARY KEY,
                    input_id TEXT NOT NULL REFERENCES inputs(id),
                    model_id TEXT NOT NULL,
                    preset TEXT NOT NULL,
                    output_layout TEXT NOT NULL,
                    status TEXT NOT NULL,
                    stage TEXT NOT NULL,
                    token_id TEXT NOT NULL,
                    idempotency_key TEXT NOT NULL,
                    cache_key TEXT NOT NULL,
                    result_json TEXT,
                    error_json TEXT,
                    resolved_spec_json TEXT NOT NULL,
                    created_at REAL NOT NULL,
                    updated_at REAL NOT NULL,
                    UNIQUE(token_id, idempotency_key)
                );
                CREATE TABLE IF NOT EXISTS artifacts(
                    id TEXT PRIMARY KEY,
                    sha256 TEXT NOT NULL,
                    size_bytes INTEGER NOT NULL,
                    media_type TEXT NOT NULL,
                    path TEXT NOT NULL UNIQUE
                );
                CREATE TABLE IF NOT EXISTS job_artifacts(
                    job_id TEXT NOT NULL REFERENCES jobs(id),
                    artifact_id TEXT NOT NULL REFERENCES artifacts(id),
                    role TEXT NOT NULL,
                    PRIMARY KEY(job_id, role)
                );
                CREATE TABLE IF NOT EXISTS metadata(
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );
                """
            )
            self._database.commit()
            job_columns = {
                row["name"]
                for row in self._database.execute("PRAGMA table_info(jobs)").fetchall()
            }
            if "cache_key" not in job_columns:
                self._database.execute(
                    "ALTER TABLE jobs ADD COLUMN cache_key TEXT NOT NULL DEFAULT ''"
                )
            if "resolved_spec_json" not in job_columns:
                self._database.execute(
                    "ALTER TABLE jobs ADD COLUMN resolved_spec_json TEXT NOT NULL DEFAULT '{}'"
                )
            self._database.commit()
            self._database.execute(
                """
                UPDATE jobs SET status = 'cancelled', stage = 'cancelled', updated_at = ?
                WHERE status = 'cancelling'
                """,
                (time.time(),),
            )
            self._database.execute(
                """
                UPDATE jobs SET status = 'queued', stage = 'queued', updated_at = ?
                WHERE status = 'running'
                """,
                (time.time(),),
            )
            self._database.commit()

    def _load_or_create_server_id(self) -> str:
        with self._lock:
            row = self._database.execute(
                "SELECT value FROM metadata WHERE key = 'server_id'"
            ).fetchone()
            if row is not None:
                return str(row["value"])
            server_id = _resource_id("server")
            self._database.execute(
                "INSERT INTO metadata(key, value) VALUES ('server_id', ?)",
                (server_id,),
            )
            self._database.commit()
            return server_id

    def _run_scheduler(self) -> None:
        while True:
            with self._condition:
                row = self._database.execute(
                    "SELECT * FROM jobs WHERE status = 'queued' ORDER BY created_at LIMIT 1"
                ).fetchone()
                if row is None and not self._stopping:
                    self._condition.wait(timeout=0.5)
                    continue
                if self._stopping:
                    return
                self._database.execute(
                    "DELETE FROM job_artifacts WHERE job_id = ?", (row["id"],)
                )
                self._database.execute(
                    "UPDATE jobs SET status = 'running', stage = 'separating_primary', updated_at = ? WHERE id = ?",
                    (time.time(), row["id"]),
                )
                self._database.commit()
            try:
                result = self._run_job(row)
            except Exception as error:  # noqa: BLE001 - preserve durable protocol state
                with self._lock:
                    self._database.execute(
                        "UPDATE jobs SET status = 'failed', error_json = ?, updated_at = ? WHERE id = ?",
                        (
                            json.dumps(
                                {
                                    "code": "separation_failed",
                                    "message": str(error),
                                    "retryable": False,
                                }
                            ),
                            time.time(),
                            row["id"],
                        ),
                    )
                    self._database.commit()
            else:
                with self._lock:
                    current = self._database.execute(
                        "SELECT status FROM jobs WHERE id = ?", (row["id"],)
                    ).fetchone()["status"]
                    if current == "cancelling":
                        self._database.execute(
                            "UPDATE jobs SET status = 'cancelled', stage = 'cancelled', updated_at = ? WHERE id = ?",
                            (time.time(), row["id"]),
                        )
                    else:
                        self._database.execute(
                            "UPDATE jobs SET status = 'completed', stage = 'completed', result_json = ?, updated_at = ? WHERE id = ?",
                            (json.dumps(result), time.time(), row["id"]),
                        )
                    self._database.commit()

    def _run_job(self, job: sqlite3.Row) -> dict[str, Any]:
        if self._backend == "fake":
            return self._run_fake_job(job)
        return self._run_audio_separator_job(job)

    def _run_audio_separator_job(self, job: sqlite3.Row) -> dict[str, Any]:
        with self._lock:
            input_row = self._database.execute(
                "SELECT path FROM inputs WHERE id = ?", (job["input_id"],)
            ).fetchone()
        if input_row is None or input_row["path"] is None:
            raise RuntimeError("input is unavailable")
        job_dir = self._jobs_dir / job["id"]
        shutil.rmtree(job_dir, ignore_errors=True)
        output_dir = job_dir / "outputs"
        output_dir.mkdir(parents=True)
        model_dir = self._config.model_dir or (self._data_dir / "models")
        if self._executor is None:
            raise RuntimeError("inference process pool is unavailable")
        response = self._executor.submit(
            _execute_inference,
            {
                "model_dir": str(model_dir),
                "registry_path": (
                    str(self._config.registry_path)
                    if self._config.registry_path is not None
                    else None
                ),
                "backend": self._backend,
                "input_path": str(self._data_dir / input_row["path"]),
                "output_dir": str(output_dir),
                "profile": job["preset"],
                "model_id": job["model_id"],
                "preserve_backing_vocals": job["output_layout"] == "karaoke",
            },
        ).result()
        role_paths = (
            {
                "lead_vocals": Path(response["vocals"]),
                "backing_vocals": Path(response["backing_vocals"]),
                "accompaniment": Path(response["accompaniment"]),
            }
            if job["output_layout"] == "karaoke"
            else {
                "vocals": Path(response["vocals"]),
                "accompaniment": Path(response["accompaniment"]),
            }
        )
        _validate_output_wavs(role_paths)
        artifacts = [
            self._publish_artifact(job["id"], role, path)
            for role, path in role_paths.items()
        ]
        provenance = response["provenance"]
        provenance.update(
            {
                "model_id": job["model_id"],
                "preset": job["preset"],
                "output_layout": job["output_layout"],
                "backend": self._backend,
                "cache_hit": False,
            }
        )
        shutil.rmtree(job_dir, ignore_errors=True)
        return {"artifacts": artifacts, "provenance": provenance}

    def _publish_artifact(self, job_id: str, role: str, source: Path) -> dict[str, Any]:
        digest = _hash_file(source)
        size_bytes = source.stat().st_size
        relative = Path("artifacts") / "sha256" / digest
        destination = self._data_dir / relative
        if not destination.exists():
            temporary = destination.with_name(f".{digest}.{job_id}.partial")
            shutil.copyfile(source, temporary)
            with temporary.open("rb") as artifact:
                os.fsync(artifact.fileno())
            os.replace(temporary, destination)
        with self._lock:
            existing = self._database.execute(
                "SELECT id FROM artifacts WHERE path = ?", (relative.as_posix(),)
            ).fetchone()
            artifact_id = (
                existing["id"] if existing is not None else _resource_id("artifact")
            )
            if existing is None:
                self._database.execute(
                    "INSERT INTO artifacts(id, sha256, size_bytes, media_type, path) VALUES (?, ?, ?, 'audio/wav', ?)",
                    (artifact_id, digest, size_bytes, relative.as_posix()),
                )
            self._database.execute(
                "INSERT INTO job_artifacts(job_id, artifact_id, role) VALUES (?, ?, ?)",
                (job_id, artifact_id, role),
            )
            self._database.commit()
        return {
            "role": role,
            "artifact_id": artifact_id,
            "media_type": "audio/wav",
            "size_bytes": size_bytes,
            "sha256": digest,
        }

    def _run_fake_job(self, job: sqlite3.Row) -> dict[str, Any]:
        if self._backend != "fake":
            raise RuntimeError(f"runtime is not implemented: {self._backend}")
        if self._config.fake_job_delay:
            time.sleep(self._config.fake_job_delay)
        roles = ["vocals", "accompaniment"]
        if job["output_layout"] == "karaoke":
            roles = ["lead_vocals", "backing_vocals", "accompaniment"]
        artifacts = []
        for index, role in enumerate(roles, start=1):
            wav = _fake_float_wav(0.05 * index)
            digest = hashlib.sha256(wav).hexdigest()
            relative = Path("artifacts") / "sha256" / digest
            destination = self._data_dir / relative
            if not destination.exists():
                temporary = destination.with_name(f".{digest}.{job['id']}.partial")
                with temporary.open("xb") as output:
                    output.write(wav)
                    output.flush()
                    os.fsync(output.fileno())
                os.replace(temporary, destination)
            with self._lock:
                existing = self._database.execute(
                    "SELECT id FROM artifacts WHERE path = ?", (relative.as_posix(),)
                ).fetchone()
                artifact_id = (
                    existing["id"] if existing is not None else _resource_id("artifact")
                )
                if existing is None:
                    self._database.execute(
                        "INSERT INTO artifacts(id, sha256, size_bytes, media_type, path) VALUES (?, ?, ?, 'audio/wav', ?)",
                        (artifact_id, digest, len(wav), relative.as_posix()),
                    )
                self._database.execute(
                    "INSERT INTO job_artifacts(job_id, artifact_id, role) VALUES (?, ?, ?)",
                    (job["id"], artifact_id, role),
                )
                self._database.commit()
            artifacts.append(
                {
                    "role": role,
                    "artifact_id": artifact_id,
                    "media_type": "audio/wav",
                    "size_bytes": len(wav),
                    "sha256": digest,
                }
            )
        return {
            "artifacts": artifacts,
            "provenance": {
                "provider": "k3-test",
                "architecture": "fake",
                "checkpoint_id": job["model_id"],
                "checkpoint_sha256": "0" * 64,
                "model_id": job["model_id"],
                "preset": job["preset"],
                "output_layout": job["output_layout"],
                "backend": "fake",
                "cache_hit": False,
                **(
                    {
                        "backing_vocals_model": {
                            "provider": "k3-test",
                            "architecture": "fake",
                            "checkpoint_id": "fake-backing-vocals",
                            "checkpoint_sha256": "1" * 64,
                        }
                    }
                    if job["output_layout"] == "karaoke"
                    else {}
                ),
            },
        }


def _execute_inference(config: dict[str, Any]) -> dict[str, Any]:
    """Run one model request inside a spawned process without touching SQLite."""

    from .models import ModelRegistry
    from .runtime import AudioSeparatorRuntime
    from .service import SeparationService

    registry_path = (
        Path(config["registry_path"]) if config["registry_path"] is not None else None
    )
    if config["backend"] == "cuda":
        import torch

        torch.cuda.reset_peak_memory_stats()
    service = SeparationService(
        ModelRegistry.load(registry_path),
        AudioSeparatorRuntime(Path(config["model_dir"]), backend=config["backend"]),
    )
    result = service.handle(
        {
            "method": "separate",
            "params": {
                "input_path": config["input_path"],
                "output_dir": config["output_dir"],
                "profile": config["profile"],
                "model_id": config["model_id"],
                "overwrite": True,
                "preserve_backing_vocals": config["preserve_backing_vocals"],
                "options": {"autocast": config["backend"] == "cuda"},
            },
        }
    )
    if config["backend"] == "cuda":
        result["_worker_metrics"] = {
            "peak_cuda_bytes": int(torch.cuda.max_memory_allocated())
        }
    return result


def create_app(config: ServerConfig) -> FastAPI:
    """Build the HTTP adapter around one separator server module."""

    server = SeparatorServer(config)

    @asynccontextmanager
    async def lifespan(_app: FastAPI):
        server.start()
        try:
            yield
        finally:
            server.close()

    app = FastAPI(title="K3 Separator Server", version="1.0.0", lifespan=lifespan)

    @app.exception_handler(HTTPException)
    async def http_error(_request: Request, error: HTTPException) -> JSONResponse:
        if isinstance(error.detail, dict) and {
            "code",
            "message",
            "retryable",
        }.issubset(error.detail):
            payload = error.detail
        else:
            codes = {
                status.HTTP_401_UNAUTHORIZED: "unauthorized",
                status.HTTP_413_CONTENT_TOO_LARGE: "input_rejected",
                status.HTTP_404_NOT_FOUND: "not_found",
                status.HTTP_409_CONFLICT: "conflict",
                status.HTTP_422_UNPROCESSABLE_CONTENT: "invalid_request",
                status.HTTP_429_TOO_MANY_REQUESTS: "queue_full",
                status.HTTP_507_INSUFFICIENT_STORAGE: "insufficient_storage",
            }
            payload = {
                "code": codes.get(error.status_code, "request_failed"),
                "message": str(error.detail),
                "retryable": error.status_code >= 500,
            }
        return JSONResponse(
            status_code=error.status_code,
            content={"error": payload},
            headers=error.headers,
        )

    @app.exception_handler(RequestValidationError)
    async def validation_error(
        _request: Request, error: RequestValidationError
    ) -> JSONResponse:
        return JSONResponse(
            status_code=status.HTTP_422_UNPROCESSABLE_CONTENT,
            content={
                "error": {
                    "code": "invalid_request",
                    "message": "request validation failed",
                    "retryable": False,
                    "details": {"errors": error.errors()},
                }
            },
        )

    def authenticated_token(
        authorization: Annotated[str | None, Header()] = None,
    ) -> str:
        return server.authenticate(authorization)

    @app.get("/healthz")
    def health() -> dict[str, str]:
        return {"status": "ok"}

    @app.get("/readyz")
    def readiness() -> dict[str, Any]:
        return server.readiness()

    @app.get("/metrics", response_class=PlainTextResponse)
    def metrics(
        _token_id: str = Depends(authenticated_token),
    ) -> str:
        return server.metrics()

    @app.get("/v1/capabilities")
    def capabilities(
        _token_id: str = Depends(authenticated_token),
    ) -> dict[str, Any]:
        return server.capabilities()

    @app.get("/v1/models")
    def models(
        _token_id: str = Depends(authenticated_token),
    ) -> dict[str, Any]:
        return {"models": server.models()}

    @app.websocket("/v1/events")
    async def events(websocket: WebSocket) -> None:
        try:
            server.authenticate(websocket.headers.get("authorization"))
        except HTTPException:
            await websocket.close(code=4401, reason="unauthorized")
            return
        await websocket.accept()
        subscribed: set[str] = set()
        last_snapshot: dict[str, tuple[str, str]] = {}
        sequence = 0
        try:
            while True:
                try:
                    message = await asyncio.wait_for(
                        websocket.receive_json(), timeout=0.1
                    )
                except TimeoutError:
                    message = None
                if message is not None:
                    action = message.get("action")
                    job_ids = message.get("job_ids", [])
                    if action == "subscribe" and isinstance(job_ids, list):
                        subscribed.update(str(job_id) for job_id in job_ids)
                    elif action == "unsubscribe" and isinstance(job_ids, list):
                        subscribed.difference_update(str(job_id) for job_id in job_ids)
                    else:
                        await websocket.send_json(
                            {
                                "error": {
                                    "code": "invalid_request",
                                    "message": "expected subscribe or unsubscribe",
                                    "retryable": False,
                                }
                            }
                        )
                for job_id in tuple(subscribed):
                    try:
                        job = server.get_job(job_id)
                    except HTTPException:
                        subscribed.remove(job_id)
                        continue
                    snapshot = (job["status"], job["stage"])
                    if last_snapshot.get(job_id) == snapshot:
                        continue
                    last_snapshot[job_id] = snapshot
                    sequence += 1
                    await websocket.send_json(
                        {
                            "job_id": job_id,
                            "sequence": sequence,
                            "status": job["status"],
                            "stage": job["stage"],
                            "message": _progress_message(job["stage"]),
                        }
                    )
        except WebSocketDisconnect:
            return

    @app.post("/v1/inputs")
    def create_input(
        request: InputRequest,
        response: Response,
        _token_id: str = Depends(authenticated_token),
    ) -> dict[str, Any]:
        result, response.status_code = server.create_input(request)
        return result

    @app.put("/v1/inputs/{input_id}/content")
    async def upload_input(
        input_id: str,
        request: Request,
        _token_id: str = Depends(authenticated_token),
    ) -> dict[str, Any]:
        return await server.upload_input(input_id, request)

    @app.get("/v1/inputs")
    def list_inputs(
        _token_id: str = Depends(authenticated_token),
    ) -> dict[str, Any]:
        return {"inputs": server.list_inputs()}

    @app.delete("/v1/inputs/{input_id}", status_code=status.HTTP_204_NO_CONTENT)
    def delete_input(
        input_id: str, _token_id: str = Depends(authenticated_token)
    ) -> Response:
        server.delete_input(input_id)
        return Response(status_code=status.HTTP_204_NO_CONTENT)

    @app.post("/v1/jobs", status_code=status.HTTP_202_ACCEPTED)
    def create_job(
        request: JobRequest,
        idempotency_key: Annotated[str, Header(alias="Idempotency-Key")],
        token_id: str = Depends(authenticated_token),
    ) -> dict[str, Any]:
        return server.create_job(token_id, idempotency_key, request)

    @app.get("/v1/jobs/{job_id}")
    def get_job(
        job_id: str, _token_id: str = Depends(authenticated_token)
    ) -> dict[str, Any]:
        return server.get_job(job_id)

    @app.get("/v1/jobs")
    def list_jobs(
        _token_id: str = Depends(authenticated_token),
    ) -> dict[str, Any]:
        return {"jobs": server.list_jobs()}

    @app.post("/v1/jobs/{job_id}/cancel")
    def cancel_job(
        job_id: str, _token_id: str = Depends(authenticated_token)
    ) -> dict[str, Any]:
        return server.cancel_job(job_id)

    @app.delete("/v1/jobs/{job_id}/artifacts", status_code=status.HTTP_204_NO_CONTENT)
    def delete_job_artifacts(
        job_id: str, _token_id: str = Depends(authenticated_token)
    ) -> Response:
        server.delete_job_artifacts(job_id)
        return Response(status_code=status.HTTP_204_NO_CONTENT)

    @app.get("/v1/artifacts/{artifact_id}")
    def download_artifact(
        artifact_id: str,
        _token_id: str = Depends(authenticated_token),
    ) -> FileResponse:
        path, media_type = server.artifact_path(artifact_id)
        return FileResponse(path, media_type=media_type)

    return app


def _resource_id(kind: str) -> str:
    return f"{kind}_{uuid.uuid4().hex}"


def _progress_message(stage: str) -> str:
    return stage.replace("_", " ").capitalize()


def _hash_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def _runtime_fingerprint(backend: str) -> dict[str, Any]:
    versions: dict[str, str | None] = {}
    for distribution in ("audio-separator", "torch", "onnxruntime"):
        try:
            versions[distribution] = importlib.metadata.version(distribution)
        except importlib.metadata.PackageNotFoundError:
            versions[distribution] = None
    return {
        "k3_separator": "0.1.0",
        "python": platform.python_version(),
        "dependencies": versions,
        "precision": "float16" if backend == "cuda" else "float32",
    }


def _probe_audio(path: Path, max_duration_seconds: int, max_channels: int) -> None:
    try:
        completed = subprocess.run(
            [
                "ffprobe",
                "-v",
                "error",
                "-show_entries",
                "format=duration:stream=codec_type,channels",
                "-of",
                "json",
                str(path),
            ],
            check=True,
            capture_output=True,
            text=True,
            timeout=30,
        )
        document = json.loads(completed.stdout)
        duration = float(document.get("format", {}).get("duration", 0))
        audio_streams = [
            stream
            for stream in document.get("streams", [])
            if stream.get("codec_type") == "audio"
        ]
        channels = max(
            (int(stream.get("channels", 0)) for stream in audio_streams), default=0
        )
    except (
        OSError,
        ValueError,
        json.JSONDecodeError,
        subprocess.SubprocessError,
    ) as error:
        raise _api_error(
            status.HTTP_422_UNPROCESSABLE_CONTENT,
            "input_rejected",
            "uploaded content is not a supported audio file",
        ) from error
    if not audio_streams or duration <= 0:
        raise _api_error(
            status.HTTP_422_UNPROCESSABLE_CONTENT,
            "input_rejected",
            "uploaded content has no usable audio stream",
        )
    if duration > max_duration_seconds:
        raise _api_error(
            status.HTTP_422_UNPROCESSABLE_CONTENT,
            "input_rejected",
            "uploaded audio exceeds the configured duration limit",
        )
    if channels > max_channels:
        raise _api_error(
            status.HTTP_422_UNPROCESSABLE_CONTENT,
            "input_rejected",
            "uploaded audio exceeds the configured channel limit",
        )


def _validate_output_wavs(role_paths: dict[str, Path]) -> None:
    try:
        import numpy as np
        import soundfile as sf
    except ImportError as error:
        raise RuntimeError(
            "numpy and soundfile are required to validate separator outputs"
        ) from error

    expected_frames: int | None = None
    for role, path in role_paths.items():
        try:
            with sf.SoundFile(path) as audio:
                valid_format = (
                    audio.format == "WAV"
                    and audio.subtype == "FLOAT"
                    and audio.samplerate == 44_100
                    and audio.channels == 2
                    and audio.frames > 0
                )
                if not valid_format:
                    raise RuntimeError(
                        f"{role} must be a non-empty 44.1 kHz stereo 32-bit float WAV"
                    )
                frames = int(audio.frames)
                for block in audio.blocks(blocksize=65_536, dtype="float32"):
                    if not np.isfinite(block).all():
                        raise RuntimeError(f"{role} contains non-finite samples")
        except (OSError, sf.LibsndfileError) as error:
            raise RuntimeError(f"{role} is not a readable WAV: {error}") from error
        if expected_frames is not None and frames != expected_frames:
            raise RuntimeError("separator outputs have different frame counts")
        expected_frames = frames


def _fake_float_wav(level: float) -> bytes:
    channels = 2
    sample_rate = 44_100
    frames = 441
    samples = struct.pack(f"<{frames * channels}f", *([level] * frames * channels))
    byte_rate = sample_rate * channels * 4
    block_align = channels * 4
    header = struct.pack(
        "<4sI4s4sIHHIIHH4sI",
        b"RIFF",
        36 + len(samples),
        b"WAVE",
        b"fmt ",
        16,
        3,
        channels,
        sample_rate,
        byte_rate,
        block_align,
        32,
        b"data",
        len(samples),
    )
    return header + samples


def main() -> None:
    parser = argparse.ArgumentParser(description="Run the K3 separator server")
    parser.add_argument("--data-dir", type=Path, required=True)
    credentials = parser.add_mutually_exclusive_group(required=True)
    credentials.add_argument(
        "--token",
        action="append",
        help="TOKEN_ID=TOKEN; may be repeated (prefer --token-env)",
    )
    credentials.add_argument(
        "--token-env",
        action="append",
        help="TOKEN_ID=ENV_NAME; may be repeated",
    )
    parser.add_argument("--token-id", default="default")
    parser.add_argument(
        "--runtime", choices=("fake", "cpu", "cuda", "coreml", "auto"), default="fake"
    )
    parser.add_argument("--model-dir", type=Path)
    parser.add_argument(
        "--models", type=Path, help="JSON registry merged over built-ins"
    )
    parser.add_argument("--max-jobs", type=int, default=1)
    parser.add_argument("--max-queued-jobs", type=int, default=32)
    parser.add_argument("--max-upload-gib", type=float, default=2.0)
    parser.add_argument("--max-duration-hours", type=float, default=4.0)
    parser.add_argument("--max-input-channels", type=int, default=8)
    parser.add_argument("--min-free-gib", type=float, default=0.5)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8765)
    parser.add_argument(
        "--public-behind-proxy",
        action="store_true",
        help="confirm that a non-loopback listener is protected by TLS/VPN",
    )
    args = parser.parse_args()
    if (
        args.host not in {"127.0.0.1", "::1", "localhost"}
        and not args.public_behind_proxy
    ):
        parser.error(
            "non-loopback --host requires --public-behind-proxy and a protected TLS/VPN path"
        )
    tokens: dict[str, str] = {}
    if args.token:
        for value in args.token:
            token_id, separator, token = value.partition("=")
            if not separator:
                token_id, token = args.token_id, value
            if not token_id or not token:
                parser.error("--token must contain TOKEN_ID=TOKEN")
            tokens[token_id] = token
    if args.token_env:
        for value in args.token_env:
            token_id, separator, variable = value.partition("=")
            if not separator or not token_id or not variable:
                parser.error("--token-env must contain TOKEN_ID=ENV_NAME")
            token = os.environ.get(variable)
            if not token:
                parser.error(f"environment variable is empty or missing: {variable}")
            tokens[token_id] = token
    try:
        import uvicorn
    except ImportError as error:
        raise SystemExit(
            "install k3-separator[server] to run the HTTP server"
        ) from error
    app = create_app(
        ServerConfig(
            data_dir=args.data_dir,
            tokens=tokens,
            runtime=args.runtime,
            model_dir=args.model_dir,
            registry_path=args.models,
            max_concurrent_jobs=args.max_jobs,
            max_queued_jobs=args.max_queued_jobs,
            max_upload_bytes=int(args.max_upload_gib * 1024**3),
            max_duration_seconds=int(args.max_duration_hours * 60 * 60),
            max_input_channels=args.max_input_channels,
            min_free_bytes=int(args.min_free_gib * 1024**3),
        )
    )
    uvicorn.run(app, host=args.host, port=args.port)


if __name__ == "__main__":
    main()
