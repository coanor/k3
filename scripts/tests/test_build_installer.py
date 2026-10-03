"""验证安装载荷链接、权限与不安全归档的拒绝行为。"""

import hashlib
import importlib.util
import io
import os
import subprocess
import shutil
import sys
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "build-installer.py"
sys.path.insert(0, str(SCRIPT.parent))
spec = importlib.util.spec_from_file_location("build_installer", SCRIPT)
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)
INNO = Path(os.environ.get("ProgramFiles(x86)", "")) / "Inno Setup 6/ISCC.exe"


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
                    with self.assertRaisesRegex(ValueError, "unsafe"):
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
        for prefix in ("opt",):
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
                self.assertEqual(command.resolve(), (payload / "k3").resolve())
                self.assertFalse(os.readlink(command).startswith("/"))
                self.assertEqual((payload / "runtime/python/bin/python3").read_bytes(), b"python")
                self.assertEqual((payload / "k3").stat().st_mode & 0o777, 0o755)
                self.assertEqual((payload / "models/model.onnx").stat().st_mode & 0o777, 0o644)

    @unittest.skipUnless(os.name == "nt" and INNO.is_file(), "真实 Windows 安装器编译回归需要 Inno Setup")
    def test_inno_compiles_cli_and_full_packages_with_command_line_defines(self):
        for name, cli_only in (("k3-windows-x86_64", False), ("k3-windows-aarch64-cli", True)):
            with self.subTest(package=name), tempfile.TemporaryDirectory() as directory:
                base = Path(directory)
                root = base / name
                root.mkdir()
                # 真实 PE 文件用于编译资源检查；实际程序安装/启动由原生 CI 另行验证。
                shutil.copy2(installer.sys.executable, root / "k3.exe")
                if not cli_only:
                    shutil.copy2(installer.sys.executable, root / "k3-gui.exe")
                artifact = installer.windows(root, base, "0.1.0", str(INNO), cli_only)
                self.assertTrue(artifact.is_file())
                self.assertGreater(artifact.stat().st_size, 0)

    @unittest.skipUnless(shutil.which("dpkg-deb"), "实际 DEB 校验需要 dpkg-deb")
    def test_deb_architecture_matches_payload_and_system_links_work(self):
        for machine, architecture in (("x86_64", "amd64"), ("aarch64", "arm64")):
            with self.subTest(machine=machine), tempfile.TemporaryDirectory() as directory:
                base = Path(directory)
                root = base / "bundle"
                for entry in ("k3", "k3-separator", "k3-gui", "models/checkpoint.onnx",
                              "share/applications/k3.desktop", "share/icons/hicolor/scalable/apps/k3.svg"):
                    path = root / entry
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text("fixture")
                artifact = installer.deb(root, base / "payload", base, "0.1.0", machine)
                self.assertEqual(artifact.name, f"k3_0.1.0_{architecture}.deb")
                actual = subprocess.check_output(["dpkg-deb", "--field", str(artifact), "Architecture"], text=True)
                self.assertEqual(actual.strip(), architecture)
                extracted = base / "installed"
                subprocess.run(["dpkg-deb", "--extract", str(artifact), str(extracted)], check=True)
                self.assertEqual((extracted / "usr/bin/k3").read_text(), "fixture")
                self.assertTrue((extracted / "opt/k3/models/checkpoint.onnx").is_file())

    @unittest.skipUnless(sys.platform == "linux" and shutil.which("dpkg-deb"),
                         "复用安装包回归需要 Linux 和 dpkg-deb")
    def test_reused_archive_and_installer_refresh_manuals_preserving_runtime(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            name = "k3-linux-x86_64"
            root = base / name
            for entry in ("k3-separator", "k3-gui", "runtime/python/bin/python3.13",
                          "runtime/bin/ffmpeg", "models/checkpoint.onnx", "docs/internal-spec.md",
                          "README.md", "share/applications/k3.desktop",
                          "share/icons/hicolor/scalable/apps/k3.svg"):
                path = root / entry
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"preserved payload")
            (root / "runtime/python/bin/python3").symlink_to("python3.13")
            (root / "bundle-manifest.json").write_text('{"platform":"linux","machine":"x86_64"}')
            binary = root / "k3"
            binary.write_text('#!/bin/sh\nprintf "k3 0.1.0\\n"\n')
            binary.chmod(0o755)
            archive = base / (name + ".tar.gz")
            installer.write_archive(root, archive)
            old_digest = archive.with_name(archive.name + ".sha256").read_text()
            artifact = installer.build(archive, base / "installers", "0.1.0", "unused",
                                       refresh_portable_manuals=True)
            refreshed, _, _ = installer.unpack(archive, base / "refreshed")
            installed = base / "installed"
            subprocess.run(["dpkg-deb", "--extract", str(artifact), str(installed)], check=True)
            for payload in (refreshed, installed / "opt/k3"):
                self.assertEqual({path.name for path in (payload / "docs").iterdir()},
                                 {"user-manual.md", "user-manual.zh-Hans.md", "user-manual.zh-Hant.md",
                                  "offline-package.md", "install-packages.md"})
                self.assertFalse((payload / "README.md").exists())
                self.assertEqual((payload / "models/checkpoint.onnx").read_bytes(), b"preserved payload")
                self.assertEqual(os.readlink(payload / "runtime/python/bin/python3"), "python3.13")
                self.assertEqual((payload / "k3").read_bytes(), binary.read_bytes())
            self.assertNotEqual(old_digest, archive.with_name(archive.name + ".sha256").read_text())

    @unittest.skipIf(os.name == "nt", "macOS 命令入口需要 POSIX shell")
    def test_macos_command_launches_the_real_program_with_unicode_arguments(self):
        with tempfile.TemporaryDirectory(prefix="K3 安装 ") as directory:
            base = Path(directory)
            root = base / "bundle"
            root.mkdir()
            binary = root / "k3"
            binary.write_text('#!/bin/sh\nprintf "%s\\n" "$0" "$@"\n')
            binary.chmod(0o755)
            staging = base / "payload"
            payload = installer.unix_payload(root, staging, "usr/local/lib", True)
            command = staging / "usr/local/bin/k3"
            result = subprocess.check_output([str(command), "工程 空格", "中文"], text=True).splitlines()
            # macOS current_exe 可能保留命令链接路径，入口必须先执行包内真实路径。
            self.assertEqual(Path(result[0]).parent.resolve(), payload.resolve())
            self.assertEqual(result[1:], ["工程 空格", "中文"])


if __name__ == "__main__":
    unittest.main()
