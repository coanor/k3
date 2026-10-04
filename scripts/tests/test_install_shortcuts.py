"""Verify shortcut creation in isolated user directories."""

import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / 'install_shortcuts.py'
spec = importlib.util.spec_from_file_location('install_shortcuts', SCRIPT)
shortcuts = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shortcuts)


class ShortcutTests(unittest.TestCase):
    def test_menu_and_optional_desktop_entries_preserve_existing_files(self):
        with tempfile.TemporaryDirectory(prefix='K3 空格 ') as directory:
            home = Path(directory)
            (home / 'Desktop').mkdir()
            prefix = home / 'apps/K3'
            with patch.object(Path, 'home', return_value=home), patch.dict(os.environ, XDG_DATA_HOME=str(home / 'data')), patch.object(shortcuts.subprocess, 'run', side_effect=FileNotFoundError):
                first = shortcuts.create_shortcuts(prefix, 'linux', False)
                self.assertEqual(len(first), 1)
                menu = Path(first[0]['path'])
                self.assertIn(shortcuts.desktop_exec(prefix / 'k3-gui'), menu.read_text())
                menu.write_text('user edited entry')
                second = shortcuts.create_shortcuts(prefix, 'linux', True)
                self.assertEqual(len(second), 1)
                self.assertEqual(Path(second[0]['path']).parent, home / 'Desktop')
                self.assertEqual(menu.read_text(), 'user edited entry')

    def test_home_is_never_used_as_a_desktop_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            with patch.object(Path, 'home', return_value=home), patch.dict(os.environ, XDG_DATA_HOME=str(home / 'data')), patch.object(shortcuts.subprocess, 'run') as run:
                run.return_value.stdout = str(home)
                created = shortcuts.create_shortcuts(home / 'K3', 'linux', True)
                self.assertEqual(len(created), 1)
                self.assertEqual(list(home.glob('*.desktop')), [])

    @unittest.skipUnless(os.name == 'nt', 'Native shortcut creation requires Windows')
    def test_windows_links_target_the_selected_unicode_installation(self):
        with tempfile.TemporaryDirectory(prefix='K3 安装 ') as directory:
            prefix = Path(directory)
            created = shortcuts.create_shortcuts(prefix, 'windows', True)
            try:
                self.assertEqual(len(created), 2)
                for entry in created:
                    link = Path(entry['path'])
                    result = shortcuts.subprocess.run(
                        ['powershell.exe', '-NoProfile', '-NonInteractive', '-Command',
                         "[Console]::OutputEncoding = New-Object Text.UTF8Encoding($false); "
                         "$shell = New-Object -ComObject WScript.Shell; "
                         "$shell.CreateShortcut($env:K3_TEST_LINK).TargetPath"],
                        capture_output=True, check=True, text=True, encoding='utf-8', timeout=30,
                        env=dict(os.environ, K3_TEST_LINK=str(link)))
                    self.assertEqual(Path(result.stdout.strip()), prefix / 'k3-gui.exe')
                    self.assertEqual(shortcuts.create_shortcuts(prefix, 'windows', True), [])
            finally:
                for entry in created:
                    Path(entry['path']).unlink(missing_ok=True)


if __name__ == '__main__':
    unittest.main()
