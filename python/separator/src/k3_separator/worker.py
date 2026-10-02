"""Long-running JSON-lines process interface."""

from __future__ import annotations

import argparse
import contextlib
import json
import sys
from pathlib import Path
from typing import TextIO

from .errors import WorkerError
from .models import ModelRegistry
from .runtime import AudioSeparatorRuntime
from .service import SeparationService
from .progress import ProgressReporter


def serve(service: SeparationService, input_stream: TextIO, output_stream: TextIO) -> None:
    """Process one JSON request per input line and emit exactly one response line."""
    _force_utf8(input_stream)
    _force_utf8(output_stream)
    _force_utf8(sys.stderr)
    for line in input_stream:
        if not line.strip():
            continue
        request_id = None
        try:
            request = json.loads(line)
            if isinstance(request, dict):
                request_id = request.get("id")
            with contextlib.redirect_stdout(sys.stderr):
                result = service.handle(request)
            response = {"id": request_id, "ok": True, "result": result}
        except WorkerError as error:
            response = {
                "id": request_id,
                "ok": False,
                "error": {"code": error.code, "message": error.message},
            }
        except json.JSONDecodeError as error:
            response = {
                "id": request_id,
                "ok": False,
                "error": {"code": "invalid_json", "message": str(error)},
            }
        except Exception as error:  # preserve the protocol if a dependency crashes
            response = {
                "id": request_id,
                "ok": False,
                "error": {"code": "internal_error", "message": str(error)},
            }
        output_stream.write(json.dumps(response, ensure_ascii=False, separators=(",", ":")))
        output_stream.write("\n")
        output_stream.flush()


def _force_utf8(stream: TextIO) -> None:
    reconfigure = getattr(stream, "reconfigure", None)
    if callable(reconfigure):
        reconfigure(encoding="utf-8", errors="strict")


def main() -> None:
    parser = argparse.ArgumentParser(description="K3 local stem-separation worker")
    parser.add_argument("--models", type=Path, help="JSON registry merged over built-ins")
    parser.add_argument(
        "--model-dir",
        type=Path,
        default=Path("~/.cache/k3/models"),
        help="checkpoint cache directory",
    )
    args = parser.parse_args()
    registry = ModelRegistry.load(args.models)
    progress = ProgressReporter.from_environment()
    runtime = AudioSeparatorRuntime(args.model_dir, progress)
    serve(SeparationService(registry, runtime, progress), sys.stdin, sys.stdout)


if __name__ == "__main__":
    main()
