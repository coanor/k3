"""Run real model bundling during upgrades without installing heavyweight dependencies."""

from dataclasses import replace
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
from types import ModuleType
import unittest
from unittest.mock import patch


REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "scripts"))
sys.path.insert(0, str(REPO / "python/separator/src"))
spec = importlib.util.spec_from_file_location("model_reuse_installer", REPO / "scripts/install-online.py")
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)
from k3_separator import bundle
from k3_separator.models import ModelRegistry


@unittest.skipIf(os.name == "nt", "Fixture programs use POSIX shell")
class OnlineModelReuseTests(unittest.TestCase):
    def release(self, base, version):
        support = base / ("support-" + version)
        (support / "docs").mkdir(parents=True)
        (support / "docs/guide.md").write_text("User guide")
        (support / "LICENSE").write_text("License")
        (support / "online-version.json").write_text(json.dumps({"version": version}))
        installer.copy_maintenance_files(support, "macos")
        assets = base / ("assets-" + version)
        assets.mkdir()
        files = []
        for command in ("k3", "k3-separator"):
            payload = f'#!/bin/sh\nprintf "k3 {version}\\n"\n'.encode()
            asset = command + "-macos-aarch64"
            files.append({"path": command, "asset": asset, "size": len(payload),
                          "sha256": hashlib.sha256(payload).hexdigest()})
            (assets / asset).write_bytes(payload)
        manifest = {"format": 1, "version": version, "platform": "macos", "machine": "aarch64",
                    "gui": False, "runtime": True, "files": files}
        (assets / "k3-macos-aarch64.json").write_text(json.dumps(manifest))
        for path in list(assets.iterdir()):
            (assets / (path.name + ".sha256")).write_text(
                hashlib.sha256(path.read_bytes()).hexdigest() + "  " + path.name + "\n")
        return support, assets

    def install(self, prefix, release, registry, payloads, ffmpeg, cache=None, update=False, fail_check=False):
        imageio = ModuleType("imageio_ffmpeg")
        imageio.get_ffmpeg_exe = lambda: str(ffmpeg)
        separator = ModuleType("audio_separator.separator")

        class DependencySeparator:
            def __init__(self, model_file_dir, output_format):
                self.root = Path(model_file_dir)

            def download_model_and_data(self, filename):
                # Checkpoint verification/download remains the real worker implementation.
                self.assert_checkpoint(filename)
                config = self.root / (filename + ".yaml")
                if config.exists():
                    raise AssertionError("Unverified cached configuration was copied")
                config.write_text("fresh configuration")

            def assert_checkpoint(self, filename):
                if (self.root / filename).read_bytes() != payloads[filename]:
                    raise AssertionError("Invalid checkpoint reached the dependency")

        separator.Separator = DependencySeparator
        parent = ModuleType("audio_separator")
        parent.separator = separator
        requests = []

        def download(request, timeout):
            name = request.full_url.rsplit("/", 1)[1]
            requests.append(name)
            return io.BytesIO(payloads[name])

        def build_runtime(*arguments, **kwargs):
            self.assertEqual(Path(arguments[2]).name, "build-runtime.py")
            output = Path(arguments[arguments.index("--output") + 1])
            model_cache = Path(arguments[arguments.index("--model-cache") + 1]) if "--model-cache" in arguments else None
            bundle.prepare(output, model_cache, [])
            (output / "requirements-resolved.txt").write_text("Fixture dependencies")

        def check(root, version, runtime):
            self.assertTrue(runtime)
            for name, payload in payloads.items():
                self.assertEqual((root / "models" / name).read_bytes(), payload)
            if fail_check:
                (root / "models" / next(iter(payloads))).write_bytes(b"changed staged weights")
                raise RuntimeError("Fixture startup failed")

        with (
            patch.object(installer, "SUPPORT", release[0]),
            patch.object(installer.platform, "system", return_value="Darwin"),
            patch.object(installer.platform, "machine", return_value="arm64"),
            patch.object(installer, "ensure_installation_idle"),
            patch.object(installer, "run", side_effect=build_runtime),
            patch.object(installer, "check", side_effect=check),
            patch.object(bundle.ModelRegistry, "load", return_value=registry),
            patch.object(bundle.importlib.metadata, "distributions", return_value=[]),
            patch.dict(sys.modules, {"imageio_ffmpeg": imageio, "audio_separator": parent,
                                     "audio_separator.separator": separator}),
            patch.dict(os.environ),
            patch("urllib.request.urlopen", side_effect=download),
        ):
            installer.install(prefix, "org/repo", Path("uv"), release[1], cache=cache, update=update)
        return requests

    def test_upgrade_downloads_only_missing_or_changed_checkpoints(self):
        for mode in ("unchanged", "changed_hash", "missing", "legacy", "external_cache", "failed_startup"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory(prefix="K3 model reuse ") as directory:
                base = Path(directory)
                prefix = base / "existing 安装目录"
                defaults = ModelRegistry.load()
                models = tuple(defaults.select(profile) for profile in ("fast", "balanced", "quality"))
                old_payloads = {model.filename: ("old " + model.id).encode() for model in models}
                old_registry = ModelRegistry(tuple(replace(model, expected_sha256=hashlib.sha256(old_payloads[model.filename]).hexdigest()) for model in models))
                ffmpeg = base / "ffmpeg-source"
                ffmpeg.write_text("#!/bin/sh\nexit 0\n")
                ffmpeg.chmod(0o755)
                first, second = self.release(base, "0.1.1"), self.release(base, "0.1.2")
                requests = self.install(prefix, first, old_registry, old_payloads, ffmpeg)
                self.assertCountEqual(requests, list(old_payloads))
                (prefix / "recording.wav").write_bytes(b"personal recording")
                (prefix / "models/private-settings.json").write_text('{"personal": true}')
                payloads = dict(old_payloads)
                changed = models[-1].filename
                expected = []
                cache = None
                if mode == "changed_hash":
                    payloads[changed] = b"new model weights"
                    expected = [changed]
                elif mode == "missing":
                    (prefix / "models" / changed).unlink()
                    expected = [changed]
                elif mode == "legacy":
                    (prefix / "installation-state.json").unlink()
                elif mode == "external_cache":
                    cache = base / "external cache"
                    shutil.copytree(prefix / "models", cache)
                    (cache / changed).write_bytes(b"corrupt cached weights")
                    (cache / changed).chmod(0o444)
                    expected = [changed]
                registry = ModelRegistry(tuple(replace(model, expected_sha256=hashlib.sha256(payloads[model.filename]).hexdigest()) for model in models))
                before = {path.relative_to(prefix).as_posix(): path.read_bytes() for path in prefix.rglob("*") if path.is_file()}
                if mode == "failed_startup":
                    with self.assertRaisesRegex(RuntimeError, "Fixture startup failed"):
                        self.install(prefix, second, registry, payloads, ffmpeg, update=True, fail_check=True)
                    self.assertEqual({path.relative_to(prefix).as_posix(): path.read_bytes() for path in prefix.rglob("*") if path.is_file()}, before)
                else:
                    requests = self.install(prefix, second, registry, payloads, ffmpeg, cache=cache, update=True)
                    self.assertEqual(requests, expected)
                    self.assertEqual((prefix / "recording.wav").read_bytes(), b"personal recording")
                    self.assertEqual((prefix / "models/private-settings.json").read_text(), '{"personal": true}')
                    self.assertNotIn("models/private-settings.json", installer.load_state(prefix)["files"])
                    self.assertEqual(installer.load_state(prefix)["version"], "0.1.2")
                    if mode == "legacy":
                        backup, = base.glob(".k3-previous-*")
                        self.assertEqual((backup / "models" / changed).read_bytes(), old_payloads[changed])
                self.assertEqual(list(base.glob(".k3-install-*")), [])


if __name__ == "__main__":
    unittest.main()
