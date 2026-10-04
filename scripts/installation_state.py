"""Track installed files and replace a verified online installation transactionally."""

from __future__ import annotations

from contextlib import contextmanager
import hashlib
import json
import os
import re
from pathlib import Path, PurePosixPath
import shutil
import tempfile

STATE = "installation-state.json"


def directory_link(path: Path) -> bool:
    return path.is_symlink() or path.is_junction()


def signature(path: Path) -> dict:
    if path.is_symlink():
        return {"link": os.readlink(path)}
    with path.open("rb") as stream:
        return {"sha256": hashlib.file_digest(stream, "sha256").hexdigest()}


def write_state(root: Path, repo: str, version: str) -> None:
    files = {path.relative_to(root).as_posix(): signature(path)
             for path in sorted(root.rglob("*"))
             if (path.is_file() or path.is_symlink()) and path != root / STATE}
    (root / STATE).write_text(json.dumps({"format": 1, "repo": repo, "version": version,
                                        "files": files}, indent=2), encoding="utf-8")


def load_state(root: Path) -> dict:
    if directory_link(root) or not root.is_dir():
        raise ValueError("Installation must be a real directory, not a symbolic link")
    if (root / STATE).is_symlink():
        raise ValueError("Installation state cannot be a symbolic link")
    state = json.loads((root / STATE).read_text(encoding="utf-8"))
    if (state.get("format") != 1 or not isinstance(state.get("files"), dict)
            or not isinstance(state.get("version"), str)
            or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", state["version"])
            or not isinstance(state.get("repo"), str)
            or not re.fullmatch(r"[A-Za-z0-9_-]+/[A-Za-z0-9_.-]+", state["repo"])):
        raise ValueError("Unsupported installation state; reinstall in a new directory")
    for name, expected in state["files"].items():
        relative = PurePosixPath(name)
        if (relative.as_posix() != name or not relative.parts or relative.is_absolute() or ".." in relative.parts
                or "\\" in name or ":" in name or name == STATE
                or any(ord(c) < 32 for c in name)
                or not isinstance(expected, dict)
                or set(expected) not in ({"sha256"}, {"link"})):
            raise ValueError("Installation state contains unsafe file metadata")
        if ("sha256" in expected and (not isinstance(expected["sha256"], str)
                                      or not re.fullmatch(r"[0-9a-f]{64}", expected["sha256"]))) or ("link" in expected and not isinstance(expected["link"], str)):
            raise ValueError("Installation state contains an invalid file signature")
        safe_path(root, name)
    return state


def safe_path(root: Path, name: str) -> Path:
    path = root / name
    for parent in path.parents:
        if parent == root:
            break
        if directory_link(parent) or (parent.exists() and not parent.is_dir()):
            raise ValueError(f"Managed path has a symbolic link or file as a parent: {name}")
    return path


@contextmanager
def installation_lock(root: Path):
    """Use a sibling lock so replacing files cannot release an active operation's lock."""
    lock = root.parent / ("." + root.name + ".k3-maintenance-lock")
    try:
        lock.mkdir()
    except FileExistsError as error:
        raise RuntimeError(f"Another maintenance operation is active; check {lock} before removing a stale lock") from error
    try:
        yield
    finally:
        lock.rmdir()


def remove_empty_directories(root: Path) -> None:
    for directory in sorted(root.rglob("*"), key=lambda p: len(p.parts), reverse=True):
        if directory.is_dir() and not directory_link(directory):
            try:
                directory.rmdir()
            except OSError:
                pass


def replace_installation(root: Path, staged: Path) -> None:
    """Replace owned files, preserve additions, and roll back any failed move."""
    old, new = load_state(root), load_state(staged)
    names = set(old["files"]) | set(new["files"]) | {STATE}
    # Detect edited managed files and new-file collisions before touching the installation.
    for name in sorted(names - {STATE}):
        target = safe_path(root, name)
        exists = target.exists() or target.is_symlink()
        if exists and (name not in old["files"] or target.is_dir() and not target.is_symlink()
                       or signature(target) != old["files"][name]):
            raise ValueError(f"Update would overwrite a changed or unregistered file: {target}")
    with tempfile.TemporaryDirectory(prefix=".k3-rollback-", dir=root.parent) as directory:
        backup = Path(directory)
        moved_old, moved_new = [], []
        try:
            for name in sorted(names, key=lambda n: (n == STATE, n)):
                target = safe_path(root, name)
                if target.exists() or target.is_symlink():
                    saved = backup / name
                    saved.parent.mkdir(parents=True, exist_ok=True)
                    target.rename(saved)
                    moved_old.append(name)
                incoming = staged / name
                if incoming.exists() or incoming.is_symlink():
                    target.parent.mkdir(parents=True, exist_ok=True)
                    incoming.rename(target)
                    moved_new.append(name)
        except BaseException:
            # Preserve the backup if rollback itself fails; it contains the original files.
            try:
                for name in reversed(moved_new):
                    target = root / name
                    incoming = staged / name
                    incoming.parent.mkdir(parents=True, exist_ok=True)
                    target.rename(incoming)
                for name in reversed(moved_old):
                    target = root / name
                    target.parent.mkdir(parents=True, exist_ok=True)
                    (backup / name).rename(target)
            except OSError as rollback_error:
                recovery = root.parent / (root.name + ".k3-recovery-" + backup.name)
                backup.rename(recovery)
                raise RuntimeError(f"Rollback failed; original files are preserved in {recovery}") from rollback_error
            raise
    remove_empty_directories(root)


