"""Run local online install/update/uninstall flows with real program and shell fixtures."""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

REPO = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('lifecycle_installer', REPO / 'scripts/install-online.py')
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)


@unittest.skipIf(os.name == 'nt', 'Native fixture programs require POSIX shell')
class OnlineLifecycleTests(unittest.TestCase):
    def setUp(self):
        locations = tempfile.TemporaryDirectory()
        self.addCleanup(locations.cleanup)
        self.locations = Path(locations.name)
        environment = patch.dict(os.environ, XDG_STATE_HOME=locations.name, HOME=locations.name)
        environment.start()
        self.addCleanup(environment.stop)

    def release(self, base, version):
        support = base / ('support-' + version)
        support.mkdir()
        (support / 'docs').mkdir()
        (support / 'docs/guide.md').write_text('New release guide: ' + version)
        (support / 'LICENSE').write_text('License')
        (support / 'online-version.json').write_text(json.dumps({'version': version}))
        installer.copy_maintenance_files(support, 'macos')
        assets = base / ('assets-' + version)
        assets.mkdir()
        binary = f'#!/bin/sh\nprintf "k3 {version}\\n"\n'.encode()
        entry = {'path': 'k3', 'asset': 'k3-macos-x86_64', 'size': len(binary),
                 'sha256': hashlib.sha256(binary).hexdigest()}
        manifest = {'format': 1, 'version': version, 'platform': 'macos', 'machine': 'x86_64',
                    'gui': False, 'runtime': False, 'files': [entry]}
        for name, payload in [(entry['asset'], binary), ('k3-macos-x86_64.json', json.dumps(manifest).encode())]:
            (assets / name).write_bytes(payload)
            (assets / (name + '.sha256')).write_text(hashlib.sha256(payload).hexdigest() + '  ' + name + '\n')
        return support, assets

    def install(self, prefix, support, assets, management, update=False):
        with patch.object(installer, 'SUPPORT', support), patch.object(installer.platform, 'system', return_value='Darwin'), patch.object(installer.platform, 'machine', return_value='x86_64'), patch.object(installer, 'ensure_installation_idle'):
            installer.install(prefix, 'org/repo', Path('uv'), assets, update=update, management_python=management)

    def test_upgrade_preserves_recording_and_installed_uninstaller_removes_only_owned_files(self):
        with tempfile.TemporaryDirectory(prefix='K3 升级 ') as directory:
            base = Path(directory)
            management = base / 'python'
            (management / 'bin').mkdir(parents=True)
            (management / 'bin/python3').symlink_to(sys.executable)
            first = self.release(base, '0.1.1')
            second = self.release(base, '0.1.2')
            prefix = base / 'selected disk/K3'
            self.install(prefix, *first, management)
            (prefix / 'recording.wav').write_bytes(b'personal recording')
            self.install(prefix, *second, management, update=True)
            records = list(self.locations.rglob("*.path"))
            self.assertEqual(len(records), 1)
            self.assertEqual(records[0].read_text(encoding="utf-8").strip(), str(prefix.resolve()))
            self.assertEqual(subprocess.check_output([str(prefix / 'k3'), '--version'], text=True).strip(), 'k3 0.1.2')
            self.assertEqual((prefix / 'docs/guide.md').read_text(), 'New release guide: 0.1.2')
            self.assertEqual((prefix / 'recording.wav').read_bytes(), b'personal recording')
            result = subprocess.run(['bash', str(prefix / 'uninstall.sh'), '--yes'], capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(list(prefix.iterdir()), [prefix / 'recording.wav'])
            self.assertEqual(list(self.locations.rglob("*.path")), [])
            self.assertEqual((prefix / 'recording.wav').read_bytes(), b'personal recording')

    def test_tampered_update_does_not_modify_previous_installation(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            first, second = self.release(base, '0.1.1'), self.release(base, '0.1.2')
            prefix = base / 'K3'
            self.install(prefix, *first, None)
            (second[1] / 'k3-macos-x86_64').write_bytes(b'tampered')
            with self.assertRaises(ValueError):
                self.install(prefix, *second, None, update=True)
            self.assertEqual(subprocess.check_output([str(prefix / 'k3')], text=True).strip(), 'k3 0.1.1')
            self.assertEqual(list(base.glob('.k3-install-*')), [])

    def test_existing_release_without_state_can_migrate_with_complete_backup(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            first, second = self.release(base, '0.1.1'), self.release(base, '0.1.2')
            prefix = base / 'K3'
            self.install(prefix, *first, None)
            (prefix / 'installation-state.json').unlink()
            (prefix / 'recording.wav').write_bytes(b'user recording')
            self.install(prefix, *second, None, update=True)
            backups = list(base.glob('.k3-previous-*'))
            self.assertEqual(len(backups), 1)
            self.assertEqual((backups[0] / 'recording.wav').read_bytes(), b'user recording')
            self.assertEqual((prefix / 'recording.wav').read_bytes(), b'user recording')
            self.assertEqual(subprocess.check_output([str(prefix / 'k3')], text=True).strip(), 'k3 0.1.2')

    def test_update_refuses_to_downgrade_to_an_older_latest_release(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            first, second = self.release(base, '0.1.2'), self.release(base, '0.1.1')
            prefix = base / 'K3'
            self.install(prefix, *first, None)
            with self.assertRaisesRegex(ValueError, 'refusing to downgrade'):
                self.install(prefix, *second, None, update=True)
            self.assertEqual(subprocess.check_output([str(prefix / 'k3')], text=True).strip(), 'k3 0.1.2')


if __name__ == '__main__':
    unittest.main()
