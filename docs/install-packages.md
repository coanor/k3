# Install K3

Use the release `install.sh` on Linux/macOS or `install.ps1` on Windows. The online installer downloads native programs, prepares standalone Python, CPU separation dependencies and FFmpeg, and verifies the Fast, Balanced and Quality models. Rust, Python and uv do not need to be installed beforehand.

[简体中文](install-packages.zh-Hans.md)

| Platform | Programs and separation support |
| --- | --- |
| Linux x86_64 / ARM64 | GUI, CLI/TUI and CPU runtime; glibc 2.39 or later |
| Windows x64 | GUI, CLI/TUI and CPU runtime; Windows 10/11 |
| Windows ARM64 | Native CLI/TUI only; Windows 11 |
| macOS Apple Silicon | CLI/TUI and CPU runtime; macOS 14 or later |
| macOS Intel | Native CLI/TUI only; macOS 14 or later |

## Online installation

Download the installation entry and its SHA-256 file from the same release. Verify the checksum before running the entry:

```bash
# Linux; use shasum -a 256 on macOS
sha256sum --check install.sh.sha256
bash install.sh --prefix /mnt/data/K3
```

```powershell
# Compare this digest with install.ps1.sha256
Get-FileHash .\install.ps1 -Algorithm SHA256
powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -InstallDir 'D:\Apps\K3'
```

The installer asks for a disk/directory and confirmation. All downloads, models, temporary files and programs use the selected disk. Full installs reserve 8 GiB of peak disk space; CLI-only installs reserve 512 MiB. Existing nonempty directories are rejected. Checks include release version, file sizes, SHA-256, program startup and offline model loading. Failed verification does not enable the destination.

Use `--repo owner/repo` / `-Repo owner/repo` to select a release repository, and `--version vX.Y.Z` / `-Version vX.Y.Z` to pin a release. The default repository is `coanor/k3`; the default version is the latest stable release. Public downloads use release URLs. Private releases may use `GITHUB_TOKEN`; credentials are restricted to the GitHub API and removed on redirects.

### Shortcuts

Windows x64 online installs create a per-user Start menu shortcut. Linux online installs create a user application menu entry. Interactive installation asks **Create a desktop shortcut? [y/N]**, defaulting to no. Automated installation uses `--yes` / `-Yes` with an explicit installation directory; request a desktop icon with `--desktop-shortcut` / `-DesktopShortcut`. Use `--no-desktop-shortcut` / `-NoDesktopShortcut` to skip the question during interactive installation.

Shortcuts point to the selected installation, use its K3 icon, and preserve existing entries. Linux uses the desktop directory reported by `xdg-user-dir`, with an existing `~/Desktop` as fallback; desktop environments may require marking the launcher as trusted. macOS and Windows ARM64 currently provide CLI/TUI only, so no GUI shortcut is created.

Run `k3` / `k3.exe` or `k3-gui` / `k3-gui.exe` from the selected directory. The installer does not modify global PATH. Keep projects, recordings and your music library in a personal data directory.

Linux requires system audio/graphics libraries. On Ubuntu 24.04:

```bash
sudo apt install libasound2t64 libfontconfig1 libxkbcommon-x11-0 libegl1 libgl1-mesa-dri
```

### Local release verification

```bash
make dist
bash install.sh --source-dir "$PWD/dist/online" --prefix /mnt/data/K3-check --yes
```

Windows uses `-SourceDir` with the same layout. `--model-cache` / `-ModelCache` reuses downloaded models with integrity checks. These options replace only K3 release assets and model downloads; uv, Python and other dependencies still use their official sources.

## Optional offline system packages

Download the matching package and checksum from the same release:

| Platform | Package | Installation directory |
| --- | --- | --- |
| Windows x64 | `k3-VERSION-windows-x86_64-setup.exe` | `%LOCALAPPDATA%\Programs\K3` |
| Windows ARM64 | `k3-VERSION-windows-aarch64-cli-setup.exe` | `%LOCALAPPDATA%\Programs\K3-ARM64-CLI` |
| Linux x86_64 / ARM64 | `k3_VERSION_amd64.deb` / `k3_VERSION_arm64.deb` | `/opt/k3` |
| macOS Apple Silicon / Intel | `k3-VERSION-macos-aarch64.pkg` / `k3-VERSION-macos-x86_64-cli.pkg` | `/usr/local/lib/k3` |

The Windows wizard is in English. Full packages include Start menu and uninstall entries and offer an optional desktop shortcut. Windows ARM64 contains CLI/TUI and an uninstall entry. Use Windows Settings or the Start menu to uninstall; extra user files are retained. Windows packages currently have no code signature.

On Linux, verify with `sha256sum --check PACKAGE.deb.sha256`, then run `sudo apt install ./PACKAGE.deb`. Commands are linked from `/usr/bin`; the application menu includes K3. Uninstall with `sudo apt remove k3`. The package manager retains personal data.

On macOS, double-click the matching `.pkg` or run `sudo installer -pkg PACKAGE.pkg -target /`. Commands in `/usr/local/bin` launch the real bundled executables. Packages are currently unsigned and not notarized. There is no automatic `.pkg` uninstaller: inspect `pkgutil --files io.github.coanor.k3`, remove only registered K3 files and launchers, then run `sudo pkgutil --forget io.github.coanor.k3`. Forgetting a receipt alone does not remove files.

System installs keep models in a directory owned by the system. To download additional models, use a writable personal directory with `--model-dir` and copy default models there as needed. Windows ARM64 and Intel macOS omit the separation runtime; Windows ARM64 is limited by availability of native Python/PyTorch dependencies.

## Build system installers

Build and verify the matching portable bundle on its native platform, then run:

```bash
python3 scripts/build-installer.py dist/k3-linux-x86_64.tar.gz
```

Linux requires `dpkg-deb`; macOS uses `pkgbuild`/`productbuild`; Windows requires Inno Setup 6.5 or later (`--iscc` accepts its executable path). The builder validates checksums, architecture, layout and version. `--refresh-worker` updates the bundled worker from current sources; `--refresh-portable-manuals` refreshes user manuals in the input portable archive as well.
