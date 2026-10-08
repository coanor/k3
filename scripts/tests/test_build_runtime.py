"""Verify hardware selection survives runtime staging and relocation."""

import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

REPO = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("runtime_builder", REPO / "scripts/build-runtime.py")
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)
from k3_separator.hardware import NvidiaGpu, read_plan, select_runtime, write_plan


class RuntimeBuildTests(unittest.TestCase):
    def test_portable_cpu_and_detected_gpu_builds_retain_verified_selection(self):
        device = NvidiaGpu(0, "GPU-fixture", "750 Ti", (5, 0), 4096, (560, 94))
        for backend in ("cpu", "auto"):
            with self.subTest(backend=backend), tempfile.TemporaryDirectory() as directory:
                output = Path(directory) / "runtime"
                plan = select_runtime([device], "linux", "x86_64", backend)

                def install(python, uv, selected_backend):
                    self.assertEqual(backend, selected_backend)
                    write_plan(python.parent if os.name == "nt" else python.parent.parent, plan)
                    return plan

                def run(*args, **kwargs):
                    if args[1:3] == ("python", "install"):
                        downloads = Path(args[args.index("--install-dir") + 1])
                        (downloads / f"cpython-{builder.PYTHON_VERSION}-linux-x86_64-none/bin").mkdir(parents=True)
                    elif "-c" in args:
                        constraints = Path(args[args.index("-c") + 1]).read_text().splitlines()
                        self.assertEqual(plan.torch_packages, constraints)
                        self.assertIn("onnxruntime==1.24.4", args)
                    elif "k3_separator.bundle" in args:
                        bundle = Path(args[args.index("--output") + 1])
                        (bundle / "bundle-manifest.json").write_text(json.dumps({"models": [], "artifacts": {}}))

                with (patch.object(builder.shutil, "which", return_value="uv"),
                      patch.object(builder, "install_torch", side_effect=install),
                      patch.object(builder, "run", side_effect=run)):
                    if backend == "cpu":
                        builder.build(output, None, [])
                    else:
                        builder.build(output, None, [], backend=backend)
                manifest = json.loads((output / "bundle-manifest.json").read_text())
                self.assertEqual(plan.backend, manifest["backend"])
                self.assertEqual(plan.torch_build, manifest["torch_build"])
                self.assertEqual(plan.backend, read_plan(output / "python")["backend"])


if __name__ == "__main__":
    unittest.main()
