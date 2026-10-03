"""验证在线发行的完整性、目标平台限制及失败时的目录保护。"""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
import urllib.request
import zipfile

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


producer = load("online_producer", "package-online.py")
installer = load("online_installer", "install-online.py")


class OnlineInstallerTests(unittest.TestCase):
    def fixture(self, base, platform="macos", machine="x86_64"):
        binary = base / "k3"
        binary.write_text(f'#!/bin/sh\nprintf "k3 {producer.VERSION}\\n"\n')
        binary.chmod(0o755)
        assets = base / "assets"
        manifest = producer.programs(platform, machine, binary, assets)
        return assets, json.loads(manifest.read_text())

    def test_support_is_small_and_contains_only_the_three_user_manuals(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = producer.support(Path(directory))
            with zipfile.ZipFile(archive) as stream:
                names = stream.namelist()
                self.assertEqual({name for name in names if name.startswith("docs/")},
                                 {"docs/" + name for name in producer.MANUALS})
                self.assertIn("scripts/install-online.py", names)
                self.assertIn("python/separator/src/k3_separator/bundle.py", names)
                self.assertFalse(any(name.startswith(("runtime/", "models/")) for name in names))
            self.assertLess(archive.stat().st_size, 1024**2)

    def test_manifest_rejects_traversal_missing_binary_wrong_version_and_wrong_capability(self):
        valid = {"format": 1, "version": producer.VERSION, "platform": "windows", "machine": "aarch64",
                 "gui": False, "runtime": False, "files": [
                     {"path": "k3.exe", "asset": "k3-windows-aarch64.exe", "size": 123,
                      "sha256": "a" * 64}]}
        installer.validate_manifest(valid, producer.VERSION, "windows", "aarch64")
        for changes in ({"version": "999.0.0"}, {"gui": True}, {"runtime": True}, {"files": []},
                        {"files": [dict(valid["files"][0], path="../outside")]},
                        {"files": [dict(valid["files"][0], asset="../../outside.exe")]}):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                installer.validate_manifest(dict(valid, **changes), producer.VERSION, "windows", "aarch64")

    def test_download_rejects_modified_program_before_execution(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            asset = base / "program"
            asset.write_bytes(b"original")
            producer.checksum(asset)
            asset.write_bytes(b"modified")
            with self.assertRaisesRegex(ValueError, "完整性"):
                installer.download("program", base / "downloaded", "unused", base)

    def test_checksum_must_match_manifest_and_the_named_asset(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            asset = base / "program"
            asset.write_bytes(b"payload")
            producer.checksum(asset)
            with self.assertRaisesRegex(ValueError, "不一致"):
                installer.download("program", base / "downloaded", "unused", base, "0" * 64)
            (base / "program.sha256").write_text("a" * 64 + "  another-program\n")
            with self.assertRaisesRegex(ValueError, "无效校验"):
                installer.download("program", base / "downloaded", "unused", base)

    def test_download_refuses_an_oversized_payload_before_copying_it(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            asset = base / "program"
            asset.write_bytes(b"unexpectedly large")
            digest = producer.checksum(asset)
            with self.assertRaisesRegex(ValueError, "大小上限"):
                installer.download("program", base / "downloaded", "unused", base, digest, 3)
            self.assertFalse((base / "downloaded").exists())

    def test_redirect_removes_repository_credentials_and_rejects_http(self):
        request = urllib.request.Request("https://api.github.com/repos/org/repo/releases/assets/1",
                                         headers={"Authorization": "Bearer secret"})
        redirect = installer.HTTPSRedirect()
        result = redirect.redirect_request(request, None, 302, "Found", {}, "https://cdn.example/asset")
        self.assertFalse(result.has_header("Authorization"))
        with self.assertRaisesRegex(RuntimeError, "HTTPS"):
            redirect.redirect_request(request, None, 302, "Found", {}, "http://cdn.example/asset")

    def test_malformed_credentials_are_rejected_without_echoing_the_secret(self):
        with patch.dict(os.environ, GITHUB_TOKEN="private-secret\ninvalid"):
            with self.assertRaisesRegex(ValueError, "GITHUB_TOKEN 格式") as error:
                installer.request("https://api.github.com/repos/org/repo/releases/latest")
            self.assertNotIn("private-secret", str(error.exception))

    def test_existing_user_files_are_rejected_before_any_download(self):
        with tempfile.TemporaryDirectory() as directory:
            prefix = Path(directory) / "K3"
            prefix.mkdir()
            saved = prefix / "my-song.wav"
            saved.write_bytes(b"user audio")
            with patch.object(installer, "download") as download, self.assertRaisesRegex(ValueError, "覆盖"):
                installer.install(prefix, "org/repo", Path("uv"), None)
            download.assert_not_called()
            self.assertEqual(saved.read_bytes(), b"user audio")

    @unittest.skipIf(os.name == "nt", "进程夹具需要 POSIX shell")
    def test_cli_install_commits_only_verified_files_to_the_selected_directory(self):
        with tempfile.TemporaryDirectory(prefix="K3 安装 ") as directory:
            base = Path(directory)
            assets, manifest = self.fixture(base)
            support = base / "support"
            support.mkdir()
            (support / "online-version.json").write_text(json.dumps({"version": producer.VERSION}))
            (support / "docs").mkdir()
            (support / "docs/user-manual.md").write_text("用户手册")
            prefix = base / "选择的磁盘/安装目录"
            with patch.object(installer, "SUPPORT", support), patch.object(installer.platform, "system", return_value="Darwin"), patch.object(installer.platform, "machine", return_value="x86_64"):
                installer.install(prefix, "org/repo", Path("uv"), assets)
            self.assertEqual((prefix / "k3").read_bytes(), (assets / manifest["files"][0]["asset"]).read_bytes())
            self.assertEqual({path.name for path in prefix.iterdir()}, {"k3", "docs", "install-manifest.json"})
            self.assertEqual(list(prefix.parent.glob(".k3-install-*")), [])

    @unittest.skipIf(os.name == "nt", "进程夹具需要 POSIX shell")
    def test_failed_startup_leaves_the_existing_empty_destination_untouched(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            assets, _ = self.fixture(base)
            support = base / "support"
            support.mkdir()
            (support / "online-version.json").write_text(json.dumps({"version": producer.VERSION}))
            (support / "docs").mkdir()
            prefix = base / "K3"
            prefix.mkdir()
            with patch.object(installer, "SUPPORT", support), patch.object(installer.platform, "system", return_value="Darwin"), patch.object(installer.platform, "machine", return_value="x86_64"), patch.object(installer, "check", side_effect=RuntimeError("启动失败")):
                with self.assertRaisesRegex(RuntimeError, "启动失败"):
                    installer.install(prefix, "org/repo", Path("uv"), assets)
            self.assertTrue(prefix.is_dir())
            self.assertEqual(list(prefix.iterdir()), [])
            self.assertEqual(list(base.glob(".k3-install-*")), [])


if __name__ == "__main__":
    unittest.main()
