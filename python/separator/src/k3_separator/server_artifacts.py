"""Content-addressed artifact storage with transactional set publication."""

from __future__ import annotations

import json
import os
import shutil
import sqlite3
import threading
import time
from pathlib import Path
from typing import Any, Protocol

from .server_utils import hash_file, resource_id


class PublishableResult(Protocol):
    role_paths: dict[str, Path]
    provenance: dict[str, Any]


class ArtifactStore:
    """Publishes a complete stem set or leaves the database unchanged."""

    def __init__(
        self,
        data_dir: Path,
        database: sqlite3.Connection,
        lock: threading.RLock,
    ) -> None:
        self._data_dir = data_dir
        self._database = database
        self._lock = lock

    def publish_set(self, job_id: str, pending: PublishableResult) -> None:
        artifacts = self._materialize_files(job_id, pending.role_paths)
        with self._lock:
            try:
                self._database.execute("BEGIN IMMEDIATE")
                for artifact in artifacts:
                    existing = self._database.execute(
                        "SELECT id FROM artifacts WHERE path = ?", (artifact["_path"],)
                    ).fetchone()
                    artifact_id = (
                        existing["id"]
                        if existing is not None
                        else resource_id("artifact")
                    )
                    if existing is None:
                        self._database.execute(
                            """
                            INSERT INTO artifacts(
                                id, sha256, size_bytes, media_type, path
                            ) VALUES (?, ?, ?, 'audio/wav', ?)
                            """,
                            (
                                artifact_id,
                                artifact["sha256"],
                                artifact["size_bytes"],
                                artifact["_path"],
                            ),
                        )
                    artifact["artifact_id"] = artifact_id
                    self._database.execute(
                        """
                        INSERT INTO job_artifacts(job_id, artifact_id, role)
                        VALUES (?, ?, ?)
                        """,
                        (job_id, artifact_id, artifact["role"]),
                    )
                public_artifacts = [
                    {key: value for key, value in artifact.items() if key != "_path"}
                    for artifact in artifacts
                ]
                result = {
                    "artifacts": public_artifacts,
                    "provenance": pending.provenance,
                }
                self._database.execute(
                    """
                    UPDATE jobs
                    SET status = 'completed', stage = 'completed', result_json = ?,
                        error_json = NULL, updated_at = ?
                    WHERE id = ?
                    """,
                    (json.dumps(result), time.time(), job_id),
                )
                self._database.commit()
            except Exception:
                self._database.rollback()
                raise

    def _materialize_files(
        self, job_id: str, role_paths: dict[str, Path]
    ) -> list[dict[str, Any]]:
        artifacts: list[dict[str, Any]] = []
        for role, source in role_paths.items():
            digest = hash_file(source)
            size_bytes = source.stat().st_size
            relative = Path("artifacts") / "sha256" / digest
            destination = self._data_dir / relative
            if destination.exists():
                if hash_file(destination) != digest:
                    raise RuntimeError(
                        f"stored artifact failed content-address verification: {digest}"
                    )
            else:
                temporary = destination.with_name(f".{digest}.{job_id}.partial")
                shutil.copyfile(source, temporary)
                with temporary.open("rb") as artifact:
                    os.fsync(artifact.fileno())
                os.replace(temporary, destination)
            artifacts.append(
                {
                    "role": role,
                    "artifact_id": "",
                    "media_type": "audio/wav",
                    "size_bytes": size_bytes,
                    "sha256": digest,
                    "_path": relative.as_posix(),
                }
            )
        return artifacts
