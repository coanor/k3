"""并发容量基准测试命令。"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import multiprocessing
import shutil
import tempfile
import time
from pathlib import Path
from typing import Any

from .server import _execute_inference


def recommend_concurrency(records: list[dict[str, Any]]) -> int:
    """返回所有任务成功的最高并发数。"""

    passing = [record["concurrency"] for record in records if record["failures"] == 0]
    return max(passing, default=0)


def benchmark(args: argparse.Namespace) -> dict[str, Any]:
    input_path = args.input.expanduser().resolve()
    if not input_path.is_file():
        raise SystemExit(f"输入文件不存在：{input_path}")
    root = Path(tempfile.mkdtemp(prefix="k3-separator-benchmark-"))
    records: list[dict[str, Any]] = []
    try:
        for concurrency in range(1, args.max_jobs + 1):
            started = time.monotonic()
            failures: list[str] = []
            peak_cuda_bytes = 0
            context = multiprocessing.get_context("spawn")
            with concurrent.futures.ProcessPoolExecutor(
                max_workers=concurrency, mp_context=context
            ) as executor:
                futures = []
                for index in range(concurrency):
                    output_dir = root / f"c{concurrency}-j{index}"
                    output_dir.mkdir()
                    futures.append(
                        executor.submit(
                            _execute_inference,
                            {
                                "model_dir": str(args.model_dir.expanduser().resolve()),
                                "registry_path": (
                                    str(args.models.expanduser().resolve())
                                    if args.models
                                    else None
                                ),
                                "backend": args.backend,
                                "input_path": str(input_path),
                                "output_dir": str(output_dir),
                                "profile": args.preset,
                                "model_id": args.model,
                                "preserve_backing_vocals": args.output_layout
                                == "karaoke",
                            },
                        )
                    )
                for future in futures:
                    try:
                        result = future.result()
                        peak_cuda_bytes = max(
                            peak_cuda_bytes,
                            result.get("_worker_metrics", {}).get("peak_cuda_bytes", 0),
                        )
                    except Exception as error:  # noqa: BLE001 - benchmark records crashes
                        failures.append(f"{type(error).__name__}: {error}")
            elapsed = time.monotonic() - started
            records.append(
                {
                    "concurrency": concurrency,
                    "jobs": concurrency,
                    "failures": len(failures),
                    "failure_messages": failures,
                    "wall_seconds": round(elapsed, 3),
                    "jobs_per_hour": round(concurrency * 3600 / elapsed, 2),
                    "max_worker_peak_cuda_bytes": peak_cuda_bytes or None,
                }
            )
    finally:
        shutil.rmtree(root, ignore_errors=True)
    return {
        "backend": args.backend,
        "model_id": args.model,
        "preset": args.preset,
        "output_layout": args.output_layout,
        "records": records,
        "recommended_max_jobs": recommend_concurrency(records),
        "warning": (
            "在目标机器、代表性长歌曲和预计同时使用的模型组合上重复运行；"
            "零失败只表示本轮未 OOM，不是生产 SLA 保证。"
        ),
    }


def main() -> None:
    parser = argparse.ArgumentParser(
        description="测量固定设备可承受的 separator 并发数"
    )
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--model", required=True)
    parser.add_argument(
        "--preset", choices=("fast", "balanced", "quality"), required=True
    )
    parser.add_argument(
        "--output-layout", choices=("two_stem", "karaoke"), default="karaoke"
    )
    parser.add_argument(
        "--backend", choices=("cpu", "cuda", "coreml", "auto"), required=True
    )
    parser.add_argument("--model-dir", type=Path, required=True)
    parser.add_argument("--models", type=Path)
    parser.add_argument("--max-jobs", type=int, default=4)
    args = parser.parse_args()
    if args.max_jobs < 1:
        parser.error("--max-jobs 必须至少为 1")
    print(json.dumps(benchmark(args), ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
