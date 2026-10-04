"""Exercise public installation when the anonymous GitHub API is rate limited."""

import hashlib
import importlib.util
import io
import json
import shutil
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
import urllib.error
import urllib.request
import zipfile


REPO = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("release_installer", REPO / "scripts/install-online.py")
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)


class PublicReleaseDownloadTests(unittest.TestCase):
    def setUp(self):
        installer.release_assets.cache_clear()

    def bootstrap_source(self, filename):
        text = (REPO / filename).read_text(encoding="utf-8-sig")
        return (text.split("<<'PY'\n", 1)[1].split("\nPY\n", 1)[0] if filename.endswith(".sh")
                else text.split("$bootstrap = @'\n", 1)[1].split("\n'@", 1)[0])

    def support_payloads(self):
        archive = io.BytesIO()
        with zipfile.ZipFile(archive, "w") as stream:
            stream.writestr("online-version.json", json.dumps({"version": "0.1.1"}))
        payload = archive.getvalue()
        return {"k3-install-support.zip": payload,
                "k3-install-support.zip.sha256": (hashlib.sha256(payload).hexdigest() + "  k3-install-support.zip\n").encode()}

    def limited_opener(self, payloads, requests):
        def open_request(request, timeout):
            url = request.full_url
            requests.append(url)
            if url.startswith("https://api.github.com/"):
                raise urllib.error.HTTPError(url, 403, "rate limit exceeded", {}, None)
            self.assertFalse(request.has_header("Authorization"))
            self.assertIn(url, payloads)
            return io.BytesIO(payloads[url])
        return type("Opener", (), {"open": staticmethod(open_request)})()

    def test_both_entry_bootstraps_work_with_a_rate_limited_api(self):
        for filename in ("install.sh", "install.ps1"):
            for version, token in (("v0.1.1", None), ("latest", None), ("v0.1.1", "fixture-token"), ("latest", "fixture-token")):
                with self.subTest(entry=filename, version=version, token_present=bool(token)), tempfile.TemporaryDirectory() as directory:
                    route = "latest/download" if version == "latest" else "download/" + version
                    base = "https://github.com/coanor/k3/releases/" + route
                    payloads = {base + "/" + name: content for name, content in self.support_payloads().items()}
                    requests = []
                    with patch.dict(os.environ, {"GITHUB_TOKEN": token} if token else {}, clear=True), patch.object(sys, "argv", [
                            "bootstrap", "coanor/k3", version, directory, "-" if filename.endswith(".ps1") else ""]), patch.object(
                            urllib.request, "build_opener", return_value=self.limited_opener(payloads, requests)):
                        exec(compile(self.bootstrap_source(filename), filename + ":bootstrap", "exec"), {})
                    self.assertEqual(set(requests), set(payloads))
                    self.assertEqual(json.loads((Path(directory) / "support/online-version.json").read_text()),
                                     {"version": "0.1.1"})

    @unittest.skipIf(os.name == "nt", "Native CLI fixture requires a POSIX shell")
    def test_program_install_works_with_a_rate_limited_api(self):
        payload = b'#!/bin/sh\nprintf "k3 0.1.1\\n"\n'
        digest = hashlib.sha256(payload).hexdigest()
        base = "https://github.com/coanor/k3/releases/download/v0.1.1"
        manifest = json.dumps({"format": 1, "version": "0.1.1", "platform": "macos", "machine": "x86_64",
                               "gui": False, "runtime": False, "files": [{"path": "k3", "asset": "k3-macos-x86_64",
                                                                          "size": len(payload), "sha256": digest}]}).encode()
        payloads = {}
        for name, content in (("k3-macos-x86_64", payload), ("k3-macos-x86_64.json", manifest)):
            payloads[base + "/" + name] = content
            payloads[base + "/" + name + ".sha256"] = (hashlib.sha256(content).hexdigest() + "  " + name + "\n").encode()
        requests = []
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {}, clear=True), patch.object(
                urllib.request, "build_opener", return_value=self.limited_opener(payloads, requests)):
            support = Path(directory) / "support"
            support.mkdir()
            installer.copy_maintenance_files(support, "macos")
            (support / "online-version.json").write_text(json.dumps({"version": "0.1.1"}))
            (support / "docs").mkdir()
            (support / "LICENSE").write_text("License fixture")
            prefix = Path(directory) / "selected disk/K3"
            with patch.object(installer, "SUPPORT", support), patch.object(installer.platform, "system", return_value="Darwin"), patch.object(
                    installer.platform, "machine", return_value="x86_64"):
                installer.install(prefix, "coanor/k3", Path("uv"), None)
            self.assertEqual((prefix / "k3").read_bytes(), payload)
        self.assertEqual(set(requests), set(payloads))

    def private_opener(self, contents, requests):
        api = "https://api.github.com/repos/coanor/k3/releases/"
        names = list(contents)
        metadata = json.dumps({"tag_name": "v0.1.1", "assets": [
            {"name": name, "url": api + "assets/" + str(index)} for index, name in enumerate(names)]}).encode()
        payloads = {api + "tags/v0.1.1": metadata, api + "latest": metadata}
        payloads.update({api + "assets/" + str(index): contents[name] for index, name in enumerate(names)})
        def open_request(request, timeout):
            url = request.full_url
            requests.append(url)
            if url.startswith("https://github.com/"):
                self.assertFalse(request.has_header("Authorization"))
                raise urllib.error.HTTPError(url, 404, "Not Found", {}, None)
            self.assertEqual(request.get_header("Authorization"), "Bearer fixture-token")
            self.assertIn(url, payloads)
            return io.BytesIO(payloads[url])
        return type("Opener", (), {"open": staticmethod(open_request)})()

    def test_private_bootstraps_retain_authenticated_api_fallback(self):
        for filename in ("install.sh", "install.ps1"):
            for version in ("v0.1.1", "latest"):
                requests = []
                with self.subTest(entry=filename, version=version), tempfile.TemporaryDirectory() as directory, patch.dict(
                        os.environ, {"GITHUB_TOKEN": "fixture-token"}, clear=True), patch.object(sys, "argv", [
                        "bootstrap", "coanor/k3", version, directory, "-" if filename.endswith(".ps1") else ""]), patch.object(
                        urllib.request, "build_opener", return_value=self.private_opener(self.support_payloads(), requests)):
                    exec(compile(self.bootstrap_source(filename), filename + ":bootstrap", "exec"), {})
                    self.assertTrue((Path(directory) / "support/online-version.json").exists())
                    self.assertEqual(len(requests), 4)

    def test_private_program_download_retains_authenticated_api_fallback(self):
        content = b"private program fixture"
        digest = hashlib.sha256(content).hexdigest()
        contents = {"k3.exe": content, "k3.exe.sha256": (digest + "  k3.exe\n").encode()}
        requests = []
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {"GITHUB_TOKEN": "fixture-token"}, clear=True), patch.object(
                urllib.request, "build_opener", return_value=self.private_opener(contents, requests)):
            output = Path(directory) / "k3.exe"
            installer.download("k3.exe", output, "https://github.com/coanor/k3/releases/download/v0.1.1", None, digest, len(content))
            self.assertEqual(output.read_bytes(), content)
        self.assertEqual(requests.count("https://api.github.com/repos/coanor/k3/releases/tags/v0.1.1"), 1)

    def test_public_program_download_rejects_tampering_and_oversized_payloads(self):
        base = "https://github.com/coanor/k3/releases/download/v0.1.1"
        digest = hashlib.sha256(b"original").hexdigest()
        for payload, size, message in ((b"tampered", None, "integrity"), (b"oversized", 3, "size limit")):
            with self.subTest(message=message), tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {}, clear=True), patch.object(
                    urllib.request, "build_opener", return_value=self.limited_opener({
                        base + "/k3.exe": payload, base + "/k3.exe.sha256": (digest + "  k3.exe\n").encode()}, [])):
                with self.assertRaisesRegex(ValueError, message):
                    installer.download("k3.exe", Path(directory) / "k3.exe", base, None, digest, size)

    def test_public_bootstraps_reject_bad_checksums_and_wrong_versions(self):
        for filename in ("install.sh", "install.ps1"):
            for version, tamper, message in (("v0.1.1", True, "SHA-256 mismatch"), ("v0.1.2", False, "Requested version")):
                contents = self.support_payloads()
                if tamper:
                    contents["k3-install-support.zip.sha256"] = ("0" * 64 + "  k3-install-support.zip\n").encode()
                base = "https://github.com/coanor/k3/releases/download/" + version
                with self.subTest(entry=filename, message=message), tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {}, clear=True), patch.object(
                        sys, "argv", ["bootstrap", "coanor/k3", version, directory, ""]), patch.object(
                        urllib.request, "build_opener", return_value=self.limited_opener({base + "/" + name: content for name, content in contents.items()}, [])):
                    with self.assertRaisesRegex(RuntimeError, message):
                        exec(compile(self.bootstrap_source(filename), filename + ":bootstrap", "exec"), {})


if __name__ == "__main__":
    unittest.main()
