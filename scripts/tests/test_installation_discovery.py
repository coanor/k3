"""Exercise native discovery and per-user installation records without downloads."""

import importlib.util
import json
import os
from pathlib import Path
import select
import shutil
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
import uuid
from unittest.mock import patch

REPO = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("discovery_state", REPO / "scripts/installation_state.py")
state = importlib.util.module_from_spec(spec)
spec.loader.exec_module(state)


class InstallationLocationTests(unittest.TestCase):
    def test_records_survive_repeated_registration_and_remove_only_one_installation(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            first, second = base / "first 安装", base / "second"
            first.mkdir()
            second.mkdir()
            records = base / "locations"
            with patch.object(state.sys, "platform", "linux"), patch.object(state, "installation_locations_directory", return_value=records):
                state.remember_installation(first)
                state.remember_installation(first)
                state.remember_installation(second)
                self.assertEqual({path.read_text(encoding="utf-8").strip() for path in records.glob("*.path")}, {str(first), str(second)})
                state.forget_installation(first)
                self.assertEqual([path.read_text(encoding="utf-8").strip() for path in records.glob("*.path")], [str(second)])
                state.forget_installation(first)

    def test_windows_records_use_hkcu_and_preserve_other_installations(self):
        values = {}

        class Key:
            def __init__(self, name): self.name = name
            def __enter__(self): return self
            def __exit__(self, *_args): pass

        def create(hive, name):
            self.assertEqual(hive, "HKCU")
            return Key(name)

        def delete(hive, name):
            self.assertEqual(hive, "HKCU")
            del values[name]

        registry = SimpleNamespace(HKEY_CURRENT_USER="HKCU", REG_SZ=1, CreateKey=create,
            SetValueEx=lambda key, name, reserved, kind, value: values.__setitem__(key.name, {name: value}), DeleteKey=delete)
        with tempfile.TemporaryDirectory() as directory, patch.object(state.sys, "platform", "win32"), patch.dict(sys.modules, winreg=registry):
            first, second = Path(directory) / "first 安装", Path(directory) / "second"
            state.remember_installation(first)
            state.remember_installation(second)
            self.assertEqual(len(values), 2)
            self.assertTrue(all(name.startswith(state.WINDOWS_LOCATIONS_KEY + "\\") for name in values))
            self.assertEqual({entry["InstallDir"] for entry in values.values()}, {str(first), str(second)})
            state.forget_installation(first)
            self.assertEqual([entry["InstallDir"] for entry in values.values()], [str(second)])


@unittest.skipIf(os.name == "nt", "Unix installer discovery requires a POSIX terminal")
class UnixDiscoveryTests(unittest.TestCase):
    def run_entry(self, base, answers, *arguments, cwd=None, extra_environment=None):
        import fcntl
        import termios

        home = base / "home"
        home.mkdir(exist_ok=True)
        master, slave = os.openpty()
        environment = dict(os.environ, HOME=str(home), XDG_DATA_HOME=str(home / ".local/share"),
                           XDG_STATE_HOME=str(home / ".local/state"),
                           K3_ENTRY_SOURCE=str(REPO / "install.sh"))
        environment.update(extra_environment or {})
        process = subprocess.Popen(["bash", "-c", 'cat "$K3_ENTRY_SOURCE" | bash -s -- "$@"', "discovery", *arguments],
            stdin=slave, stdout=slave, stderr=slave, cwd=cwd or base, env=environment,
            preexec_fn=lambda: (os.setsid(), fcntl.ioctl(0, termios.TIOCSCTTY, 0)))
        os.close(slave)
        output = bytearray()
        try:
            os.write(master, answers.encode())
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                ready, _, _ = select.select([master], [], [], 0.1)
                if ready:
                    try: output.extend(os.read(master, 65536))
                    except OSError: break
                if process.poll() is not None and not ready: break
            process.wait(timeout=1)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            os.close(master)
        self.assertEqual(process.returncode, 0, output.decode(errors="replace"))
        self.assertIn("Cancelled. No components were downloaded.", output.decode())
        return output.decode()

    def installation(self, root):
        root.mkdir(parents=True)
        (root / "k3").write_text("#!/bin/sh\nexit 91\n")
        (root / "k3").chmod(0o755)
        (root / "install-manifest.json").write_text('{"version":"0.1.1"}')

    def record(self, base, name, root):
        directory = base / "home/.local/state/k3/online-installations"
        directory.mkdir(parents=True, exist_ok=True)
        (directory / (name + ".path")).write_text(str(root) + "\n")

    def test_installer_and_upgrader_find_a_unique_registered_unicode_installation(self):
        for arguments in ((), ("--update",)):
            with self.subTest(arguments=arguments), tempfile.TemporaryDirectory() as directory:
                base = Path(directory)
                root = base / "custom disk/旧版本"
                self.installation(root)
                self.record(base, "first", root)
                self.record(base, "duplicate", root)
                self.record(base, "stale", base / "missing")
                before = {p.name: p.read_bytes() for p in root.iterdir()}
                output = self.run_entry(base, "n\n", *arguments)
                self.assertIn(f"Found existing K3 installation: {root}", output)
                self.assertIn("Download and update all K3 components?", output)
                self.assertNotIn("Enter installation directory", output)
                self.assertEqual({p.name: p.read_bytes() for p in root.iterdir()}, before)
                self.assertFalse(list(base.glob(".k3-bootstrap.*")))

    def test_multiple_installations_require_a_selection(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            first, second = base / "first", base / "second 安装"
            for name, root in (("a", first), ("b", second)):
                self.installation(root)
                self.record(base, name, root)
            output = self.run_entry(base, "2\nn\n", "--update")
            self.assertIn("Multiple K3 installations were found", output)
            self.assertIn(f"Installation directory: {second}", output)
            self.assertNotIn("Enter installation directory", output)

    def test_legacy_default_path_symlink_and_shortcut_discovery(self):
        for source in ("default", "path", "shortcut"):
            with self.subTest(source=source), tempfile.TemporaryDirectory() as directory:
                base = Path(directory)
                root = base / ("home/.local/share/k3" if source == "default" else "custom 安装")
                self.installation(root)
                environment = {}
                if source == "path":
                    tools = base / "tools"
                    tools.mkdir()
                    (tools / "k3").symlink_to(root / "k3")
                    environment["PATH"] = str(tools) + os.pathsep + os.environ["PATH"]
                elif source == "shortcut":
                    applications = base / "home/.local/share/applications"
                    applications.mkdir(parents=True)
                    (applications / "k3-fixture.desktop").write_text(f"[Desktop Entry]\nIcon={root}/k3.svg\n")
                output = self.run_entry(base, "n\n", "--update", extra_environment=environment)
                self.assertIn(f"Found existing K3 installation: {root}", output)
                self.assertNotIn("Enter installation directory", output)

    def test_legacy_current_directory_and_explicit_directory_enter_update_mode(self):
        for explicit in (False, True):
            with self.subTest(explicit=explicit), tempfile.TemporaryDirectory() as directory:
                base = Path(directory)
                root = base / "legacy"
                self.installation(root)
                arguments = ("--prefix", str(root)) if explicit else ()
                output = self.run_entry(base, "n\n", *arguments, cwd=base if explicit else root)
                self.assertIn("Download and update all K3 components?", output)
                self.assertNotIn("Enter installation directory", output)

    def test_stale_and_symlink_records_fall_back_to_manual_selection(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = base / "outside"
            self.installation(root)
            link = base / "linked"
            link.symlink_to(root, target_is_directory=True)
            self.record(base, "link", link)
            self.record(base, "missing", base / "missing")
            output = self.run_entry(base, str(base / "fresh") + "\nn\n")
            self.assertIn("Enter installation directory", output)
            self.assertNotIn("Found existing", output)


POWERSHELL = shutil.which("powershell.exe") if os.name == "nt" else shutil.which("powershell") or shutil.which("pwsh")


@unittest.skipUnless(POWERSHELL, "Discovery requires PowerShell")
class WindowsDiscoveryTests(unittest.TestCase):
    @unittest.skipUnless(os.name == "nt", "Shell-link integration requires native Windows")
    def test_unicode_application_shortcut_finds_a_legacy_installation(self):
        shortcut_spec = importlib.util.spec_from_file_location("discovery_shortcuts", REPO / "scripts/install_shortcuts.py")
        shortcuts = importlib.util.module_from_spec(shortcut_spec)
        shortcut_spec.loader.exec_module(shortcuts)
        with tempfile.TemporaryDirectory(prefix="K3 discovery ") as directory:
            root = Path(directory) / "旧版 安装"
            root.mkdir()
            (root / "k3.exe").write_bytes(b"unused fixture")
            shutil.copy2(sys.executable, root / "k3-gui.exe")
            (root / "install-manifest.json").write_text('{"version":"0.1.1"}')
            created = shortcuts.create_shortcuts(root, "windows", False)
            try:
                source = (REPO / "install.ps1").read_text(encoding="utf-8-sig")
                discovery = source[source.index("function Find-K3OnlineInstallations"):source.index("if ($Yes -and -not $InstallDir)")]
                harness = Path(directory) / "shortcut-harness.ps1"
                harness.write_text("[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)\n" + discovery +
                    "\nConvertTo-Json -Compress -InputObject @(Find-K3OnlineInstallations)\n", encoding="utf-8-sig")
                result = subprocess.check_output([POWERSHELL, "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", str(harness)],
                    cwd=directory, text=True, encoding="utf-8", stderr=subprocess.PIPE, timeout=30)
                self.assertIn(str(root), json.loads(result))
            finally:
                for entry in created: Path(entry["path"]).unlink(missing_ok=True)

    @unittest.skipUnless(os.name == "nt", "Registry integration requires native Windows")
    def test_real_registry_records_are_discovered_and_unregistration_keeps_other_copies(self):
        import winreg

        key = r"Software\K3\DiscoveryTests" + "\\" + uuid.uuid4().hex
        with tempfile.TemporaryDirectory() as directory, patch.object(state, "WINDOWS_LOCATIONS_KEY", key):
            base = Path(directory)
            roots = [base / "first 安装", base / "second"]
            for root in roots:
                root.mkdir()
                (root / "k3.exe").write_bytes(b"unused fixture")
                (root / "installation-state.json").write_text("{}")
            try:
                for root in roots: state.remember_installation(root)
                source = (REPO / "install.ps1").read_text(encoding="utf-8-sig")
                discovery = source[source.index("function Find-K3OnlineInstallations"):source.index("if ($Yes -and -not $InstallDir)")]
                # Replace the production registry root without changing discovery itself.
                discovery = discovery.replace(r"Software\K3\OnlineInstallations", key)
                harness = base / "registry-harness.ps1"
                harness.write_text("[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)\n" + discovery +
                    "\nConvertTo-Json -Compress -InputObject @(Find-K3OnlineInstallations)\n", encoding="utf-8-sig")

                def discover():
                    result = subprocess.check_output([POWERSHELL, "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", str(harness)],
                        cwd=base, text=True, encoding="utf-8", stderr=subprocess.PIPE, timeout=30)
                    return json.loads(result)

                self.assertTrue(set(map(str, roots)).issubset(set(discover())))
                state.forget_installation(roots[0])
                found = discover()
                self.assertNotIn(str(roots[0]), found)
                self.assertIn(str(roots[1]), found)
            finally:
                for root in roots: state.forget_installation(root)
                winreg.DeleteKey(winreg.HKEY_CURRENT_USER, key)

    def test_install_and_upgrade_entries_skip_disk_selection_for_detected_locations(self):
        for multiple, upgrade in ((False, False), (False, True), (True, True)):
            with self.subTest(multiple=multiple, upgrade=upgrade), tempfile.TemporaryDirectory() as directory:
                base = Path(directory)
                roots = [base / "first 安装", base / "second"]
                for root in roots:
                    root.mkdir()
                    (root / "k3.exe").write_bytes(b"unused fixture")
                    (root / "install-manifest.json").write_text('{"version":"0.1.1"}')
                paths = roots if multiple else roots[:1]
                literal = "@(" + ",".join("'" + str(path).replace("'", "''") + "'" for path in paths) + ")"
                source = (REPO / "install.ps1").read_text(encoding="utf-8-sig")
                harness = r'''
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$env:PROCESSOR_ARCHITECTURE = 'AMD64'
$script:paths = PATHS
$script:prompts = New-Object 'System.Collections.Generic.List[string]'
function Get-CimInstance { param([string]$ClassName); [PSCustomObject]@{Version='10.0.26100'} }
function Get-ChildItem {
    [CmdletBinding()] param([string]$LiteralPath, [string]$Filter, [switch]$File)
    if ($LiteralPath -eq 'HKCU:\Software\K3\OnlineInstallations') {
        foreach ($index in 0..($script:paths.Count - 1)) { [PSCustomObject]@{PSPath=[string]$index} }
    }
}
function Get-ItemProperty { [CmdletBinding()] param([string]$LiteralPath); [PSCustomObject]@{InstallDir=$script:paths[[int]$LiteralPath]} }
function Get-PSDrive { param([string]$PSProvider) }
function Get-Command { [CmdletBinding()] param([string]$Name, [string]$CommandType, [switch]$All) }
function Read-Host {
    param([string]$Prompt)
    $script:prompts.Add($Prompt)
    if ($Prompt -eq 'Select installation number') { return '2' }
    if ($Prompt -eq 'Download and update all K3 components? [y/N]') { return 'n' }
    throw "Unexpected prompt: $Prompt"
}
& { SOURCE } ARGUMENTS
ConvertTo-Json -Compress -InputObject @($script:prompts.ToArray())
'''
                harness = harness.replace("PATHS", literal).replace("SOURCE", source).replace("ARGUMENTS", "-Update" if upgrade else "")
                harness_path = base / "entry-harness.ps1"
                harness_path.write_text(harness, encoding="utf-8-sig")
                result = subprocess.run([POWERSHELL, "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", str(harness_path)],
                    cwd=base, capture_output=True, text=True, encoding="utf-8", timeout=30)
                self.assertEqual(result.returncode, 0, result.stderr)
                prompts = json.loads(result.stdout.strip().splitlines()[-1])
                self.assertEqual(prompts, (["Select installation number"] if multiple else []) + ["Download and update all K3 components? [y/N]"])
                self.assertIn(str(roots[-1] if multiple else roots[0]), result.stdout)
                self.assertIn("Cancelled. No components were downloaded.", result.stdout)
                self.assertFalse(list(base.glob(".k3-bootstrap-*")))

    def test_registry_discovery_skips_stale_records_and_preserves_unicode_paths(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            roots = [base / "first 安装", base / "second"]
            for root in roots:
                root.mkdir()
                (root / "k3.exe").write_bytes(b"unused fixture")
                (root / "installation-state.json").write_text("{}")
            source = (REPO / "install.ps1").read_text(encoding="utf-8-sig")
            discovery = source[source.index("function Find-K3OnlineInstallations"):source.index("if ($Yes -and -not $InstallDir)")]
            harness = r'''
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$script:paths = PATHS
function Get-ChildItem {
    [CmdletBinding()] param([string]$LiteralPath, [string]$Filter, [switch]$File)
    if ($LiteralPath -eq 'HKCU:\Software\K3\OnlineInstallations') {
        foreach ($index in 0..($script:paths.Count - 1)) { [PSCustomObject]@{PSPath=[string]$index} }
    }
}
function Get-ItemProperty {
    [CmdletBinding()] param([string]$LiteralPath)
    [PSCustomObject]@{InstallDir=$script:paths[[int]$LiteralPath]}
}
function Get-PSDrive { param([string]$PSProvider) }
function Get-Command { [CmdletBinding()] param([string]$Name, [string]$CommandType, [switch]$All) }
DISCOVERY
ConvertTo-Json -Compress -InputObject @(Find-K3OnlineInstallations)
'''
            paths = [str(roots[0]), str(roots[0]), str(base / "missing"), str(roots[1])]
            literal = "@(" + ",".join("'" + path.replace("'", "''") + "'" for path in paths) + ")"
            harness = harness.replace("PATHS", literal).replace("DISCOVERY", discovery)
            harness_path = base / "discovery-harness.ps1"
            harness_path.write_text(harness, encoding="utf-8-sig")
            result = subprocess.run([POWERSHELL, "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", str(harness_path)],
                cwd=base, capture_output=True, text=True, encoding="utf-8", timeout=30)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout), [str(root) for root in roots])


if __name__ == "__main__":
    unittest.main()
