# 离线发行包

Windows x86_64、Linux x86_64 与 macOS Apple Silicon 的完整发行包包含：

- K3 CLI/TUI 程序和原生 `k3-separator` 启动器；
- 独立 CPython 3.13.15，无需系统 Python、Rust 或 uv；
- PyTorch 2.11 CPU runtime、ONNX Runtime、audio-separator 和全部运行依赖；
- FFmpeg、默认 Fast / Balanced / Quality 模型及模型配置和元数据；
- SHA-256 校验文件、依赖版本记录和第三方组件清单。

包内默认模型为 `uvr-mdx-karaoke-2`、`uvr-mdx-inst-hq-3` 和
`bs-roformer-viperx-1297`。Fast 模型同时用于保留和声，三个默认档位都可离线使用。
可选的 MelBand 和 HTDemucs 模型仍需在有网络时下载，或者在构建时通过
`--model` 显式加入；它们不在默认包中。

Intel macOS 包以 `-cli` 标注，只含 K3 程序与文档。官方 PyTorch 2.2 是最后支持
Intel macOS 的版本，无法使用本项目的 PyTorch 2.11 runtime。
参见 [PyTorch 官方说明](https://pytorch.org/blog/pytorch2-2/)。

## 解压与运行

下载与你的系统和 CPU 架构相符的压缩包及 `.sha256` 文件，校验后完整解压到可写目录。
保留 `k3`、`k3-separator`、`runtime/` 与 `models/` 的相对位置；移动或改名整个目录
不会破坏环境。不要只复制单个程序。

Linux 完整包在 Ubuntu 22.04 runner 构建，要求 glibc 2.35 或更高及 ALSA 运行库
（Ubuntu/Debian 的 `libasound2`，新版本为 `libasound2t64`）；播放和录音还需要
系统音频服务及可用设备。Windows 完整包面向 Windows 10/11 x86_64。
macOS 完整包面向 macOS 14 或更高的 Apple Silicon 机器。

Linux/macOS 在解压目录运行：

```bash
./k3 --help
printf '%s\n' '{"id":"health","method":"health"}' | ./k3-separator
./k3 new --root ./songs/example --song /path/to/song.flac --title "示例歌曲"
./k3 separate --project ./songs/example --profile quality
./k3 tui --project ./songs/example
```

Windows 在 PowerShell 中运行：

```powershell
.\k3.exe --help
'{"id":"health","method":"health"}' | .\k3-separator.exe
.\k3.exe new --root .\songs\example --song 'D:\Music\song.flac' --title '示例歌曲'
.\k3.exe separate --project .\songs\example --profile quality
.\k3.exe tui --project .\songs\example
```

`k3 separate` 默认找到同目录的 worker；worker 默认使用同包的 `models/`，不需要
修改 PATH 或激活虚拟环境。自定义 `--worker` 与 `--model-dir` 仍然有效。
媒体库配置中的 `separation.worker` 可写 `k3-separator`，`model_dir` 可写 `null`。

完整包默认使用 CPU，无需 NVIDIA 驱动。需要 CUDA 加速时，使用源码安装脚本创建
独立 GPU 环境，再通过 `--worker` 指向该环境；不要覆盖随包 Python 的依赖。
Windows 随包保留 `install-separator.ps1` 和源码，可用于这种额外安装。

## 构建与验证

构建机器需要 Rust、Python 3.11 或更高、uv 和网络；Linux 还需要 ALSA 开发库。
完整 runtime 必须在目标 OS 和架构上构建，Python wheel 不能跨平台复用。

```bash
cargo build --release --locked --bins
python3 scripts/build-runtime.py --output dist/runtime
python3 scripts/package-dist.py linux k3-linux-x86_64 target/release/k3 dist/runtime
python3 scripts/check-dist.py dist/k3-linux-x86_64.tar.gz
```

macOS Apple Silicon 将打包参数换为 `macos k3-macos-aarch64`。
Windows 将参数换为 `windows k3-windows-x86_64 target/release/k3.exe`，使用
`python` 运行这些脚本，产物为 ZIP。更方便的入口是 `make dist` 或 GitHub Actions。

`--model-cache /path/to/models` 可以复用已有下载；固定摘要的 checkpoint 仍会校验。
`--model mel-band-roformer-kim-vocal-2` 可加入额外模型，`--model all` 可加入全部
内置模型。模型文件较大，打包器会拒绝超过 GitHub Release 单个 asset 的 2 GiB 上限。

校验脚本实际解压、移动到含中文和空格的目录，然后启动程序、验证每个模型文件摘要、
在禁止网络访问的条件下加载全部随包模型，并通过 CLI 分离真实短音频，检查主唱、
伴奏和和声输出。CI 在各目标平台执行相同检查，通过后才上传包。

`bundle-manifest.json` 记录随包模型的来源、声明的许可证、SHA-256 和 Python 组件
版本；各依赖的许可证文件保留在其 `.dist-info` 等安装目录中。
`requirements-resolved.txt` 记录该次构建的实际依赖版本。UV 安装的 Python 来自
[python-build-standalone](https://docs.astral.sh/uv/concepts/python-versions/)。
部分 UVR 模型的许可证尚未声明，清单保留 `NOASSERTION`，不将其写成 MIT。

当前没有图形安装向导、macOS 签名或公证；这些包是可解压运行的便携包。
