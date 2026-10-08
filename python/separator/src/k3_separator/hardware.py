"""Select a tested runtime and first-use settings without importing PyTorch."""

from __future__ import annotations

import argparse
import csv
from dataclasses import asdict, dataclass
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
from typing import Any

PLAN_FILENAME = "k3-hardware.json"
TORCH_VERSIONS = {"torch": "2.11.0", "torchvision": "0.26.0", "torchaudio": "2.11.0"}


@dataclass(frozen=True)
class NvidiaGpu:
    index: int
    uuid: str
    name: str
    capability: tuple[int, int]
    memory_mib: int
    driver: tuple[int, ...]


@dataclass(frozen=True)
class RuntimePlan:
    backend: str
    torch_build: str
    gpu: NvidiaGpu | None
    separation: dict[str, Any]
    reason: str

    @property
    def torch_packages(self) -> list[str]:
        suffix = "" if self.torch_build == "default" else "+" + self.torch_build
        return [f"{name}=={version}{suffix}" for name, version in TORCH_VERSIONS.items()]

    @property
    def index_url(self) -> str | None:
        return None if self.torch_build == "default" else f"https://download.pytorch.org/whl/{self.torch_build}"

    def document(self) -> dict[str, Any]:
        return {"schema_version": 1, **asdict(self)}


def _nvidia_smi() -> str | None:
    executable = shutil.which("nvidia-smi")
    if executable:
        return executable
    if sys.platform == "win32":
        for folder in (Path(os.environ.get("SystemRoot", r"C:\Windows")) / "System32",
                       Path(os.environ.get("ProgramFiles", r"C:\Program Files")) / "NVIDIA Corporation/NVSMI"):
            candidate = folder / "nvidia-smi.exe"
            if candidate.is_file():
                return str(candidate)
    return None


def detect_nvidia() -> list[NvidiaGpu]:
    executable = _nvidia_smi()
    if executable is None:
        return []
    try:
        result = subprocess.run(
            [executable, "--query-gpu=index,uuid,name,compute_cap,memory.total,driver_version",
             "--format=csv,noheader,nounits"],
            check=True, capture_output=True, text=True, timeout=10,
            **({"creationflags": subprocess.CREATE_NO_WINDOW} if sys.platform == "win32" else {}),
        )
    except (OSError, subprocess.SubprocessError):
        return []
    devices = []
    visibility = os.environ.get("CUDA_VISIBLE_DEVICES")
    for fields in csv.reader(result.stdout.splitlines(), skipinitialspace=True):
        try:
            index, uuid, name, capability, memory, driver = (value.strip() for value in fields)
            cc = tuple(int(value) for value in capability.split("."))
            if len(cc) != 2:
                continue
            gpu = NvidiaGpu(int(index), uuid, name, cc, int(float(memory)),
                            tuple(int(value) for value in driver.split(".")))
            devices.append(gpu)
        except (ValueError, TypeError):
            continue
    if visibility is None:
        return devices
    # CUDA stops at an invalid visibility token; never skip it to enable a
    # later GPU. Only the first visible device is used by the worker.
    token = visibility.split(",", 1)[0].strip()
    matches = [gpu for gpu in devices if token == str(gpu.index)
               or (token.startswith("GPU-") and gpu.uuid.startswith(token))]
    # UUID prefixes must uniquely identify a physical card.
    return matches if len(matches) == 1 else []


def select_runtime(gpus: list[NvidiaGpu], system: str, machine: str,
                   backend: str = "auto") -> RuntimePlan:
    """Prefer a compatible visible GPU, then choose conservative inference defaults."""
    if backend not in {"auto", "cpu", "gpu"}:
        raise ValueError("backend must be auto, cpu, or gpu")
    apple = system == "darwin" and machine.lower() in {"arm64", "aarch64"}
    base_build = "default" if system == "darwin" else "cpu"
    cpu_defaults = {"profile": "fast", "model": "uvr-mdx-karaoke-2", "segment_size": 256,
                    "autocast": False, "preserve_backing_vocals": False}
    minimum_driver = (528, 33) if system == "win32" else (525, 60, 13)
    supported_host = system in {"linux", "win32"} and machine.lower() in {"amd64", "x86_64"}
    supported = [gpu for gpu in gpus if supported_host
                 and (5, 0) <= gpu.capability <= (12, 0) and gpu.capability != (8, 7)
                 and gpu.driver >= minimum_driver
                 and (gpu.capability < (10, 0) or gpu.driver >= (570,))]
    if backend != "cpu" and supported:
        gpu = max(supported, key=lambda item: (item.memory_mib, item.capability, -item.index))
        build = "cu128" if gpu.capability >= (10, 0) else "cu126"
        defaults = dict(cpu_defaults, segment_size=128)
        if gpu.capability >= (7, 5) and gpu.memory_mib >= 8 * 1024:
            defaults.update(profile="quality", model="bs-roformer-viperx-1297", segment_size=256,
                            autocast=True, preserve_backing_vocals=True)
        elif gpu.capability >= (7, 5) and gpu.memory_mib >= 6 * 1024:
            defaults.update(profile="balanced", model="uvr-mdx-inst-hq-3", autocast=False)
        return RuntimePlan("cuda", build, gpu, defaults,
                           f"Selected {gpu.name} ({gpu.memory_mib} MiB, compute capability {gpu.capability[0]}.{gpu.capability[1]})")
    if backend != "cpu" and apple:
        return RuntimePlan("mps", "default", None, dict(cpu_defaults, segment_size=128),
                           "Apple Silicon uses the native PyTorch MPS backend")
    reason = "CPU runtime requested" if backend == "cpu" else "No supported accelerator and driver were detected; using CPU"
    return RuntimePlan("cpu", base_build, None, cpu_defaults, reason)


