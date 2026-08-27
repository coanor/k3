import sqlite3
import tempfile
import threading
import unittest
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from k3_separator.server_artifacts import ArtifactStore


@dataclass
class Pending:
    role_paths: dict[str, Path]
    provenance: dict[str, Any]


class ArtifactStoreTests(unittest.TestCase):
    def setUp(self) -> None:
        self.sandbox = tempfile.TemporaryDirectory()
        self.root = Path(self.sandbox.name)
        (self.root / "artifacts" / "sha256").mkdir(parents=True)
        self.database = sqlite3.connect(":memory:")
        self.database.row_factory = sqlite3.Row
        self.database.executescript(
            """
            CREATE TABLE jobs(
                id TEXT PRIMARY KEY, status TEXT, stage TEXT,
                result_json TEXT, error_json TEXT, updated_at REAL
            );
            CREATE TABLE artifacts(
                id TEXT PRIMARY KEY, sha256 TEXT, size_bytes INTEGER,
                media_type TEXT, path TEXT UNIQUE
            );
            CREATE TABLE job_artifacts(
                job_id TEXT, artifact_id TEXT, role TEXT,
                PRIMARY KEY(job_id, role)
            );
            INSERT INTO jobs(id, status, stage) VALUES ('job_1', 'running', 'separating');
            """
        )
        self.store = ArtifactStore(self.root, self.database, threading.RLock())

    def tearDown(self) -> None:
        self.database.close()
        self.sandbox.cleanup()

    def pending(self) -> Pending:
        vocals = self.root / "vocals.wav"
        accompaniment = self.root / "accompaniment.wav"
        vocals.write_bytes(b"vocals")
        accompaniment.write_bytes(b"music")
        return Pending(
            {"vocals": vocals, "accompaniment": accompaniment},
            {"provider": "test"},
        )

    def test_publishes_the_complete_set_and_job_result_in_one_transaction(self) -> None:
        self.store.publish_set("job_1", self.pending())

        job = self.database.execute(
            "SELECT status, result_json FROM jobs WHERE id = 'job_1'"
        ).fetchone()
        links = self.database.execute(
            "SELECT role FROM job_artifacts ORDER BY role"
        ).fetchall()
        self.assertEqual("completed", job["status"])
        self.assertIsNotNone(job["result_json"])
        self.assertEqual(["accompaniment", "vocals"], [row["role"] for row in links])

    def test_database_failure_exposes_neither_partial_links_nor_completed_job(
        self,
    ) -> None:
        self.database.execute(
            """
            CREATE TRIGGER reject_accompaniment
            BEFORE INSERT ON job_artifacts
            WHEN NEW.role = 'accompaniment'
            BEGIN
                SELECT RAISE(ABORT, 'forced publication failure');
            END;
            """
        )

        with self.assertRaisesRegex(sqlite3.IntegrityError, "forced publication"):
            self.store.publish_set("job_1", self.pending())

        job = self.database.execute(
            "SELECT status, result_json FROM jobs WHERE id = 'job_1'"
        ).fetchone()
        link_count = self.database.execute(
            "SELECT COUNT(*) FROM job_artifacts"
        ).fetchone()[0]
        self.assertEqual("running", job["status"])
        self.assertIsNone(job["result_json"])
        self.assertEqual(0, link_count)


if __name__ == "__main__":
    unittest.main()
