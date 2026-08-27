"""Bounded inference dispatcher and atomic artifact-set publisher."""

from __future__ import annotations

import concurrent.futures
import json
import multiprocessing
import shutil
import sqlite3
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Protocol

from .server_artifacts import ArtifactStore
from .server_runtime import (
    execute_inference,
    fake_float_wav,
    validate_output_wavs,
)


@dataclass(frozen=True)
class PendingResult:
    """Validated output files that are not visible through the API yet."""

    role_paths: dict[str, Path]
    provenance: dict[str, Any]
    cleanup_dir: Path


class DispatcherConfig(Protocol):
    max_concurrent_jobs: int
    fake_job_delay: float
    model_dir: Path | None
    registry_path: Path | None


class JobDispatcher:
    """Runs only the configured number of jobs and owns their state transitions."""

    def __init__(
        self,
        config: DispatcherConfig,
        backend: str,
        data_dir: Path,
        jobs_dir: Path,
        database: sqlite3.Connection,
        lock: threading.RLock,
        condition: threading.Condition,
    ) -> None:
        self._config = config
        self._backend = backend
        self._data_dir = data_dir
        self._jobs_dir = jobs_dir
        self._database = database
        self._lock = lock
        self._condition = condition
        self._artifacts = ArtifactStore(data_dir, database, lock)
        self._stopping = False
        if backend == "fake":
            self._executor: concurrent.futures.Executor = (
                concurrent.futures.ThreadPoolExecutor(
                    max_workers=config.max_concurrent_jobs,
                    thread_name_prefix="k3-separator-fake",
                )
            )
        else:
            self._executor = concurrent.futures.ProcessPoolExecutor(
                max_workers=config.max_concurrent_jobs,
                mp_context=multiprocessing.get_context("spawn"),
            )
        self._scheduler = threading.Thread(
            target=self._run_scheduler,
            name="k3-separator-dispatcher",
            daemon=True,
        )

    def start(self) -> None:
        self._scheduler.start()

    def close(self) -> None:
        with self._condition:
            self._stopping = True
            self._condition.notify_all()
        self._scheduler.join()
        self._executor.shutdown(wait=True, cancel_futures=True)

    def _run_scheduler(self) -> None:
        in_flight: dict[concurrent.futures.Future[Any], sqlite3.Row] = {}
        while True:
            with self._condition:
                while (
                    not self._stopping
                    and len(in_flight) < self._config.max_concurrent_jobs
                ):
                    row = self._database.execute(
                        "SELECT * FROM jobs WHERE status = 'queued' ORDER BY created_at LIMIT 1"
                    ).fetchone()
                    if row is None:
                        break
                    self._database.execute(
                        "DELETE FROM job_artifacts WHERE job_id = ?", (row["id"],)
                    )
                    self._database.execute(
                        """
                        UPDATE jobs
                        SET status = 'running', stage = 'separating_primary', updated_at = ?
                        WHERE id = ? AND status = 'queued'
                        """,
                        (time.time(), row["id"]),
                    )
                    self._database.commit()
                    try:
                        future = self._submit_job(row)
                    except Exception as error:  # noqa: BLE001 - durable protocol boundary
                        self._fail_job(row, error)
                        continue
                    in_flight[future] = row

                if self._stopping and not in_flight:
                    return
                if not in_flight:
                    self._condition.wait(timeout=0.5)
                    continue

            done, _pending = concurrent.futures.wait(
                tuple(in_flight),
                timeout=0.2,
                return_when=concurrent.futures.FIRST_COMPLETED,
            )
            for future in done:
                job = in_flight.pop(future)
                try:
                    response = future.result()
                    pending_result = (
                        response
                        if isinstance(response, PendingResult)
                        else self._collect_audio_separator_result(job, response)
                    )
                    self._finish_job(job, pending_result)
                except Exception as error:  # noqa: BLE001 - durable protocol boundary
                    self._fail_job(job, error)

    def _finish_job(self, job: sqlite3.Row, pending: PendingResult) -> None:
        try:
            with self._lock:
                current = self._database.execute(
                    "SELECT status FROM jobs WHERE id = ?", (job["id"],)
                ).fetchone()["status"]
            if current == "cancelling":
                with self._lock:
                    self._database.execute(
                        """
                        UPDATE jobs
                        SET status = 'cancelled', stage = 'cancelled', updated_at = ?
                        WHERE id = ?
                        """,
                        (time.time(), job["id"]),
                    )
                    self._database.commit()
                return
            self._artifacts.publish_set(job["id"], pending)
        finally:
            shutil.rmtree(pending.cleanup_dir, ignore_errors=True)

    def _fail_job(self, job: sqlite3.Row, error: Exception) -> None:
        shutil.rmtree(self._jobs_dir / job["id"], ignore_errors=True)
        with self._lock:
            self._database.execute(
                """
                UPDATE jobs
                SET status = 'failed', stage = 'failed', error_json = ?, updated_at = ?
                WHERE id = ?
                """,
                (
                    json.dumps(
                        {
                            "code": "separation_failed",
                            "message": str(error),
                            "retryable": False,
                        }
                    ),
                    time.time(),
                    job["id"],
                ),
            )
            self._database.commit()

    def _submit_job(self, job: sqlite3.Row) -> concurrent.futures.Future[Any]:
        if self._backend == "fake":
            return self._executor.submit(self._run_fake_job, job)
        return self._executor.submit(execute_inference, self._inference_request(job))

    def _inference_request(self, job: sqlite3.Row) -> dict[str, Any]:
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
        return {
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
        }

    def _collect_audio_separator_result(
        self, job: sqlite3.Row, response: dict[str, Any]
    ) -> PendingResult:
        job_dir = self._jobs_dir / job["id"]
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
        validate_output_wavs(role_paths)
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
        return PendingResult(role_paths, provenance, job_dir)

    def _run_fake_job(self, job: sqlite3.Row) -> PendingResult:
        if self._config.fake_job_delay:
            time.sleep(self._config.fake_job_delay)
        job_dir = self._jobs_dir / job["id"]
        shutil.rmtree(job_dir, ignore_errors=True)
        output_dir = job_dir / "outputs"
        output_dir.mkdir(parents=True)
        roles = ["vocals", "accompaniment"]
        if job["output_layout"] == "karaoke":
            roles = ["lead_vocals", "backing_vocals", "accompaniment"]
        role_paths: dict[str, Path] = {}
        for index, role in enumerate(roles, start=1):
            path = output_dir / f"{role}.wav"
            path.write_bytes(fake_float_wav(0.05 * index))
            role_paths[role] = path
        validate_output_wavs(role_paths)
        provenance = {
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
        }
        return PendingResult(role_paths, provenance, job_dir)
