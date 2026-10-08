"""Install and verify the hardware-compatible PyTorch runtime."""

from __future__ import annotations

import argparse
import json
import os
import shutil
from pathlib import Path
import subprocess
import sys

from .hardware import RuntimePlan, detect_runtime, write_plan


def install_torch(python: Path, uv: str | Path | None = None,
                  backend: str = "auto") -> RuntimePlan:
    plan = detect_runtime(backend)
    python_root = python.parent if os.name == "nt" else python.parent.parent
    if (python.parent / "pyvenv.cfg").exists():
        python_root = python.parent
    elif (python.parent.parent / "pyvenv.cfg").exists():
        python_root = python.parent.parent
    # Copying packages into the runtime keeps it relocatable, but CUDA wheels
    # also occupy the persistent uv/pip cache and download staging space.
    if plan.backend == "cuda" and shutil.disk_usage(python_root).free < 24 * 1024**3:
        raise RuntimeError("CUDA runtime preparation requires at least 24 GiB free for installed packages, cached downloads and temporary files; choose another disk or free space")
    environment = dict(os.environ, PYTHONUTF8="1")
    if plan.gpu:
        environment["CUDA_VISIBLE_DEVICES"] = plan.gpu.uuid
    prefix = ([str(uv), "pip", "install", "--python", str(python), "--no-config",
               "--break-system-packages", "--link-mode", "copy"] if uv else
              [str(python), "-I", "-X", "utf8", "-m", "pip", "install", "--break-system-packages"])

    def install(selected: RuntimePlan) -> None:
        print(f"{selected.reason}; PyTorch build: {selected.torch_build}", flush=True)
        args = prefix + selected.torch_packages
        if selected.index_url:
            args += ["--index-url", selected.index_url]
        subprocess.run(args, check=True, env=environment)

    install(plan)
    write_plan(python_root, plan)
    if plan.backend != "cpu":
        probe = Path(__file__).with_name("hardware.py")
        try:
            subprocess.run([str(python), "-I", "-X", "utf8", str(probe), "--check", plan.backend],
                           check=True, env=environment, timeout=120)
        except subprocess.SubprocessError as error:
            print(f"Accelerator validation failed ({error}); preparing the CPU runtime instead.", flush=True)
            plan = detect_runtime("cpu")
            install(plan)
            write_plan(python_root, plan)
    return plan


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--python", type=Path, default=Path(sys.executable))
    parser.add_argument("--uv", type=Path)
    parser.add_argument("--backend", choices=("auto", "cpu", "gpu"), default="auto")
    args = parser.parse_args()
    plan = install_torch(args.python, args.uv, args.backend)
    print(json.dumps(plan.document()))


if __name__ == "__main__":
    main()