def uninstall(root: Path) -> None:
    """Delete unchanged owned files, preserving edited files and everything else."""
    with installation_lock(root):
        state = load_state(root)
        ensure_installation_idle(root)
        removable = []
        for name, expected in state["files"].items():
            path = safe_path(root, name)
            if not path.exists() and not path.is_symlink():
                continue
            if path.is_dir() and not path.is_symlink() or signature(path) != expected:
                print(f"Keeping changed installation file: {path}", flush=True)
            else:
                removable.append(path)
        for shortcut in shortcut_paths(root):
            shortcut.unlink()
        for path in removable:
            path.unlink()
        (root / STATE).unlink()
        remove_empty_directories(root)
    try:
        root.rmdir()
    except OSError:
        print(f"Kept remaining user files in: {root}", flush=True)


def shortcut_paths(root: Path) -> list[Path]:
    path = root / "shortcuts.json"
    if not path.is_file() or path.is_symlink():
        return []
    # Restrict deletion to known per-user shortcut locations and installation-specific names.
    identity = hashlib.sha256(str(root.resolve()).encode()).hexdigest()[:12]
    allowed = {f"k3-{identity}.desktop", f"K3-{identity}.desktop", f"K3-{identity}.lnk"}
    home = Path.home().resolve()
    removable = []
    for entry in json.loads(path.read_text(encoding="utf-8")):
        target = Path(entry["path"])
        if (target.name in allowed and target.is_absolute() and not target.is_symlink()
                and target.resolve().is_relative_to(home) and target.is_file()
                and signature(target) == {"sha256": entry["sha256"]}):
            removable.append(target)
    return removable


def uninstall_plan(root: Path) -> dict:
    """Plan Windows deletion using Python, then execute after Python has exited."""
    state = load_state(root)
    ensure_installation_idle(root)
    files, kept = [], []
    for name, expected in state["files"].items():
        path = safe_path(root, name)
        if not path.exists() and not path.is_symlink():
            continue
        if (path.is_dir() and not path.is_symlink()) or signature(path) != expected:
            kept.append(str(path))
        else:
            files.append(str(path))
    files += [str(path) for path in shortcut_paths(root)]
    directories = [str(path) for path in root.rglob("*") if path.is_dir() and not directory_link(path)]
    return {"files": files, "kept": kept, "directories": sorted(directories, key=len, reverse=True)}


def migrate_legacy_installation(root: Path, staged: Path) -> Path:
    """Upgrade an old online installation while retaining its complete original directory."""
    new = load_state(staged)
    # Carry personal files and additional models forward without adopting them as owned.
    # Legacy runtime additions remain in the complete backup to avoid mixing dependencies.
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root)
        if relative.parts[0] == "runtime" or relative.as_posix() in new["files"]:
            continue
        target = staged / relative
        if target.exists() or target.is_symlink():
            continue
        safe_path(staged, relative.as_posix())
        if path.is_symlink():
            target.parent.mkdir(parents=True, exist_ok=True)
            target.symlink_to(os.readlink(path), target_is_directory=path.is_dir())
        elif path.is_dir():
            target.mkdir(parents=True, exist_ok=True)
        elif path.is_file():
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, target)
    backup = Path(tempfile.mkdtemp(prefix=".k3-previous-", dir=root.parent))
    backup.rmdir()
    root.rename(backup)
    try:
        staged.rename(root)
    except BaseException:
        backup.rename(root)
        raise
    return backup


def ensure_installation_idle(root: Path) -> None:
    """Reject maintenance while a process is executing programs from this installation."""
    import platform
    import subprocess
    system = platform.system()
    root = root.resolve()
    active = []
    if system == 'Linux':
        for entry in Path('/proc').iterdir():
            if not entry.name.isdigit() or int(entry.name) == os.getpid():
                continue
            try:
                executable = (entry / 'exe').resolve(strict=True)
                if executable.is_relative_to(root):
                    active.append(entry.name)
            except (OSError, RuntimeError):
                continue
    elif system == 'Windows':
        script = ("Get-CimInstance Win32_Process | Where-Object { $_.ProcessId -ne "
                  "$env:K3_MAINTENANCE_PID -and $_.ExecutablePath -and "
                  "$_.ExecutablePath.StartsWith($env:K3_MAINTENANCE_ROOT + '\\', "
                  "[StringComparison]::OrdinalIgnoreCase) } | ForEach-Object { $_.ProcessId }")
        result = subprocess.run(['powershell.exe', '-NoProfile', '-NonInteractive', '-Command', script],
                                check=True, capture_output=True, text=True, timeout=30,
                                env=dict(os.environ, K3_MAINTENANCE_ROOT=str(root), K3_MAINTENANCE_PID=str(os.getpid())))
        active = result.stdout.split()
    elif system == 'Darwin':
        programs = [root / name for name in ('k3', 'k3-separator', 'runtime/python/bin/python3',
                                             'runtime/python/bin/python3.13', 'runtime/bin/ffmpeg')]
        result = subprocess.run(['lsof', '-t', '--', *map(str, filter(Path.exists, programs))],
                                capture_output=True, text=True, timeout=30)
        if result.returncode not in (0, 1):
            raise RuntimeError('Could not check running K3 processes')
        active = [pid for pid in result.stdout.split() if pid != str(os.getpid())]
    if active:
        raise RuntimeError('Close K3 windows, TUI sessions and separation jobs before maintenance; active process IDs: ' + ', '.join(active))


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser(description="Read a registered online installation's release repository")
    parser.add_argument("--prefix", type=Path, required=True)
    args = parser.parse_args()
    root = args.prefix
    metadata = root / STATE if (root / STATE).exists() else root / "install-manifest.json"
    info = json.loads(metadata.read_text(encoding="utf-8"))
    print(info.get("repo", "coanor/k3"))
