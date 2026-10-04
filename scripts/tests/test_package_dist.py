"""检查完整包的平台隔离、压缩格式和可移动的相对链接。"""

import hashlib
import importlib.util
import json
import os
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "package-dist.py"
sys.path.insert(0, str(SCRIPT.parent))
spec = importlib.util.spec_from_file_location("package_dist", SCRIPT)
packager = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packager)
checker_spec = importlib.util.spec_from_file_location("check_runtime", SCRIPT.with_name("check-runtime.py"))
checker = importlib.util.module_from_spec(checker_spec)
checker_spec.loader.exec_module(checker)


class PackageTests(unittest.TestCase):
    def test_windows_manifest_checks_the_relocated_ffmpeg_and_models(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ("runtime/bin/ffmpeg.exe", "models/checkpoint.onnx"):
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"bundled artifact")
            digest = hashlib.sha256(b"bundled artifact").hexdigest()
            checker.check_artifacts(root, {r"bin\ffmpeg.exe": digest, r"models\checkpoint.onnx": digest})

    def test_build_tools_handle_a_non_utf8_windows_style_pipe(self):
        environment = dict(os.environ, PYTHONIOENCODING="cp1252", PYTHONUTF8="0")
        for filename in ("build-runtime.py", "package-dist.py", "check-dist.py", "check-runtime.py"):
            with self.subTest(script=filename):
                output = subprocess.run(
                    [sys.executable, "-c",
                     "import runpy, sys; from pathlib import Path; sys.argv = sys.argv[1:]; "
                     "sys.path.insert(0, str(Path(sys.argv[0]).parent))\n"
                     "try: runpy.run_path(sys.argv[0], run_name='__main__')\n"
                     "finally: print('Unicode path: \\u6a21\\u578b')",
                     str(SCRIPT.parent / filename), "--help"],
                    env=environment, capture_output=True, check=True,
                )
                help_text, marker = output.stdout.decode("utf-8").rsplit("Unicode path: ", 1)
                self.assertTrue(help_text.isascii())
                self.assertEqual(marker.strip(), "模型")

    def fixture(self, root: Path, platform: str, machine: str = "x86_64"):
        suffix = ".exe" if platform == "windows" else ""
        binary = root / f"k3{suffix}"
        binary.write_bytes(b"native program")
        (root / f"k3-separator{suffix}").write_bytes(b"native launcher")
        (root / f"k3-gui{suffix}").write_bytes(b"native GUI")
        runtime = root / "runtime-source"
        for component in ("python", "bin", "models"):
            (runtime / component).mkdir(parents=True)
        (runtime / "models/checkpoint.onnx").write_bytes(b"checkpoint")
        (runtime / "requirements-resolved.txt").write_text("package==1.0\n")
        (runtime / "bundle-manifest.json").write_text(json.dumps({
            "platform": {"linux": "linux", "windows": "win32", "macos": "darwin"}[platform],
            "machine": machine,
        }))
        return binary, runtime

    @unittest.skipIf(os.name == "nt", "Unix 包的可执行权限需要 Unix 文件系统")
    def test_tar_preserves_python_relative_links_and_models_after_move(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary, runtime = self.fixture(root, "linux")
            (runtime / "python/bin").mkdir()
            (runtime / "python/bin/python3.13").write_bytes(b"standalone python")
            (runtime / "python/bin/python3").symlink_to("python3.13")
            archive = packager.package("linux", "k3-linux-x86_64", binary, runtime, False, root / "dist")
            unpacked = root / "移动后的 包"
            with tarfile.open(archive) as stream:
                stream.extractall(unpacked, filter="data")
            package = unpacked / "k3-linux-x86_64"
            self.assertEqual((package / "runtime/python/bin/python3").read_bytes(), b"standalone python")
            self.assertEqual((package / "models/checkpoint.onnx").read_bytes(), b"checkpoint")
            self.assertTrue((package / "k3-separator").stat().st_mode & 0o111)
            self.assertTrue((package / "separate.sh").stat().st_mode & 0o111)
            self.assertEqual((package / "k3-gui").read_bytes(), b"native GUI")
            self.assertTrue((package / "share/applications/k3.desktop").is_file())
            self.assertTrue((package / "share/icons/hicolor/scalable/apps/k3.svg").is_file())
            self.assertTrue((package / "licenses/SourceHanSansCN-OFL.txt").is_file())
            self.assertTrue((package / "licenses/LicenseRef-Slint-Royalty-free-2.0.md").is_file())
            expected = archive.with_name(archive.name + ".sha256").read_text().split()[0]
            self.assertEqual(expected, hashlib.sha256(archive.read_bytes()).hexdigest())

    def test_linux_arm64_bundle_keeps_architecture_and_rejects_x86_runtime(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary, runtime = self.fixture(root, "linux", "aarch64")
            archive = packager.package("linux", "k3-linux-aarch64", binary, runtime, False, root / "dist")
            with tarfile.open(archive) as stream:
                manifest = json.load(stream.extractfile("k3-linux-aarch64/bundle-manifest.json"))
                self.assertEqual(manifest["machine"], "aarch64")
                self.assertIn("k3-linux-aarch64/k3-gui", stream.getnames())
                self.assertIn("k3-linux-aarch64/models/checkpoint.onnx", stream.getnames())
            with self.assertRaisesRegex(ValueError, "platform or architecture"):
                packager.package("linux", "k3-linux-x86_64", binary, runtime, False, root / "dist")
            self.assertFalse((root / "dist/k3-linux-x86_64.tar.gz").exists())

    def test_windows_zip_contains_python_models_and_native_launcher(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary, runtime = self.fixture(root, "windows")
            (runtime / "python/python.exe").write_bytes(b"windows python")
            archive = packager.package("windows", "k3-windows-x86_64", binary, runtime, False, root / "dist")
            with zipfile.ZipFile(archive) as stream:
                names = set(stream.namelist())
                self.assertEqual(stream.read("k3-windows-x86_64/LICENSE"), (packager.REPO / "LICENSE").read_bytes())
                self.assertIn("k3-windows-x86_64/runtime/python/python.exe", names)
                self.assertIn("k3-windows-x86_64/models/checkpoint.onnx", names)
                self.assertIn("k3-windows-x86_64/k3-separator.exe", names)
                self.assertIn("k3-windows-x86_64/k3-gui.exe", names)
                self.assertIn("k3-windows-x86_64/licenses/SourceHanSansCN-OFL.txt", names)
                self.assertIn("k3-windows-x86_64/install-separator.ps1", names)
                self.assertFalse(any("__pycache__" in name for name in names))
                self.assertEqual({name.split("/docs/", 1)[1] for name in names if "/docs/" in name},
                                 {"user-manual.md", "user-manual.zh-Hans.md", "user-manual.zh-Hant.md",
                                  "offline-package.md", "install-packages.md", "install-packages.zh-Hans.md"})
                self.assertNotIn("k3-windows-x86_64/README.md", names)
                self.assertNotIn("k3-windows-x86_64/INSTALL.md", names)

    def test_windows_arm64_cli_archive_contains_only_cli_and_documentation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary, _runtime = self.fixture(root, "windows")
            archive = packager.package("windows", "k3-windows-aarch64-cli", binary, None, True, root / "dist")
            with zipfile.ZipFile(archive) as stream:
                names = set(stream.namelist())
                self.assertIn("k3-windows-aarch64-cli/k3.exe", names)
                self.assertFalse(any("/runtime/" in name or "/models/" in name for name in names))
                self.assertNotIn("k3-windows-aarch64-cli/k3-gui.exe", names)
                self.assertNotIn("k3-windows-aarch64-cli/k3-separator.exe", names)
                self.assertEqual({name.split("/docs/", 1)[1] for name in names if "/docs/" in name},
                                 {"user-manual.md", "user-manual.zh-Hans.md", "user-manual.zh-Hant.md",
                                  "offline-package.md", "install-packages.md", "install-packages.zh-Hans.md"})
                self.assertNotIn("k3-windows-aarch64-cli/README.md", names)

    def test_foreign_runtime_is_rejected_before_creating_archive(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary, runtime = self.fixture(root, "linux")
            with self.assertRaisesRegex(ValueError, "platform or architecture"):
                packager.package("windows", "k3-windows-x86_64", binary, runtime, False, root / "dist")
            self.assertFalse(list((root / "dist").iterdir()))

    def test_cli_only_archive_requires_an_explicit_name(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary, _runtime = self.fixture(root, "linux")
            with self.assertRaisesRegex(ValueError, "-cli"):
                packager.package("linux", "k3-linux-x86_64", binary, None, True, root / "dist")
            archive = packager.package("linux", "k3-linux-x86_64-cli", binary, None, True, root / "dist")
            with tarfile.open(archive) as stream:
                self.assertEqual(stream.extractfile("k3-linux-x86_64-cli/LICENSE").read(),
                                 (packager.REPO / "LICENSE").read_bytes())
                self.assertFalse(any("/runtime/" in name for name in stream.getnames()))


if __name__ == "__main__":
    unittest.main()
