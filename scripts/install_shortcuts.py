"""Create per-user application and optional desktop shortcuts for online installs."""

from __future__ import annotations

import hashlib
import os
from pathlib import Path
import subprocess


def desktop_exec(path: Path) -> str:
    """Quote an executable according to the Desktop Entry Exec specification."""
    value = str(path).replace("%", "%%")
    for character in ('\\', '"', '`', '$'):
        value = value.replace(character, '\\' + character)
    return '"' + value.replace('\\', '\\\\') + '"'


def create_shortcuts(prefix: Path, system: str, desktop: bool) -> list[dict]:
    """Return paths and digests of created shortcuts; preserve existing files."""
    identity = hashlib.sha256(str(prefix.resolve()).encode()).hexdigest()[:12]
    created = []
    if system == "linux":
        applications = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local/share")) / "applications"
        contents = ("[Desktop Entry]\nType=Application\nName=K3\n"
                    "Comment=Karaoke player and recorder\n"
                    f"Exec={desktop_exec(prefix / 'k3-gui')}\n"
                    f"Icon={str(prefix / 'k3.svg').replace(chr(92), chr(92) * 2)}\n"
                    "Terminal=false\nCategories=AudioVideo;Audio;\n")
        targets = [applications / f"k3-{identity}.desktop"]
        if desktop:
            directory = Path.home() / "Desktop"
            try:
                result = subprocess.run(["xdg-user-dir", "DESKTOP"], check=True,
                                        capture_output=True, text=True, timeout=10)
                directory = Path(result.stdout.strip())
            except (OSError, subprocess.SubprocessError):
                pass
            if directory.is_absolute() and directory.is_dir() and directory.resolve() != Path.home().resolve():
                targets.append(directory / f"K3-{identity}.desktop")
            else:
                print("Desktop directory is unavailable; the application menu shortcut was created.", flush=True)
        for target in targets:
            target.parent.mkdir(parents=True, exist_ok=True)
            try:
                with target.open("x", encoding="utf-8") as stream:
                    stream.write(contents)
                target.chmod(0o755)
                created.append(target)
            except FileExistsError:
                print(f"Keeping existing shortcut: {target}", flush=True)
    elif system == "windows":
        environment = dict(os.environ, K3_SHORTCUT_ROOT=str(prefix),
                           K3_SHORTCUT_ID=identity, K3_SHORTCUT_DESKTOP=str(int(desktop)))
        script = Path(__file__).with_name("install-shortcuts.ps1")
        result = subprocess.run(["powershell.exe", "-NoProfile", "-NonInteractive",
                                 "-ExecutionPolicy", "Bypass", "-File", str(script)],
                                check=True, capture_output=True, text=True, encoding="utf-8",
                                timeout=30, env=environment)
        created = [Path(line) for line in result.stdout.splitlines() if line]
    return [{"path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
            for path in created]
