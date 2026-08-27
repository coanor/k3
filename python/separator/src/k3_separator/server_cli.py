"""Command-line entry point for the separator server."""

from __future__ import annotations

import argparse
import os
from pathlib import Path

from .server_app import create_app
from .server_core import ServerConfig


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
