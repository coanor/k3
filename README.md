# K3

K3 是一个本地优先的 K 歌工作区，目前同时提供 Linux 桌面 GUI、CLI 和 TUI。它可以
保存歌曲工程、导入 LRC 歌词、调用本地分轨 worker，播放 Original、Accompaniment
与 Vocals 音轨，并在桌面 GUI 中录制默认麦克风、进行实时监听和搜索同步歌词。

安装步骤和完整工作流见[中文用户手册](docs/user-manual.md)。首期桌面界面的范围与
技术决策见 [GUI 规格](docs/gui-spec.md)，平台与性能证据见
[GUI 验收记录](docs/gui-acceptance.md)。

## 构建与测试

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

## 创建并打开工程

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

## 本地分轨 worker

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

## 离线发行包

Windows x86_64、Linux x86_64/ARM64 与 macOS Apple Silicon 的完整包包含程序、独立
Python、CPU 分离依赖、FFmpeg 和默认模型；解压后无需安装 Python 或在线下载模型。
Intel macOS 提供纯 CLI 包。构建入口为 `make dist` 和 GitHub Actions，具体用法见
[离线发行包说明](docs/offline-package.md)。
