"""Persistent separator domain service and lifecycle coordinator."""

from __future__ import annotations

import hashlib
import hmac
import json
import os
import shutil
import sqlite3
import threading
import time
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from fastapi import Request, status

from .server_catalog import CatalogConfig, ModelCatalog
from .server_contracts import InputRequest, JobRequest, api_error
from .server_jobs import JobDispatcher
from .server_runtime import probe_audio
from .server_utils import resource_id


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
        self._catalog = ModelCatalog(
            CatalogConfig(
                backend=self._backend,
                data_dir=self._data_dir,
                model_dir=config.model_dir,
                registry_path=config.registry_path,
            )
        )
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
        if config.max_concurrent_jobs < 1:
            raise ValueError("max_concurrent_jobs must be at least one")
        self._initialize_database()
        self._server_id = self._load_or_create_server_id()
        self._dispatcher = JobDispatcher(
            config,
            self._backend,
            self._data_dir,
            self._jobs_dir,
            self._database,
            self._lock,
            self._condition,
        )

    def start(self) -> None:
        self._dispatcher.start()

    def close(self) -> None:
        self._dispatcher.close()
        with self._lock:
            self._database.close()

    def authenticate(self, authorization: str | None) -> str:
        if authorization is None or not authorization.startswith("Bearer "):
            raise api_error(
                status.HTTP_401_UNAUTHORIZED,
                "unauthorized",
                "missing bearer token",
            )
        supplied = authorization.removeprefix("Bearer ")
        supplied_hash = hashlib.sha256(supplied.encode()).digest()
        for token_id, expected_hash in self._token_hashes.items():
            if hmac.compare_digest(supplied_hash, expected_hash):
                return token_id
        raise api_error(
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
        return self._catalog.models()

    def readiness(self) -> dict[str, Any]:
        return self._catalog.readiness()

    def create_input(self, request: InputRequest) -> tuple[dict[str, Any], int]:
        if request.size_bytes > self._config.max_upload_bytes:
            raise api_error(
                status.HTTP_413_CONTENT_TOO_LARGE,
                "input_rejected",
                "input exceeds the configured upload limit",
            )
        free_bytes = shutil.disk_usage(self._data_dir).free
        if free_bytes - request.size_bytes < self._config.min_free_bytes:
            raise api_error(
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
            input_id = resource_id("input")
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
            raise api_error(
                status.HTTP_404_NOT_FOUND, "input_not_found", "input not found"
            )
        if row["status"] != "uploading":
            raise api_error(
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
                        raise api_error(
                            status.HTTP_422_UNPROCESSABLE_CONTENT,
                            "checksum_mismatch",
                            "uploaded content exceeds declared size",
                        )
                    digest.update(chunk)
                    destination.write(chunk)
                destination.flush()
                os.fsync(destination.fileno())
            if received != row["size_bytes"] or digest.hexdigest() != row["sha256"]:
                raise api_error(
                    status.HTTP_422_UNPROCESSABLE_CONTENT,
                    "checksum_mismatch",
                    "uploaded content does not match declared size and SHA-256",
                )
            if self._backend != "fake":
                probe_audio(
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
            raise api_error(
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
                raise api_error(
                    status.HTTP_404_NOT_FOUND, "input_not_found", "input not found"
                )
            if input_row["status"] != "ready":
                raise api_error(
                    status.HTTP_409_CONFLICT,
                    "input_not_ready",
                    "input is not ready",
                )
            resolved_spec = self._catalog.resolve(request)
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
            job_id = resource_id("job")
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
                raise api_error(
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
            raise api_error(status.HTTP_404_NOT_FOUND, "job_not_found", "job not found")
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
                raise api_error(
                    status.HTTP_404_NOT_FOUND, "input_not_found", "input not found"
                )
            referenced = self._database.execute(
                "SELECT 1 FROM jobs WHERE input_id = ? LIMIT 1", (input_id,)
            ).fetchone()
            if referenced is not None:
                raise api_error(
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
                raise api_error(
                    status.HTTP_404_NOT_FOUND, "job_not_found", "job not found"
                )
            if job["status"] not in {"completed", "failed", "cancelled"}:
                raise api_error(
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
                raise api_error(
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
            raise api_error(
                status.HTTP_404_NOT_FOUND,
                "artifact_unavailable",
                "artifact not found",
            )
        path = (self._data_dir / row["path"]).resolve()
        if self._data_dir not in path.parents or not path.is_file():
            raise api_error(
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
            server_id = resource_id("server")
            self._database.execute(
                "INSERT INTO metadata(key, value) VALUES ('server_id', ?)",
                (server_id,),
            )
            self._database.commit()
            return server_id
