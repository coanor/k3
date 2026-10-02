"""通过独立文件报告进度，保持 worker 的单行 JSON 响应协议。"""

from __future__ import annotations

import contextlib
import json
import math
import os
import re
import sys
from pathlib import Path
from typing import Iterator, TextIO


class ProgressReporter:
    def __init__(self, path: Path | None = None) -> None:
        self._path = path
        self._phase = "preparing"
        self._fraction: float | None = None

    @classmethod
    def from_environment(cls) -> ProgressReporter:
        value = os.environ.get("K3_SEPARATION_PROGRESS_PATH")
        return cls(Path(value) if value else None)

    def stage(self, phase: str) -> None:
        self._phase = phase
        self._fraction = None
        self._write()

    def fraction(self, value: float) -> None:
        if not math.isfinite(value) or not 0 <= value <= 1 or value == self._fraction:
            return
        self._fraction = value
        self._write()

    @contextlib.contextmanager
    def watch_inference(self) -> Iterator[None]:
        if self._path is None:
            yield
            return
        with contextlib.redirect_stderr(_ProgressStream(sys.stderr, self)):
            yield

    def _write(self) -> None:
        if self._path is None:
            return
        temporary = self._path.with_suffix(".tmp")
        try:
            temporary.write_text(
                json.dumps(
                    {"schema_version": 1, "phase": self._phase, "fraction": self._fraction}
                ),
                encoding="utf-8",
            )
            os.replace(temporary, self._path)
        except OSError as error:
            # 进度不可写不应中断音轨分离，诊断仍进入原有 stderr 日志。
            self._path = None
            print(f"k3 progress unavailable: {error}", file=sys.stderr)
            with contextlib.suppress(OSError):
                temporary.unlink(missing_ok=True)


class _ProgressStream:
    """保留 tqdm 的诊断输出，并提取当前推理阶段的真实百分比。"""

    def __init__(self, target: TextIO, reporter: ProgressReporter) -> None:
        self._target = target
        self._reporter = reporter
        self._buffer = ""

    def write(self, text: str) -> int:
        written = self._target.write(text)
        self._buffer = (self._buffer + text)[-4096:]
        matches = re.findall(r"(?:^|[\r\n])\s*(\d{1,3})%\|", self._buffer)
        if matches:
            self._reporter.fraction(int(matches[-1]) / 100)
        self._buffer = self._buffer.rsplit("\r", 1)[-1].rsplit("\n", 1)[-1]
        return written

    def flush(self) -> None:
        self._target.flush()

    def __getattr__(self, name: str):
        return getattr(self._target, name)
