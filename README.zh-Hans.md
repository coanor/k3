# K3

[English](README.md) | **简体中文** | [繁體中文](README.zh-Hant.md)

K3 是一个本地优先的 K 歌工作区，提供 Linux/Windows 桌面 GUI 和跨平台 CLI/TUI。它可以
保存歌曲工程、导入 LRC 歌词、调用本地分轨 worker，播放 Original、Accompaniment
与 Vocals 音轨，并在桌面 GUI 中录制默认麦克风、进行实时监听和搜索同步歌词。

## 快速安装

使用 `install.sh`（Linux/macOS）或 `install.ps1`（Windows）安装，无需预先安装 Rust、
Python 或 uv。安装器会让你选择磁盘和目录，确认后下载当前平台的程序，并在平台支持时
准备独立 Python、分离依赖及模型，最后执行启动和模型检查。完整安装需预留 **8 GiB**
峰值空间，安装后约占 **2–4 GiB**；
Windows ARM64 和 Intel macOS 的精简 CLI 安装需预留 **512 MiB**。

**当前正式版本：[v0.1.1](https://github.com/coanor/k3/releases/tag/v0.1.1)。
六平台在线安装已通过验证；请选择下面对应系统的一行命令。**

系统要求：Linux 需 glibc ≥ 2.39（如 Ubuntu 24.04），macOS 需 14 或更高，Windows x64
需 Windows 10/11，Windows ARM64 需 Windows 11。Ubuntu 24.04 安装前准备运行库：

```bash
sudo apt update && sudo apt install libasound2t64 libfontconfig1 libxkbcommon-x11-0 libegl1 libgl1-mesa-dri
```

### 一行安装：v0.1.1

复制对应系统的一行命令即可开始安装，无需登录 GitHub 或手动下载平台组件。
下面的命令明确选择正式版 `v0.1.1`。

Linux/macOS：

```bash
curl -fsSL https://github.com/coanor/k3/releases/download/v0.1.1/install.sh | bash -s -- --version v0.1.1
```

Windows PowerShell：

```powershell
irm -ErrorAction Stop https://github.com/coanor/k3/releases/download/v0.1.1/get.ps1 | iex
```

脚本从终端读取选盘和确认输入。Windows 启动脚本自动校验安装入口的 SHA-256 并处理 UTF-8 编码；
安装器继续校验程序、uv、支持文件与模型。Bash 脚本内部已有 `pipefail`，但它无法改变外层
`curl | bash` 的退出状态：下载失败时 curl 会报错；若需自动化捕获该失败，请在运行前执行
`set -o pipefail`。手动查看与校验入口脚本的步骤可在下面展开。

<details>
<summary>可选：下载入口脚本，核对 SHA-256 后运行</summary>

Linux/macOS：

```bash
release_url='https://github.com/coanor/k3/releases/download/v0.1.1'
curl --proto '=https' --proto-redir '=https' -fL "$release_url/install.sh" -o install.sh &&
curl --proto '=https' --proto-redir '=https' -fL "$release_url/install.sh.sha256" -o install.sh.sha256 &&
if command -v sha256sum >/dev/null; then
    sha256sum --check install.sh.sha256
else
    shasum -a 256 --check install.sh.sha256
fi &&
bash ./install.sh --version v0.1.1
```

Windows PowerShell：

```powershell
& {
    $ErrorActionPreference = 'Stop'
    $releaseUrl = 'https://github.com/coanor/k3/releases/download/v0.1.1'
    Invoke-WebRequest -UseBasicParsing "$releaseUrl/install.ps1" -OutFile .\install.ps1
    Invoke-WebRequest -UseBasicParsing "$releaseUrl/install.ps1.sha256" -OutFile .\install.ps1.sha256
    $expected = ((Get-Content .\install.ps1.sha256 -Raw).Trim() -split '\s+')[0]
    if ((Get-FileHash .\install.ps1 -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expected) {
        throw 'Installer script SHA-256 verification failed'
    }
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -Version v0.1.1
}
```

</details>

安装器完成后会输出启动路径；Linux 和 Windows x64 可启动 `k3-gui` / `k3-gui.exe`，
所有平台均可用 `k3 --help` / `k3.exe --help` 查看命令。安装器不自动修改系统 PATH。

### 平台支持

| 系统 | 构建组件（自动选择） | GUI | 音轨分离 |
| --- | --- | --- | --- |
| Linux x64 | `online-k3-linux-x86_64` | 支持 | 支持 |
| Linux ARM64 | `online-k3-linux-aarch64` | 支持 | 支持 |
| Windows x64 | `online-k3-windows-x86_64` | 支持 | 支持 |
| Windows ARM64 | `online-k3-windows-aarch64-cli` | 暂不支持 | 暂不提供完整分离环境 |
| macOS Apple Silicon（M1/M2/M3/M4 等） | `online-k3-macos-aarch64` | 暂不支持 | 支持 |
| macOS Intel | `online-k3-macos-x86_64-cli` | 暂不支持 | 暂不提供完整分离环境 |

<details>
<summary>可选：从 Actions 组件安装</summary>

1. 登录 GitHub，打开[已验证的六平台构建](https://github.com/coanor/k3/actions/runs/37147301570)，
   在页面底部 Artifacts 下载 **`online-support`** 和上表中与你的系统对应的一组组件。
   构建产物保留 14 天；若已过期，请从 [Actions](https://github.com/coanor/k3/actions/workflows/dist.yml)
   选择较新的成功六平台构建，并从同一次构建下载两组文件。
2. 将 `online-support` 的外层 ZIP 解压到 `k3-install/support/`，平台组件的外层 ZIP 解压到
   `k3-install/platform/`。**保留全部 `.sha256` 文件；内部的 `k3-install-support.zip` 无需手动解压。**
3. 打开终端，进入包含 `k3-install` 文件夹的目录，运行对应安装命令。

Linux/macOS：

```bash
bash ./k3-install/support/install.sh --source-dir "$PWD/k3-install"
```

Windows PowerShell：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\k3-install\support\install.ps1 -SourceDir "$PWD\k3-install"
```

这两条命令会提示选择磁盘/安装目录和确认下载。这种方式从本地组件安装 K3 程序，
Python、第三方依赖和模型仍需联网下载。选择新的空目录，安装器会拒绝覆盖非空目录。

</details>

如果匿名下载遇到 GitHub API `403` / `429` 限流，请等待额度恢复；目前安装器
支持在进程环境中设置个人 `GITHUB_TOKEN` 进行认证，勿将令牌写入命令参数或公开文件。
磁盘选择、指定目录/版本、卸载及可选离线包见[安装说明](docs/install-packages.md)，
GUI 加歌、播放和录音的操作见[简体中文用户指南](docs/user-manual.zh-Hans.md)。

## 从源码构建

桌面界面的技术决策见 [GUI 规格](docs/gui-spec.md)，平台与性能证据见
[GUI 验收记录](docs/gui-acceptance.md)。

### 构建与测试

仓库通过 `rust-toolchain.toml` 固定 Rust 1.99.0，以及对应的 Clippy 和 rustfmt；
本地与 GitHub Actions 使用同一版本。使用 rustup 时，在仓库中运行下列命令会自动
选择并安装该工具链。升级编译器时应同时更新此文件并通过全部门禁。

```bash
rustup show active-toolchain
cargo test --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo run -p k3 -- --help
cargo run -p k3-gui
```

Linux 桌面发行包同时包含 `k3` 和 `k3-gui`。`k3-gui` 使用 Slint 的
femtovg/OpenGL ES 硬件 renderer，不启用软件 renderer；无法启动桌面后端时仍可运行
`k3 tui`。

### 创建并打开工程

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

首次启动 GUI 时只需选择保存现有 K3 工程的目录。GUI 与 TUI 共享各工程中的
`project.json`，但 GUI 的窗口、音量和工程根目录设置单独保存。

### 本地分轨 worker

Rust 通过小型 `StemSeparator` interface 调用 `python/separator` 下的 JSON-lines
worker。它支持质量 profile、明确的 checkpoint、SHA-256 provenance，以及由
`project.json` manifest 引用的版本化 stem 输出。安装与协议说明见
[worker README](python/separator/README.md)。

示例：

```bash
cargo run -p k3 -- separate \
  --project ./songs/example \
  --profile quality \
  --model mel-band-roformer-kim-vocal-2 \
  --worker ./.venv-separator/bin/k3-separator \
  --model-dir ~/.cache/k3/models
```

## 可选的离线发行包

Windows x86_64、Linux x86_64/ARM64 与 macOS Apple Silicon 的完整包包含程序、独立
Python、CPU 分离依赖、FFmpeg 和默认模型；解压后无需安装 Python 或在线下载模型。
Intel macOS 与 Windows ARM64 提供原生 CLI/TUI 精简包，不包含 GUI 和离线分离环境。
M1/M2/M3/M4 等 Apple Silicon 芯片共用 macOS ARM64 包，要求 macOS 14 或更高。离线构建入口为 `make dist-offline` 和 GitHub Actions 的 `offline_bundle`，具体用法见
[离线发行包说明](docs/offline-package.md)。

## 许可证

K3 自有代码使用 [MIT 许可证](LICENSE)，允许使用、修改和分发，但需保留版权与许可声明；
软件按现状提供，不附带担保。依赖库、字体和模型分别遵循各自的许可证；仓库内的第三方
许可声明仍然适用，其中 Slint 的许可原文见 [Slint 许可声明](crates/k3-gui/assets/licenses/LicenseRef-Slint-Royalty-free-2.0.md)。
