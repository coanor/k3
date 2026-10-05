"""Verify installed Windows maintenance entry points from a different working directory."""

import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('windows_installation_state', REPO / 'scripts/installation_state.py')
state = importlib.util.module_from_spec(spec)
spec.loader.exec_module(state)


@unittest.skipUnless(os.name == 'nt', 'Native entry points require Windows PowerShell')
class WindowsMaintenanceTests(unittest.TestCase):
    def run_script(self, script, *arguments):
        return subprocess.run(['powershell.exe', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass',
                               '-File', str(script), *arguments], cwd=REPO, capture_output=True,
                              text=True, encoding='utf-8', errors='replace', timeout=60)

    def test_update_defaults_to_its_installed_directory(self):
        with tempfile.TemporaryDirectory(prefix='K3 维护 ') as directory:
            root = Path(directory)
            shutil.copy2(REPO / 'update.ps1', root / 'update.ps1')
            (root / 'install.ps1').write_text('''param([string]$InstallDir, [string]$Repo, [string]$Version, [switch]$Update, [switch]$Yes)
[Console]::OutputEncoding = New-Object Text.UTF8Encoding($false)
ConvertTo-Json -Compress @{InstallDir=$InstallDir; Repo=$Repo; Update=$Update.IsPresent; Yes=$Yes.IsPresent}
''', encoding='utf-8')
            result = self.run_script(root / 'update.ps1', '-Repo', 'org/repo', '-Yes')
            self.assertEqual(result.returncode, 0, result.stderr)
            forwarded = json.loads(result.stdout)
            self.assertEqual(Path(forwarded['InstallDir']).resolve(), root.resolve())
            self.assertEqual(forwarded['Repo'], 'org/repo')
            self.assertTrue(forwarded['Update'])
            self.assertTrue(forwarded['Yes'])

    def test_uninstall_defaults_to_its_installed_directory_and_keeps_recordings(self):
        import winreg

        with tempfile.TemporaryDirectory(prefix='K3 卸载 ') as directory:
            root = Path(directory) / 'installed'
            root.mkdir()
            shutil.copytree(sys.base_prefix, root / 'runtime/python',
                            ignore=shutil.ignore_patterns('__pycache__', 'site-packages', 'Scripts', 'include', 'libs', 'tcl'))
            scripts = root / 'scripts'
            scripts.mkdir()
            for name in ('installation_state.py', 'uninstall-online.py'):
                shutil.copy2(REPO / 'scripts' / name, scripts / name)
            shutil.copy2(REPO / 'uninstall.ps1', root / 'uninstall.ps1')
            (root / 'k3.exe').write_bytes(b'owned program fixture')
            state.write_state(root, 'org/repo', '0.1.3')
            state.remember_installation(root)
            self.addCleanup(state.forget_installation, root)
            key = state.WINDOWS_LOCATIONS_KEY + '\\' + state.location_identity(root)
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER, key) as handle:
                self.assertEqual(winreg.QueryValueEx(handle, 'InstallDir')[0], str(root.resolve()))
            recording = root / 'personal.wav'
            recording.write_bytes(b'personal recording')
            result = self.run_script(root / 'uninstall.ps1', '-Yes')
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
            self.assertEqual(recording.read_bytes(), b'personal recording')
            self.assertEqual(list(root.iterdir()), [recording])
            with self.assertRaises(FileNotFoundError):
                winreg.OpenKey(winreg.HKEY_CURRENT_USER, key)


if __name__ == '__main__':
    unittest.main()
