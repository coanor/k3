"""验证在线发行的完整性、目标平台限制及失败时的目录保护。"""

import base64
import hashlib
import importlib.util
import itertools
import json
import shutil
import os
from pathlib import Path
import shutil
import subprocess
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
    @unittest.skipUnless(shutil.which("powershell.exe") or shutil.which("powershell"),
                         "启动入口回归需要 Windows PowerShell")
    def test_windows_bootstrap_checks_download_before_running_utf8_installer(self):
        powershell = shutil.which("powershell.exe") or shutil.which("powershell")
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            producer.support(output)
            for name, mode in itertools.product(("get.ps1", "upgrade.ps1"),
                    ("valid", "wrong_digest", "wrong_filename", "invalid_checksum", "download_failed", "oversized")):
                with self.subTest(name=name, mode=mode):
                    bootstrap = (output / name).read_bytes()
                    self.assertTrue(bootstrap.isascii())
                    harness = """
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$ProgressPreference = 'SilentlyContinue'
$ErrorActionPreference = 'Continue'
$global:fixtureState = @{started=$false; version=$null}
$fixture = [char]0xFEFF + '[CmdletBinding()] param([string]$Version, [switch]$Update); $global:fixtureState.started=$true; $global:fixtureState.version=$Version; $global:fixtureState.update=[bool]$Update; $global:fixtureState.message="安装检查"'
$script:payload = [Text.Encoding]::UTF8.GetBytes($fixture)
if ($script:mode -eq 'oversized') { $script:payload += [byte[]]::new(262144) }
$hash = [Security.Cryptography.SHA256]::Create()
try { $digest = ([BitConverter]::ToString($hash.ComputeHash($script:payload))).Replace('-', '').ToLowerInvariant() }
finally { $hash.Dispose() }
$script:checksum = "$digest  install.ps1"
if ($script:mode -eq 'wrong_digest') { $script:checksum = ('0' * 64) + '  install.ps1' }
if ($script:mode -eq 'wrong_filename') { $script:checksum = "$digest  another.ps1" }
if ($script:mode -eq 'invalid_checksum') { $script:checksum = 'invalid' }
function Invoke-WebRequest {
    [CmdletBinding()] param([switch]$UseBasicParsing, [Parameter(Position=0)][string]$Uri)
    if ($script:mode -eq 'download_failed') { throw 'fixture download failed' }
    if ($Uri -ne "https://github.com/coanor/k3/releases/download/vEXPECTED_VERSION/install.ps1") { throw 'unexpected URL' }
    return [PSCustomObject]@{RawContentStream=[IO.MemoryStream]::new($script:payload)}
}
function Invoke-RestMethod {
    [CmdletBinding()] param([Parameter(Position=0)][string]$Uri)
    if ($Uri -ne "https://github.com/coanor/k3/releases/download/vEXPECTED_VERSION/install.ps1.sha256") { throw 'unexpected URL' }
    return $script:checksum
}
$failed = $false
try {
BOOTSTRAP_BODY
} catch { $failed = $true }
ConvertTo-Json -Compress -InputObject @{state=$global:fixtureState; failed=$failed; preference=$ErrorActionPreference.ToString()}
""".replace("EXPECTED_VERSION", producer.VERSION).replace("BOOTSTRAP_BODY", bootstrap.decode("ascii"))
                    # 将夹具编码为 UTF-16LE，避免原生命令行编码影响中文数据验证。
                    harness = f"$script:mode = '{mode}'; " + harness
                    encoded = base64.b64encode(harness.encode("utf-16le")).decode("ascii")
                    result = subprocess.check_output(
                        [powershell, "-NoProfile", "-NonInteractive", "-EncodedCommand", encoded],
                        text=True, encoding="utf-8", stderr=subprocess.PIPE, timeout=30)
                    state = json.loads(result)
                    self.assertEqual(state["preference"], "Continue")
                    self.assertEqual(state["state"]["started"], mode == "valid")
                    self.assertEqual(state["failed"], mode != "valid")
                    if mode == "valid":
                        self.assertEqual(state["state"]["version"], "v" + producer.VERSION)
                        self.assertEqual(state["state"]["message"], "安装检查")
                        self.assertEqual(state["state"]["update"], name == "upgrade.ps1")

    @unittest.skipUnless(shutil.which("powershell.exe") or shutil.which("powershell"),
                         "验证原生 Python 请求需要 Windows PowerShell")
    def test_windows_bootstrap_requests_python_for_the_selected_native_architecture(self):
        powershell = shutil.which("powershell.exe") or shutil.which("powershell")
        script = (producer.REPO / "install.ps1").read_text(encoding="utf-8-sig")
        command = next(line.strip() for line in script.splitlines()
                       if line.strip().startswith("& $uv --no-config python install "))
        for machine in ("x86_64", "aarch64"):
            with self.subTest(machine=machine):
                # 执行真实安装入口中的 uv 调用，仅替换下载程序以捕获 Python 请求。
                harness = ("$ErrorActionPreference = 'Stop'; "
                           "function Capture-Uv { $script:captured = @($args) }; "
                           f"$uv = 'Capture-Uv'; $machine = '{machine}'; "
                           "$env:UV_PYTHON_INSTALL_DIR = 'C:\\fixture path'; "
                           + command + "; ConvertTo-Json -Compress -InputObject $script:captured")
                output = subprocess.check_output(
                    [powershell, "-NoProfile", "-NonInteractive", "-Command", harness],
                    text=True, timeout=30)
                self.assertIn(f"cpython-3.13.15-windows-{machine}-none", json.loads(output))

    def fixture(self, base, platform="macos", machine="x86_64"):
        binary = base / "k3"
        binary.write_text(f'#!/bin/sh\nprintf "k3 {producer.VERSION}\\n"\n')
        binary.chmod(0o755)
        assets = base / "assets"
        manifest = producer.programs(platform, machine, binary, assets)
        return assets, json.loads(manifest.read_text())

    def test_support_is_small_and_contains_only_user_manuals(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = producer.support(Path(directory))
            with zipfile.ZipFile(archive) as stream:
                names = stream.namelist()
                self.assertEqual({name for name in names if name.startswith("docs/")},
                                 {"docs/" + name for name in producer.MANUALS})
                self.assertIn("scripts/install-online.py", names)
                self.assertEqual(stream.read("LICENSE"), (producer.REPO / "LICENSE").read_bytes())
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
            with self.assertRaisesRegex(ValueError, "integrity"):
                installer.download("program", base / "downloaded", "unused", base)

    def test_checksum_must_match_manifest_and_the_named_asset(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            asset = base / "program"
            asset.write_bytes(b"payload")
            producer.checksum(asset)
            with self.assertRaisesRegex(ValueError, "do not match"):
                installer.download("program", base / "downloaded", "unused", base, "0" * 64)
            (base / "program.sha256").write_text("a" * 64 + "  another-program\n")
            with self.assertRaisesRegex(ValueError, "Invalid checksum"):
                installer.download("program", base / "downloaded", "unused", base)

    def test_download_refuses_an_oversized_payload_before_copying_it(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            asset = base / "program"
            asset.write_bytes(b"unexpectedly large")
            digest = producer.checksum(asset)
            with self.assertRaisesRegex(ValueError, "size limit"):
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
            with self.assertRaisesRegex(ValueError, "GITHUB_TOKEN format") as error:
                installer.request("https://api.github.com/repos/org/repo/releases/latest")
            self.assertNotIn("private-secret", str(error.exception))

    def test_existing_user_files_are_rejected_before_any_download(self):
        with tempfile.TemporaryDirectory() as directory:
            prefix = Path(directory) / "K3"
            prefix.mkdir()
            saved = prefix / "my-song.wav"
            saved.write_bytes(b"user audio")
            with patch.object(installer, "download") as download, self.assertRaisesRegex(ValueError, "overwritten"):
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
            installer.copy_maintenance_files(support, "macos")
            (support / "online-version.json").write_text(json.dumps({"version": producer.VERSION}))
            (support / "docs").mkdir()
            (support / "docs/user-manual.md").write_text("用户手册")
            (support / "LICENSE").write_text("许可声明")
            prefix = base / "选择的磁盘/安装目录"
            with patch.object(installer, "SUPPORT", support), patch.object(installer.platform, "system", return_value="Darwin"), patch.object(installer.platform, "machine", return_value="x86_64"):
                installer.install(prefix, "org/repo", Path("uv"), assets)
            self.assertEqual((prefix / "k3").read_bytes(), (assets / manifest["files"][0]["asset"]).read_bytes())
            self.assertEqual({path.name for path in prefix.iterdir()}, {"k3", "docs", "LICENSE", "install-manifest.json", "installation-state.json",
                                "scripts", "install.sh", "update.sh", "uninstall.sh"})
            self.assertEqual((prefix / "LICENSE").read_text(), "许可声明")
            self.assertEqual(list(prefix.parent.glob(".k3-install-*")), [])

    @unittest.skipIf(os.name == "nt", "进程夹具需要 POSIX shell")
    def test_failed_startup_leaves_the_existing_empty_destination_untouched(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            assets, _ = self.fixture(base)
            support = base / "support"
            support.mkdir()
            installer.copy_maintenance_files(support, "macos")
            (support / "online-version.json").write_text(json.dumps({"version": producer.VERSION}))
            (support / "docs").mkdir()
            (support / "LICENSE").write_text("许可声明")
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
