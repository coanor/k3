"""Exercise update rollback, legacy migration and user-data preservation."""

import importlib.util
import hashlib
import json
import shutil
import subprocess
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / 'installation_state.py'
spec = importlib.util.spec_from_file_location('installation_state', SCRIPT)
state = importlib.util.module_from_spec(spec)
spec.loader.exec_module(state)


class InstallationStateTests(unittest.TestCase):
    def installation(self, root, value, version='1.0.0'):
        (root / 'runtime').mkdir(parents=True)
        (root / 'k3').write_bytes(value)
        (root / 'runtime/dependency').write_bytes(value)
        state.write_state(root, 'org/repo', version)

    def test_update_and_uninstall_preserve_projects_and_added_models(self):
        with tempfile.TemporaryDirectory() as directory:
            root, staged = Path(directory) / 'K3', Path(directory) / 'new'
            self.installation(root, b'old')
            self.installation(staged, b'new', '1.1.0')
            (root / 'projects').mkdir()
            (root / 'projects/recording.wav').write_bytes(b'user audio')
            (root / 'runtime/extra').write_bytes(b'user dependency')
            with state.installation_lock(root):
                state.replace_installation(root, staged)
            self.assertEqual((root / 'k3').read_bytes(), b'new')
            self.assertEqual(state.load_state(root)['version'], '1.1.0')
            state.uninstall(root)
            self.assertEqual((root / 'projects/recording.wav').read_bytes(), b'user audio')
            self.assertEqual((root / 'runtime/extra').read_bytes(), b'user dependency')
            self.assertFalse((root / 'k3').exists())

    def test_failed_move_rolls_back_all_old_files_and_state(self):
        with tempfile.TemporaryDirectory() as directory:
            root, staged = Path(directory) / 'K3', Path(directory) / 'new'
            self.installation(root, b'old')
            self.installation(staged, b'new', '1.1.0')
            previous = (root / state.STATE).read_bytes()
            original = Path.rename
            def failing(path, target):
                if path == staged / 'runtime/dependency':
                    raise PermissionError('file in use')
                return original(path, target)
            with patch.object(Path, 'rename', failing), self.assertRaises(PermissionError):
                state.replace_installation(root, staged)
            self.assertEqual((root / 'k3').read_bytes(), b'old')
            self.assertEqual((root / 'runtime/dependency').read_bytes(), b'old')
            self.assertEqual((root / state.STATE).read_bytes(), previous)
            self.assertEqual(list(Path(directory).glob('.k3-rollback-*')), [])

    def test_changed_files_and_new_file_collisions_abort_before_mutation(self):
        for changed in [False, True]:
            with self.subTest(changed=changed), tempfile.TemporaryDirectory() as directory:
                root, staged = Path(directory) / 'K3', Path(directory) / 'new'
                self.installation(root, b'old')
                self.installation(staged, b'new', '1.1.0')
                if changed:
                    (root / 'k3').write_bytes(b'edited')
                else:
                    (root / 'added').write_bytes(b'user data')
                    (staged / 'added').write_bytes(b'release file')
                    state.write_state(staged, 'org/repo', '1.1.0')
                previous = (root / state.STATE).read_bytes()
                with self.assertRaisesRegex(ValueError, 'changed or unregistered'):
                    state.replace_installation(root, staged)
                self.assertEqual((root / 'runtime/dependency').read_bytes(), b'old')
                self.assertEqual((root / state.STATE).read_bytes(), previous)

    @unittest.skipIf(os.name == 'nt', 'Directory symlinks require extra Windows privileges')
    def test_symlinked_parent_cannot_delete_external_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / 'K3'
            self.installation(root, b'old')
            outside = Path(directory) / 'outside'
            (root / 'runtime').rename(outside)
            (root / 'runtime').symlink_to(outside, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, 'symbolic link'):
                state.uninstall(root)
            self.assertEqual((outside / 'dependency').read_bytes(), b'old')
            self.assertEqual((root / 'k3').read_bytes(), b'old')

    def test_legacy_upgrade_keeps_complete_backup_and_carries_personal_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root, staged = Path(directory) / 'K3', Path(directory) / 'new'
            self.installation(root, b'old')
            self.installation(staged, b'new', '1.1.0')
            (root / 'song.wav').write_bytes(b'user audio')
            (root / 'runtime/custom.py').write_bytes(b'custom runtime')
            backup = state.migrate_legacy_installation(root, staged)
            self.assertEqual((root / 'song.wav').read_bytes(), b'user audio')
            self.assertEqual((backup / 'song.wav').read_bytes(), b'user audio')
            self.assertEqual((backup / 'runtime/custom.py').read_bytes(), b'custom runtime')
            self.assertNotIn('song.wav', state.load_state(root)['files'])
            self.assertFalse((root / 'runtime/custom.py').exists())

    def test_uninstall_keeps_edited_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / 'K3'
            self.installation(root, b'old')
            (root / 'k3').write_bytes(b'user changes')
            state.uninstall(root)
            self.assertEqual((root / 'k3').read_bytes(), b'user changes')

    def test_concurrent_maintenance_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / 'K3'
            with state.installation_lock(root):
                with self.assertRaisesRegex(RuntimeError, 'Another maintenance'):
                    with state.installation_lock(root):
                        self.fail('lock should not be acquired twice')
            self.assertEqual(list(Path(directory).iterdir()), [])

    @unittest.skipUnless(os.name == 'posix' and Path('/proc').is_dir(), 'Process validation requires Linux procfs')
    def test_running_program_prevents_maintenance_before_deletion(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / 'K3'
            self.installation(root, b'old')
            shutil.copy2(shutil.which('sleep'), root / 'sleep')
            process = subprocess.Popen([str(root / 'sleep'), '30'])
            try:
                with self.assertRaisesRegex(RuntimeError, 'Close K3'):
                    state.uninstall(root)
                self.assertEqual((root / 'k3').read_bytes(), b'old')
            finally:
                process.terminate()
                process.wait(timeout=5)

    def test_recovery_directory_retains_originals_if_rollback_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root, staged = Path(directory) / 'K3', Path(directory) / 'new'
            self.installation(root, b'old')
            self.installation(staged, b'new')
            original = Path.rename
            def failing(path, target):
                if path == staged / 'runtime/dependency' or (path.name == 'k3' and path.parent.name.startswith('.k3-rollback-')):
                    raise PermissionError('file in use')
                return original(path, target)
            with patch.object(Path, 'rename', failing), self.assertRaisesRegex(RuntimeError, 'original files are preserved'):
                state.replace_installation(root, staged)
            recovery = list(Path(directory).glob('K3.k3-recovery-*'))
            self.assertEqual(len(recovery), 1)
            self.assertEqual((recovery[0] / 'k3').read_bytes(), b'old')

    def test_uninstall_removes_only_unchanged_generated_shortcuts(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            root = home / 'K3'
            self.installation(root, b'old')
            identity = hashlib.sha256(str(root.resolve()).encode()).hexdigest()[:12]
            unchanged = home / f'k3-{identity}.desktop'
            edited = home / f'K3-{identity}.desktop'
            for path in [unchanged, edited]:
                path.write_bytes(b'original shortcut')
            entries = [{'path': str(path), 'sha256': state.signature(path)['sha256']} for path in [unchanged, edited]]
            (root / 'shortcuts.json').write_text(json.dumps(entries))
            state.write_state(root, 'org/repo', '1.0.0')
            edited.write_bytes(b'user changed shortcut')
            with patch.object(Path, 'home', return_value=home):
                state.uninstall(root)
            self.assertFalse(unchanged.exists())
            self.assertEqual(edited.read_bytes(), b'user changed shortcut')


if __name__ == '__main__':
    unittest.main()
