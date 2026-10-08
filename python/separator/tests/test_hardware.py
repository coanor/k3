import json
import os
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from k3_separator.hardware import (
    NvidiaGpu, configure_device, detect_nvidia, read_plan, select_runtime, separation_defaults, write_plan,
)
from k3_separator.setup_runtime import install_torch


def gpu(capability=(5, 0), memory=4096, driver=(580, 0), index=0):
    return NvidiaGpu(index, f"GPU-fixture-{index}", "Fixture GPU", capability, memory, driver)


class HardwareTests(unittest.TestCase):
    def test_explicit_device_defaults_match_inference_mode(self):
        from unittest.mock import patch
        plan = select_runtime([gpu((12, 0), 16384)], "win32", "AMD64").document()
        with patch("k3_separator.hardware.read_plan", return_value=plan):
            self.assertEqual("quality", separation_defaults("auto")["profile"])
            self.assertEqual(256, separation_defaults("cpu")["segment_size"])
            self.assertEqual("fast", separation_defaults("cpu")["profile"])
        with patch("k3_separator.hardware.read_plan", return_value=None):
            self.assertEqual({}, separation_defaults("auto"))
            self.assertEqual(128, separation_defaults("gpu")["segment_size"])
    def test_runtime_matrix_and_first_use_defaults(self):
        cases = [
            ((3, 5), 4096, "cpu", "cpu", "fast", 256),
            ((5, 0), 4096, "cuda", "cu126", "fast", 128),
            ((5, 2), 8192, "cuda", "cu126", "fast", 128),
            ((6, 1), 8192, "cuda", "cu126", "fast", 128),
            ((7, 0), 16384, "cuda", "cu126", "fast", 128),
            ((7, 5), 6144, "cuda", "cu126", "balanced", 128),
            ((8, 6), 8192, "cuda", "cu126", "quality", 256),
            ((8, 9), 24576, "cuda", "cu126", "quality", 256),
            ((9, 0), 81920, "cuda", "cu126", "quality", 256),
            ((10, 0), 32768, "cuda", "cu128", "quality", 256),
            ((12, 0), 16384, "cuda", "cu128", "quality", 256),
            ((13, 0), 32768, "cpu", "cpu", "fast", 256),
        ]
        for cc, memory, backend, build, profile, segment in cases:
            with self.subTest(capability=cc, memory=memory):
                plan = select_runtime([gpu(cc, memory)], "win32", "AMD64")
                self.assertEqual((backend, build), (plan.backend, plan.torch_build))
                self.assertEqual((profile, segment), (plan.separation["profile"], plan.separation["segment_size"]))
                self.assertIn(f"torch==2.11.0+{build}", plan.torch_packages)

    def test_driver_and_host_limits(self):
        cases = [("win32", "AMD64", (5, 0), (528, 32)),
                 ("linux", "x86_64", (5, 0), (525, 60, 12)),
                 ("linux", "aarch64", (8, 7), (580, 0)),
                 ("win32", "AMD64", (12, 0), (560, 94))]
        for system, machine, cc, driver in cases:
            with self.subTest(system=system, machine=machine, capability=cc):
                self.assertEqual("cpu", select_runtime([gpu(cc, driver=driver)], system, machine).backend)
        self.assertEqual("cuda", select_runtime([gpu(driver=(528, 33))], "win32", "AMD64").backend)
        self.assertEqual("cuda", select_runtime([gpu(driver=(525, 60, 13))], "linux", "x86_64").backend)

    def test_selects_largest_compatible_gpu_and_cpu_override(self):
        devices = [gpu((3, 5), 32768), gpu((5, 0), index=1), gpu((8, 6), 8192, index=2)]
        self.assertEqual(2, select_runtime(devices, "linux", "x86_64").gpu.index)
        self.assertEqual("cpu", select_runtime(devices, "linux", "x86_64", "cpu").backend)

    def test_apple_silicon_uses_native_torch(self):
        plan = select_runtime([], "darwin", "arm64")
        self.assertEqual("mps", plan.backend)
        self.assertIsNone(plan.index_url)
        self.assertEqual("torch==2.11.0", plan.torch_packages[0])
        self.assertEqual("cpu", select_runtime([], "darwin", "arm64", "cpu").backend)
        self.assertEqual("cpu", select_runtime([], "linux", "x86_64").backend)

    def test_detection_parses_csv_and_respects_first_visible_device(self):
        output = ('0, GPU-zero, "GPU, old", 5.0, 4096, 560.94\n'
                  'malformed,row\n1, GPU-one, New GPU, 8.6, 8192, 580.0\n')
        with (patch("k3_separator.hardware._nvidia_smi", return_value="nvidia-smi"),
              patch("k3_separator.hardware.subprocess.run", return_value=subprocess.CompletedProcess([], 0, output)),
              patch.dict(os.environ, {}, clear=True)):
            self.assertEqual([0, 1], [item.index for item in detect_nvidia()])
            for value, indices in (("1,0", [1]), ("GPU-zero", [0]), ("", []), ("-1", []),
                                   ("-1,0", []), ("missing,1", []), (",1", []),
                                   ("GPU-,1", [])):
                os.environ["CUDA_VISIBLE_DEVICES"] = value
                self.assertEqual(indices, [item.index for item in detect_nvidia()])

    def test_detection_failure_is_cpu_fallback(self):
        with (patch("k3_separator.hardware._nvidia_smi", return_value="nvidia-smi"),
              patch("k3_separator.hardware.subprocess.run", side_effect=subprocess.TimeoutExpired("smi", 10))):
            self.assertEqual([], detect_nvidia())

    def test_metadata_roundtrip_and_malformed_metadata(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plan = select_runtime([gpu()], "linux", "x86_64")
            write_plan(root, plan)
            self.assertEqual(plan.document()["separation"], read_plan(root)["separation"])
            for value in ([], {}, {"schema_version": 2},
                          {**plan.document(), "gpu": ["invalid"]},
                          {**plan.document(), "backend": ["invalid"]},
                          {**plan.document(), "separation": {"profile": "fast"}}):
                (root / "k3-hardware.json").write_text(json.dumps(value))
                self.assertIsNone(read_plan(root))

    def test_device_selection_ignores_removed_gpu_and_preserves_override(self):
        plan = select_runtime([gpu()], "linux", "x86_64").document()
        with patch("k3_separator.hardware.read_plan", return_value=plan), patch.dict(os.environ, {}, clear=True):
            with patch("k3_separator.hardware.detect_nvidia", return_value=[]):
                configure_device()
                self.assertNotIn("CUDA_VISIBLE_DEVICES", os.environ)
            with patch("k3_separator.hardware.detect_nvidia", return_value=[gpu()]):
                configure_device()
                self.assertEqual(gpu().uuid, os.environ["CUDA_VISIBLE_DEVICES"])
                os.environ["CUDA_VISIBLE_DEVICES"] = "1"
                configure_device()
                self.assertEqual("1", os.environ["CUDA_VISIBLE_DEVICES"])


class SetupRuntimeTests(unittest.TestCase):
    def test_gpu_install_checks_kernels_then_falls_back_to_cpu(self):
        selected = select_runtime([gpu()], "linux", "x86_64")
        cpu = select_runtime([], "linux", "x86_64", "cpu")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            python = root / ("python.exe" if os.name == "nt" else "bin/python3")
            with (patch("k3_separator.setup_runtime.detect_runtime", side_effect=[selected, cpu]),
                  patch("k3_separator.setup_runtime.shutil.disk_usage", return_value=SimpleNamespace(free=64 * 1024**3)),
                  patch("k3_separator.setup_runtime.subprocess.run", side_effect=[
                      subprocess.CompletedProcess([], 0), subprocess.CalledProcessError(1, "probe"),
                      subprocess.CompletedProcess([], 0)]) as run):
                result = install_torch(python, "uv")
            self.assertEqual("cpu", result.backend)
            self.assertEqual("cpu", read_plan(root)["backend"])
            install, probe, fallback = [call.args[0] for call in run.call_args_list]
            self.assertIn("--break-system-packages", install)
            self.assertIn("torch==2.11.0+cu126", install)
            self.assertEqual(["--check", "cuda"], probe[-2:])
            self.assertIn("torch==2.11.0+cpu", fallback)

    def test_cuda_install_checks_space_before_downloading_or_changing_metadata(self):
        plan = select_runtime([gpu()], "linux", "x86_64")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_plan(root, select_runtime([], "linux", "x86_64", "cpu"))
            original = (root / "k3-hardware.json").read_bytes()
            with (patch("k3_separator.setup_runtime.detect_runtime", return_value=plan),
                  patch("k3_separator.setup_runtime.shutil.disk_usage", return_value=SimpleNamespace(free=18 * 1024**3)),
                  patch("k3_separator.setup_runtime.subprocess.run") as run):
                with self.assertRaisesRegex(RuntimeError, "24 GiB"):
                    install_torch(root / ("python.exe" if os.name == "nt" else "bin/python3"))
            run.assert_not_called()
            self.assertEqual(original, (root / "k3-hardware.json").read_bytes())

    def test_network_failure_does_not_claim_gpu_is_ready(self):
        plan = select_runtime([gpu()], "linux", "x86_64")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with (patch("k3_separator.setup_runtime.detect_runtime", return_value=plan),
                  patch("k3_separator.setup_runtime.shutil.disk_usage", return_value=SimpleNamespace(free=64 * 1024**3)),
                  patch("k3_separator.setup_runtime.subprocess.run", side_effect=subprocess.CalledProcessError(1, "download"))):
                with self.assertRaises(subprocess.CalledProcessError):
                    install_torch(root / ("python.exe" if os.name == "nt" else "bin/python3"))
            self.assertFalse((root / "k3-hardware.json").exists())

    def test_managed_python_and_windows_venv_locations(self):
        cpu = select_runtime([], "win32", "AMD64", "cpu")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "pyvenv.cfg").write_text("home = fixture")
            with (patch("k3_separator.setup_runtime.detect_runtime", return_value=cpu),
                  patch("k3_separator.setup_runtime.subprocess.run") as run):
                install_torch(root / "Scripts/python.exe")
            self.assertEqual("cpu", read_plan(root)["backend"])
            self.assertIn("--break-system-packages", run.call_args.args[0])


if __name__ == "__main__":
    unittest.main()
