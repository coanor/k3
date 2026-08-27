"""Dependency-free identifiers and content hashing for server modules."""

import hashlib
import uuid
from pathlib import Path


def resource_id(kind: str) -> str:
    return f"{kind}_{uuid.uuid4().hex}"


def hash_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()
