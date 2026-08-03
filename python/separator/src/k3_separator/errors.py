"""Errors exposed by the worker protocol."""


class WorkerError(Exception):
    """A failure safe to expose to the worker caller."""

    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code
        self.message = message

