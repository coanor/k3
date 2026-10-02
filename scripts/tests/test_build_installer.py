"""验证安装载荷链接、权限与不安全归档的拒绝行为。"""

import hashlib
import importlib.util
import io
import os
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "build-installer.py"
spec = importlib.util.spec_from_file_location("build_installer", SCRIPT)
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)


class InstallerTests(unittest.TestCase):
    def checksum(self, archive):
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        archive.with_name(archive.name + ".sha256").write_text(f"{digest}  {archive.name}\n")

    def test_modified_archive_is_rejected_before_extracting(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "k3-windows-x86_64.zip"
            archive.write_bytes(b"first")
            self.checksum(archive)
            archive.write_bytes(b"second")
            destination = Path(directory) / "unpacked"
            destination.mkdir()
            with self.assertRaisesRegex(ValueError, "SHA-256"):
                installer.unpack(archive, destination)
            self.assertEqual(list(destination.iterdir()), [])

    def test_zip_traversal_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "k3-windows-x86_64.zip"
            for unsafe in ("../outside", "k3-windows-x86_64/../../outside",
                           "k3-windows-x86_64\\..\\outside", "C:/outside"):
                with self.subTest(path=unsafe):
                    with zipfile.ZipFile(archive, "w") as stream:
                        stream.writestr(unsafe, b"invalid")
                    self.checksum(archive)
                    destination = Path(directory) / "unpacked"
                    destination.mkdir(exist_ok=True)
                    with self.assertRaisesRegex(ValueError, "不安全"):
                        installer.unpack(archive, destination)
                    self.assertFalse((Path(directory) / "outside").exists())

    def test_tar_cannot_escape_destination(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "k3-linux-x86_64.tar.gz"
            with tarfile.open(archive, "w:gz") as stream:
                member = tarfile.TarInfo("../outside")
                member.size = 3
                stream.addfile(member, io.BytesIO(b"bad"))
            self.checksum(archive)
            destination = Path(directory) / "unpacked"
            destination.mkdir()
            with self.assertRaises(tarfile.FilterError):
                installer.unpack(archive, destination)
            self.assertFalse((Path(directory) / "outside").exists())

    @unittest.skipIf(os.name == "nt", "Unix 安装载荷使用 Unix 权限与符号链接")
    def test_system_links_preserve_bundle_and_restrict_write_permissions(self):
        for prefix in ("opt", "usr/local/lib"):
            with self.subTest(prefix=prefix), tempfile.TemporaryDirectory() as directory:
                base = Path(directory)
                root = base / "bundle"
                (root / "runtime/python/bin").mkdir(parents=True)
                (root / "runtime/python/bin/python3.13").write_bytes(b"python")
                (root / "runtime/python/bin/python3").symlink_to("python3.13")
                (root / "k3").write_bytes(b"executable")
                (root / "k3").chmod(0o777)
                (root / "k3-separator").write_bytes(b"launcher")
                (root / "models").mkdir()
                (root / "models/model.onnx").write_bytes(b"model")
                (root / "models/model.onnx").chmod(0o666)
                staging = base / "payload"
                payload = installer.unix_payload(root, staging, prefix, False)
                command = staging / ("usr/bin/k3" if prefix == "opt" else "usr/local/bin/k3")
                self.assertEqual(command.resolve(), payload / "k3")
                self.assertFalse(os.readlink(command).startswith("/"))
                self.assertEqual((payload / "runtime/python/bin/python3").read_bytes(), b"python")
                self.assertEqual((payload / "k3").stat().st_mode & 0o777, 0o755)
                self.assertEqual((payload / "models/model.onnx").stat().st_mode & 0o777, 0o644)


if __name__ == "__main__":
    unittest.main()
