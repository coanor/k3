"""验证 Makefile 实际构建环境及复用 runtime 后的 worker 内容。"""

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]


@unittest.skipIf(os.name == "nt", "Makefile 入口回归需要 POSIX shell 和 make")
class MakeDistTests(unittest.TestCase):
    def test_linux_distribution_uses_configured_python_for_every_step(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tools = root / "tools"
            tools.mkdir()
            uname = tools / "uname"
            uname.write_text('#!/bin/sh\ncase "$1" in -s) echo Linux ;; -m) echo x86_64 ;; esac\n')
            uname.chmod(0o755)
            python = tools / "configured-python"
            python.write_text('#!/bin/sh\nprintf "%s\\n" "$1" >> "$K3_TEST_CALLS"\n')
            python.chmod(0o755)
            runtime = root / "runtime"
            runtime.mkdir()
            (runtime / "bundle-manifest.json").write_text("{}")
            calls = root / "calls"
            environment = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"],
                               K3_TEST_CALLS=str(calls))
            subprocess.run(["make", "dist-linux", "CARGO=true", f"PYTHON={python}",
                            f"RUNTIME_DIR={runtime}"], cwd=REPO, env=environment,
                           check=True, capture_output=True)
            self.assertEqual(calls.read_text().splitlines(),
                             ["scripts/refresh-worker.py", "scripts/package-dist.py", "scripts/check-dist.py"])

    def test_linux_arm64_installer_uses_native_target_and_archive(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tools = root / "tools"
            tools.mkdir()
            for name, script in {
                "uname": '#!/bin/sh\ncase "$1" in -s) echo Linux ;; -m) echo aarch64 ;; esac\n',
                "cargo": '#!/bin/sh\nprintf "%s\\n" "$*" >> "$K3_TEST_CALLS"\n',
                "python": '#!/bin/sh\nprintf "%s\\n" "$*" >> "$K3_TEST_CALLS"\n',
            }.items():
                path = tools / name
                path.write_text(script)
                path.chmod(0o755)
            runtime = root / "runtime"
            runtime.mkdir()
            (runtime / "bundle-manifest.json").write_text("{}")
            calls = root / "calls"
            environment = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"],
                               K3_TEST_CALLS=str(calls))
            subprocess.run(["make", "installer", f"CARGO={tools / 'cargo'}",
                            f"PYTHON={tools / 'python'}", f"RUNTIME_DIR={runtime}", "DIST_DIR=dist/arm-test"],
                           cwd=REPO, env=environment, check=True, capture_output=True)
            commands = calls.read_text().splitlines()
            self.assertIn("--target aarch64-unknown-linux-gnu", commands[1])
            self.assertIn("linux k3-linux-aarch64 target/aarch64-unknown-linux-gnu/release/k3", commands[2])
            self.assertEqual(commands[3], "scripts/check-dist.py dist/arm-test/k3-linux-aarch64.tar.gz")
            self.assertEqual(commands[4], "scripts/build-installer.py dist/arm-test/k3-linux-aarch64.tar.gz --output-dir dist/arm-test/installers")

    def test_windows_arm64_host_builds_cli_installer_without_runtime(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tools = root / "tools"
            tools.mkdir()
            for name, script in {
                "uname": '#!/bin/sh\ncase "$1" in -s) echo MINGW64_NT-10.0 ;; -m) echo aarch64 ;; esac\n',
                "rustup": '#!/bin/sh\nexit 0\n',
                "cargo": '#!/bin/sh\nprintf "%s\\n" "$*" >> "$K3_TEST_CALLS"\nprintf "%s" "$RUSTFLAGS" > "$K3_TEST_FLAGS"\n',
                "python": '#!/bin/sh\nprintf "%s\\n" "$*" >> "$K3_TEST_CALLS"\n',
            }.items():
                path = tools / name
                path.write_text(script)
                path.chmod(0o755)
            calls = root / "calls"
            flags = root / "flags"
            runtime = root / "must-not-be-created"
            environment = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"],
                               K3_TEST_CALLS=str(calls), K3_TEST_FLAGS=str(flags))
            subprocess.run(["make", "installer", f"CARGO={tools / 'cargo'}",
                            f"PYTHON={tools / 'python'}", f"RUNTIME_DIR={runtime}", "DIST_DIR=dist/arm-test",
                            "RUSTFLAGS=-C opt-level=2"], cwd=REPO, env=environment, check=True, capture_output=True)
            commands = calls.read_text().splitlines()
            self.assertIn("--bin k3 --package k3 --target aarch64-pc-windows-msvc", commands[0])
            self.assertIn("windows k3-windows-aarch64-cli", commands[1])
            self.assertIn("--cli-only", commands[1])
            self.assertEqual(commands[2], "scripts/check-dist.py dist/arm-test/k3-windows-aarch64-cli.zip --cli-only")
            self.assertEqual(commands[3], "scripts/build-installer.py dist/arm-test/k3-windows-aarch64-cli.zip --output-dir dist/arm-test/installers")
            self.assertEqual(flags.read_text(), "-C opt-level=2 -C target-feature=+crt-static")
            self.assertFalse(runtime.exists())

    def test_native_windows_build_keeps_user_flags_and_links_static_crt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tools = root / "tools"
            tools.mkdir()
            uname = tools / "uname"
            uname.write_text('#!/bin/sh\nprintf "MINGW64_NT-10.0\\n"\n')
            uname.chmod(0o755)
            cargo = tools / "cargo"
            cargo.write_text('#!/bin/sh\nprintf "%s" "$RUSTFLAGS" > "$K3_TEST_FLAGS"\n')
            cargo.chmod(0o755)
            runtime = root / "runtime"
            runtime.mkdir()
            (runtime / "bundle-manifest.json").write_text("{}")
            flags = root / "flags"
            environment = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"],
                               K3_TEST_FLAGS=str(flags))
            subprocess.run(["make", "dist-windows", f"CARGO={cargo}", "PYTHON=true",
                            f"RUNTIME_DIR={runtime}", "RUSTFLAGS=-C opt-level=2"],
                           cwd=REPO, env=environment, check=True, capture_output=True)
            self.assertEqual(flags.read_text(), "-C opt-level=2 -C target-feature=+crt-static")

    @unittest.skipUnless(shutil.which("uv"), "更新包内 worker 需要 uv")
    def test_reused_runtime_gets_current_worker_without_changing_models(self):
        with tempfile.TemporaryDirectory(prefix="K3 缓存 ") as directory:
            root = Path(directory)
            runtime = root / "runtime"
            python_root = runtime / "python"
            subprocess.run(["uv", "venv", "--python", sys.executable, str(python_root)],
                           check=True, capture_output=True)
            python = python_root / "bin/python3"
            old_project = root / "old-worker"
            shutil.copytree(REPO / "python/separator", old_project,
                            ignore=shutil.ignore_patterns("__pycache__", "tests"))
            (old_project / "src/k3_separator/runtime.py").write_text('OLD_WORKER = True\n')
            subprocess.run(["uv", "pip", "install", "--python", str(python), "--no-deps",
                            str(old_project)], check=True, capture_output=True)
            installed = next(python_root.glob("lib/python*/site-packages/k3_separator/runtime.py"))
            self.assertIn("OLD_WORKER", installed.read_text())
            (runtime / "bundle-manifest.json").write_text("{}")
            models = runtime / "models"
            models.mkdir()
            checkpoint = models / "model.onnx"
            checkpoint.write_bytes(b"keep existing model")
            subprocess.run(["make", "dist-runtime", f"PYTHON={sys.executable}",
                            f"RUNTIME_DIR={runtime}"], cwd=REPO, check=True, capture_output=True)
            self.assertEqual(installed.read_bytes(),
                             (REPO / "python/separator/src/k3_separator/runtime.py").read_bytes())
            self.assertEqual(checkpoint.read_bytes(), b"keep existing model")
            self.assertFalse((python_root / "bin/k3-separator").exists())


if __name__ == "__main__":
    unittest.main()
