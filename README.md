# K3

**English** | [简体中文](README.zh-Hans.md) | [繁體中文](README.zh-Hant.md)

K3 is a local-first karaoke workspace with a Linux/Windows desktop GUI and a cross-platform
CLI/TUI. It saves song projects, imports LRC lyrics, runs a local stem-separation worker, and
plays Original, Accompaniment, and Vocals tracks. The desktop GUI also records the default
microphone, provides live monitoring, and searches for synchronized lyrics.

<a id="快速安装"></a>

## Quick installation

Use `install.sh` on Linux/macOS or `install.ps1` on Windows. You do not need to install Rust,
Python, or uv beforehand. The installer asks you to choose a disk and directory, requests
confirmation, and downloads the programs for your platform. Where supported, it also prepares
standalone Python, separation dependencies, and models, then checks startup and models.
A full installation needs **8 GiB** of free space at its peak and uses about **2–4 GiB** afterward.
The minimal CLI installation on Windows ARM64 and Intel macOS needs **512 MiB** of free space.

**Current release: [v0.1.6](https://github.com/coanor/k3/releases/tag/v0.1.6).
Online installation has been verified on all six platforms. Use the one-line command for your system below.**

System requirements: Linux needs glibc ≥ 2.39 (for example, Ubuntu 24.04); macOS needs version
14 or later; Windows x64 needs Windows 10/11; Windows ARM64 needs Windows 11.
On Ubuntu 24.04, install the runtime libraries first:

```bash
sudo apt update && sudo apt install libasound2t64 libfontconfig1 libxkbcommon-x11-0 libegl1 libgl1-mesa-dri
```

### One-line installation: v0.1.6

Copy the one-line command for your system to start installation. You do not need to sign in
to GitHub or download platform components manually. These commands select the stable
`v0.1.6` release explicitly.

Linux/macOS:

```bash
curl -fsSL https://github.com/coanor/k3/releases/download/v0.1.6/install.sh | bash -s -- --version v0.1.6
```

Windows PowerShell:

```powershell
irm -ErrorAction Stop https://github.com/coanor/k3/releases/download/v0.1.6/get.ps1 | iex
```

The script reads disk selection and confirmation from the terminal. The Windows bootstrap
verifies the installer SHA-256 and handles UTF-8 decoding; the installer continues to verify
programs, uv, support files, and models. The Bash script already enables `pipefail` internally,
but it cannot change the exit status of the outer `curl | bash` pipeline. Curl reports download
errors; for automation that must capture them, run `set -o pipefail` first. Expand the manual
steps below to inspect and verify the entry script before running it.

<details>
<summary>Optional: download the entry script, verify SHA-256, then run it</summary>

Linux/macOS:

```bash
release_url='https://github.com/coanor/k3/releases/download/v0.1.6'
curl --proto '=https' --proto-redir '=https' -fL "$release_url/install.sh" -o install.sh &&
curl --proto '=https' --proto-redir '=https' -fL "$release_url/install.sh.sha256" -o install.sh.sha256 &&
if command -v sha256sum >/dev/null; then
    sha256sum --check install.sh.sha256
else
    shasum -a 256 --check install.sh.sha256
fi &&
bash ./install.sh --version v0.1.6
```

Windows PowerShell:

```powershell
& {
    $ErrorActionPreference = 'Stop'
    $releaseUrl = 'https://github.com/coanor/k3/releases/download/v0.1.6'
    Invoke-WebRequest -UseBasicParsing "$releaseUrl/install.ps1" -OutFile .\install.ps1
    Invoke-WebRequest -UseBasicParsing "$releaseUrl/install.ps1.sha256" -OutFile .\install.ps1.sha256
    $expected = ((Get-Content .\install.ps1.sha256 -Raw).Trim() -split '\s+')[0]
    if ((Get-FileHash .\install.ps1 -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expected) {
        throw 'Installer script SHA-256 verification failed'
    }
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -Version v0.1.6
}
```

</details>

After installation it prints the launch paths.
On Linux and Windows x64, launch `k3-gui` / `k3-gui.exe`. On all platforms, use
`k3 --help` / `k3.exe --help` to list commands. The installer does not automatically change PATH.

### Platform support

| System | Build components (selected automatically) | GUI | Stem separation |
| --- | --- | --- | --- |
| Linux x64 | `online-k3-linux-x86_64` | Supported | Supported |
| Linux ARM64 | `online-k3-linux-aarch64` | Supported | Supported |
| Windows x64 | `online-k3-windows-x86_64` | Supported | Supported |
| Windows ARM64 | `online-k3-windows-aarch64-cli` | Not yet supported | Full separation environment not yet available |
| macOS Apple Silicon (M1/M2/M3/M4, etc.) | `online-k3-macos-aarch64` | Not yet supported | Supported |
| macOS Intel | `online-k3-macos-x86_64-cli` | Not yet supported | Full separation environment not yet available |

<details>
<summary>Optional: install from Actions components</summary>

1. Sign in to GitHub and open the [v0.1.6 release build](https://github.com/coanor/k3/actions/workflows/dist.yml?query=branch%3Av0.1.6).
   Under Artifacts at the bottom of the page, download **`online-support`** and the component
   group for your system from the table above. Artifacts are retained for 14 days. If they have
   expired, find a newer successful six-platform build in
   [Actions](https://github.com/coanor/k3/actions/workflows/dist.yml) and download both groups from the same run.
2. Extract the outer ZIP for `online-support` into `k3-install/support/` and the outer ZIP for
   your platform components into `k3-install/platform/`. **Keep all `.sha256` files. Do not
   manually extract the inner `k3-install-support.zip`.**
3. Open a terminal, go to the directory containing the `k3-install` folder, and run the
   installation command for your system.

Linux/macOS:

```bash
bash ./k3-install/support/install.sh --source-dir "$PWD/k3-install"
```

Windows PowerShell:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\k3-install\support\install.ps1 -SourceDir "$PWD\k3-install"
```

These commands prompt you to choose a disk/installation directory and confirm downloads.
This method installs the K3 programs from local components; Python, third-party dependencies,
and models still require an internet connection. Choose a new, empty directory: the installer
refuses to overwrite a nonempty directory.

</details>

If anonymous downloads hit GitHub API rate limits (`403` / `429`), wait for the allowance to
reset. The installer currently supports authentication through a personal `GITHUB_TOKEN` set
in the process environment; do not put the token in command arguments or public files.
See the [installation guide](docs/install-packages.md) for disk selection, directory/version
options, updating, uninstalling, application shortcuts, and optional offline packages.
Online installs include `update.sh` / `update.ps1` and `uninstall.sh` / `uninstall.ps1`;
to upgrade an older online installation that has no maintenance scripts, use the
[one-line upgrade commands](docs/user-manual.md#upgrade-an-existing-installation).
Both install and upgrade entries discover existing online installations and select a
unique match automatically. Successful installs remember their location per user;
Windows uses `HKCU\Software\K3\OnlineInstallations`. Multiple matches require a choice.
Updates verify existing model weights against the new release's SHA-256 and reuse
matching checkpoints, downloading only missing or changed weights.
GUI/TUI check for new stable releases in the background. See the [user manual](docs/user-manual.md)
for the GUI workflow, playback, and recording. The user guide is available in English,
Simplified Chinese and Traditional Chinese. The installation guide is in English with a
Simplified Chinese translation; older technical documents may still be in Simplified Chinese.

## Building from source

See the [GUI specification](docs/gui-spec.md) for desktop design decisions and the
[GUI acceptance record](docs/gui-acceptance.md) for platform and performance evidence.

### Build and test

The repository pins Rust 1.99.0 and its matching Clippy and rustfmt in `rust-toolchain.toml`.
Local development and GitHub Actions use the same version. With rustup, running these
commands in the repository automatically selects and installs the toolchain. When upgrading
the compiler, update that file and pass all checks.

```bash
rustup show active-toolchain
cargo test --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo run -p k3 -- --help
cargo run -p k3-gui
```

Linux desktop packages include both `k3` and `k3-gui`. The GUI uses Slint's femtovg/OpenGL ES
hardware renderer, with no software renderer enabled. If the desktop backend cannot start,
you can still run `k3 tui`.

### Create and open a project

```bash
cargo run -p k3 -- new \
  --root ./songs/example \
  --song /path/to/song.flac \
  --lyrics /path/to/song.lrc \
  --title "Example"

cargo run -p k3 -- show --project ./songs/example
cargo run -p k3 -- tui --project ./songs/example
cargo run -p k3-gui
```

On the GUI's first launch, select the directory containing your existing K3 projects. The GUI
and TUI share each project's `project.json`, while the GUI stores its window, volume, and
project-root settings separately.

### Local stem-separation worker

Rust calls the JSON-lines worker in `python/separator` through a small `StemSeparator`
interface. It supports quality profiles, explicit checkpoints, SHA-256 provenance, and
versioned stem outputs referenced by the `project.json` manifest. For installation and
protocol details, see the [worker README](python/separator/README.md).

Example:

```bash
cargo run -p k3 -- separate \
  --project ./songs/example \
  --profile quality \
  --model mel-band-roformer-kim-vocal-2 \
  --worker ./.venv-separator/bin/k3-separator \
  --model-dir ~/.cache/k3/models
```

## Optional offline packages

Full packages for Windows x86_64, Linux x86_64/ARM64, and macOS Apple Silicon include the
programs, standalone Python, CPU separation dependencies, FFmpeg, and default models.
After extraction, you do not need to install Python or download models. Intel macOS and
Windows ARM64 have minimal native CLI/TUI packages without a GUI or offline separation
environment. M1/M2/M3/M4 and other Apple Silicon chips use the same macOS ARM64 package,
requiring macOS 14 or later. Build offline packages with `make dist-offline` or the GitHub
Actions `offline_bundle` option. See the [offline package guide](docs/offline-package.md) for details.

## License

K3's own code is licensed under the [MIT License](LICENSE). You may use, modify, and
distribute it while retaining the copyright and license notices. The software is provided
as is, without warranty. Dependencies, fonts, and models have their own licenses; the
third-party notices in this repository still apply. See the
[Slint license notice](crates/k3-gui/assets/licenses/LicenseRef-Slint-Royalty-free-2.0.md) for Slint's original terms.
