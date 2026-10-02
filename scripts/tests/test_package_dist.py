"""检查完整包的平台隔离、压缩格式和可移动的相对链接。"""

import hashlib
import importlib.util
import json
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "package-dist.py"
spec = importlib.util.spec_from_file_location("package_dist", SCRIPT)
packager = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packager)


class PackageTests(unittest.TestCase):
    def fixture(self, root: Path, platform: str):
        suffix = ".exe" if platform == "windows" else ""
        binary = root / f"k3{suffix}"
        binary.write_bytes(b"native program")
        (root / f"k3-separator{suffix}").write_bytes(b"native launcher")
        runtime = root / "runtime-source"
        for component in ("python", "bin", "models"):
            (runtime / component).mkdir(parents=True)
        (runtime / "models/checkpoint.onnx").write_bytes(b"checkpoint")
        (runtime / "requirements-resolved.txt").write_text("package==1.0\n")
        (runtime / "bundle-manifest.json").write_text(json.dumps({
            "platform": {"linux": "linux", "windows": "win32", "macos": "darwin"}[platform],
            "machine": "x86_64",
        }))
        return binary, runtime

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
            expected = archive.with_name(archive.name + ".sha256").read_text().split()[0]
            self.assertEqual(expected, hashlib.sha256(archive.read_bytes()).hexdigest())

    def test_windows_zip_contains_python_models_and_native_launcher(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary, runtime = self.fixture(root, "windows")
            (runtime / "python/python.exe").write_bytes(b"windows python")
            archive = packager.package("windows", "k3-windows-x86_64", binary, runtime, False, root / "dist")
            with zipfile.ZipFile(archive) as stream:
                names = set(stream.namelist())
                self.assertIn("k3-windows-x86_64/runtime/python/python.exe", names)
                self.assertIn("k3-windows-x86_64/models/checkpoint.onnx", names)
                self.assertIn("k3-windows-x86_64/k3-separator.exe", names)
                self.assertIn("k3-windows-x86_64/install-separator.ps1", names)
                self.assertFalse(any("__pycache__" in name for name in names))

    def test_foreign_runtime_is_rejected_before_creating_archive(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary, runtime = self.fixture(root, "linux")
            with self.assertRaisesRegex(ValueError, "平台或架构"):
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
                self.assertFalse(any("/runtime/" in name for name in stream.getnames()))


if __name__ == "__main__":
    unittest.main()
