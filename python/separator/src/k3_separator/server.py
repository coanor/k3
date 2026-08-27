"""Stable public façade for the remote separator server."""

from .server_app import create_app
from .server_cli import main
from .server_contracts import InputRequest, JobRequest
from .server_core import SeparatorServer, ServerConfig
from .server_runtime import (
    execute_inference as _execute_inference,
)
from .server_runtime import (
    fake_float_wav as _fake_float_wav,
)
from .server_runtime import (
    validate_output_wavs as _validate_output_wavs,
)

__all__ = [
    "InputRequest",
    "JobRequest",
    "SeparatorServer",
    "ServerConfig",
    "_execute_inference",
    "_fake_float_wav",
    "_validate_output_wavs",
    "create_app",
    "main",
]


if __name__ == "__main__":
    main()