def detect_runtime(backend: str = "auto") -> RuntimePlan:
    return select_runtime(detect_nvidia() if backend != "cpu" else [], sys.platform, platform.machine(), backend)


def write_plan(python_root: Path, plan: RuntimePlan) -> None:
    path = python_root / PLAN_FILENAME
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(plan.document(), indent=2) + "\n", encoding="utf-8")
    temporary.replace(path)


def read_plan(python_root: Path | None = None) -> dict[str, Any] | None:
    try:
        document = json.loads(((python_root or Path(sys.prefix)) / PLAN_FILENAME).read_text(encoding="utf-8"))
        if (not isinstance(document, dict) or document.get("schema_version") != 1
                or document.get("backend") not in {"cpu", "cuda", "mps"}
                or document.get("torch_build") not in {"cpu", "cu126", "cu128", "default"}):
            return None
        settings = document.get("separation")
        gpu = document.get("gpu")
        if (not isinstance(settings, dict) or settings.get("profile") not in
                {"fast", "balanced", "quality", "compatible"}
                or not isinstance(settings.get("model"), str)
                or not isinstance(settings.get("segment_size"), int)
                or isinstance(settings.get("segment_size"), bool)
                or not 1 <= settings["segment_size"] <= 4096
                or not isinstance(settings.get("autocast"), bool)
                or not isinstance(settings.get("preserve_backing_vocals"), bool)
                or (gpu is not None and not isinstance(gpu, dict))):
            return None
        return document
    except (OSError, ValueError, AttributeError, TypeError):
        return None


def configure_device() -> None:
    """Use the installation's selected GPU unless the user explicitly controls visibility."""
    plan = read_plan()
    gpu = plan.get("gpu") if plan else None
    if ("CUDA_VISIBLE_DEVICES" not in os.environ and gpu
            and isinstance(gpu.get("uuid"), str) and gpu["uuid"].startswith("GPU-")
            and any(device.uuid == gpu["uuid"] for device in detect_nvidia())):
        os.environ["CUDA_VISIBLE_DEVICES"] = gpu["uuid"]


def separation_defaults(device: str = "auto") -> dict[str, Any]:
    """Keep hardware recommendations consistent with an explicit device choice."""
    if device not in {"auto", "cpu", "gpu"}:
        raise ValueError("device must be auto, cpu, or gpu")
    plan = read_plan()
    if device == "cpu":
        return select_runtime([], sys.platform, platform.machine(), "cpu").separation
    if plan and (device == "auto" or plan["backend"] != "cpu"):
        return dict(plan["separation"])
    if device == "gpu":
        return dict(select_runtime([], sys.platform, platform.machine(), "cpu").separation,
                    segment_size=128)
    return {}


def check_accelerator(backend: str) -> dict[str, Any]:
    """Execute kernels; device enumeration alone does not prove compatibility."""
    import torch

    result = {"torch": torch.__version__, "backend": backend}
    if backend == "cpu":
        return result
    if backend == "cuda":
        if not torch.cuda.is_available():
            raise RuntimeError("CUDA is unavailable; check the NVIDIA driver")
        result.update(device=torch.cuda.get_device_name(0),
                      capability=list(torch.cuda.get_device_capability(0)))
    elif not torch.backends.mps.is_available():
        raise RuntimeError("PyTorch MPS is unavailable on this installation")
    device = "cuda" if backend == "cuda" else "mps"
    value = torch.ones((1, 1, 16, 16), device=device)
    kernel = torch.ones((1, 1, 3, 3), device=device)
    output = torch.nn.functional.conv2d(value, kernel)
    if not torch.isfinite(output).all().item() or output[0, 0, 0, 0].item() != 9:
        raise RuntimeError("Accelerator convolution check failed")
    if backend == "cuda":
        spectrum = torch.fft.rfft(torch.ones(64, device=device))
        if spectrum[0].real.item() != 64 or not torch.isfinite(spectrum).all().item():
            raise RuntimeError("Accelerator FFT check failed")
        torch.cuda.synchronize()
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--backend", choices=("auto", "cpu", "gpu"), default="auto")
    parser.add_argument("--check", choices=("cpu", "cuda", "mps"))
    parser.add_argument("--write", type=Path, help="Write recommendations into a Python environment")
    parser.add_argument("--torch-build", action="store_true")
    args = parser.parse_args()
    if args.check:
        configure_device()
        print(json.dumps(check_accelerator(args.check)))
        return
    plan = detect_runtime(args.backend)
    if args.write:
        write_plan(args.write, plan)
    print(plan.torch_build if args.torch_build else json.dumps(plan.document()))


if __name__ == "__main__":
    main()
