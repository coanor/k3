# K3 用户手册

K3 是一个本地优先的终端 K 歌工作区。目前可创建歌曲工程、导入 LRC
歌词、使用本地 GPU 模型分离人声和伴奏，并在 TUI 中播放和切换音轨。

本文以 Ubuntu 24.04、Windows WSL2 和 Windows 11 原生环境为当前支持环境。
Windows 原生版本支持播放、录音和 GPU/CPU 音轨分离。GitHub Actions 会生成 macOS
Intel 与 Apple Silicon 二进制包，但 macOS 的音频设备和模型安装尚未完成实机验证。

## 1. 系统要求

基础程序需要：

- Rust stable；
- ALSA 开发库（Ubuntu/WSL 使用 `sudo apt install libasound2-dev`）；
- Python 3.10 或更高版本；
- `uv`；
- 足够存放模型和 WAV stem 的磁盘空间。

Windows 原生分离需要 64 位 Python 3.11、PowerShell 5.1 或更高版本以及网络。
GPU 模式还需要 NVIDIA 显卡与支持 CUDA 12.8 的驱动；安装脚本会在发行目录创建
独立的 `.venv-separator`，无需预先安装 Rust、`uv` 或系统 FFmpeg。

使用 NVIDIA GPU 分离还需要可被 WSL/Linux 识别的 NVIDIA 驱动。RTX
5070 Ti 16GB 已完成实测，可同时运行最多 4 个 Quality worker；普通单曲
分离只需运行一个 worker。

检查 GPU：

```bash
nvidia-smi
```

## 2. 编译 K3

在仓库根目录运行：

```bash
cargo build --release
```

生成的程序位于：

```text
target/release/k3
```

开发时也可以把后文的 `k3` 替换为：

```bash
cargo run -p k3 --
```

查看命令：

```bash
target/release/k3 --help
```

### 2.1 Makefile

仓库根目录提供统一的常用构建入口。Ubuntu/WSL 若尚未安装 GNU Make，先运行
`sudo apt install make`：

```bash
make help
make check
make build
make dist
```

`make dist` 根据当前主机生成 Linux x86_64 或对应架构的 macOS 包。也可明确指定：

```bash
make dist-linux
make dist-windows
make dist-macos
```

`dist-windows` 在 Linux/WSL 中使用 `cargo-xwin`；缺少工具时先运行
`cargo install cargo-xwin`。如果 `llvm-lib` 不在 `PATH`，通过
`make dist-windows LLVM_BIN=/path/to/llvm/bin` 指定。macOS 目标必须在 macOS
主机或 GitHub macOS runner 上运行，因为构建需要 Apple SDK。

要从本机触发 GitHub 上的完整四平台构建，先安装并登录 GitHub CLI，然后运行：

```bash
gh auth login
make dist-all REF=main
```

本地产物写入被 Git 忽略的 `dist/`；可用 `DIST_DIR=/path/to/output` 改变目录。

### 2.2 GitHub Actions 发行包

仓库中的 `Build distributions` workflow 支持在 GitHub Actions 页面手动运行，也会在
推送 `v*` tag 时自动执行。每次运行先在 Linux 上执行 workspace 测试和严格 Clippy，
随后并行生成：

- `k3-linux-x86_64.tar.gz`；
- `k3-windows-x86_64.zip`；
- `k3-macos-x86_64.tar.gz`；
- `k3-macos-aarch64.tar.gz`。

手动运行时，文件保存在该 workflow run 的 Artifacts 中 14 天。推送版本 tag 时，
workflow 会创建或更新同名 GitHub Release，并附加四个平台包、各自的 `.sha256` 文件
和汇总的 `SHA256SUMS`。例如：

```bash
git tag v0.1.0
git push origin v0.1.0
```

Windows 包使用静态 MSVC C runtime。Linux 包包含 `separate.sh`；Windows 包包含
`separate.ps1` 和 `install-separator.ps1`。macOS 包目前只承诺 K3 原生二进制构建，
因此只包含 `k3`、README 和文档，不包含尚未验证的 Python 分离环境。

## 3. 安装本地分离 worker

针对 NVIDIA GPU，运行仓库提供的安装脚本：

```bash
bash python/separator/scripts/install-gpu.sh
source .venv-separator/bin/activate
```

脚本会安装：

- PyTorch 2.11、CUDA 12.8 runtime；
- `audio-separator`；
- K3 Python worker；
- 用户态 FFmpeg fallback。

Python 环境保存在 `.venv-separator/`，模型默认缓存在
`~/.cache/k3/models/`，两者均不会写入 Git。

检查 worker、CUDA 和 FFmpeg：

```bash
printf '%s\n' '{"id":"health","method":"health"}' | \
  .venv-separator/bin/k3-separator --model-dir ~/.cache/k3/models
```

成功结果中的以下字段应为 `true`：

```json
{
  "audio_separator_installed": true,
  "torch_installed": true,
  "cuda_available": true
}
```

没有 NVIDIA GPU 的 Linux 机器可安装 CPU worker。第二个参数用于固定 Python
版本；建议使用 3.13，避免系统滚动升级影响音频依赖：

```bash
bash python/separator/scripts/install-cpu.sh .venv-separator 3.13
```

老旧 CPU 建议使用 `fast`、`uvr-mdx-karaoke-2`、`segment_size=64` 或
`128`，并关闭 autocast。CPU 分离可能明显慢于歌曲时长，不建议运行 quality
RoFormer。

把 `separate.sh` 和运行包复制到另一台 Linux 机器时，最低要求是 x86-64
Linux、Bash 4（脚本使用关联数组）、`realpath`、FFmpeg、可执行的 K3 二进制，
以及已安装依赖的 Python worker。安装 worker 还需要 `uv` 和网络；运行时不需要
Rust 工具链。当前 Arch 老机器的 i7-2620M（AVX、4 线程、约 10 GiB 内存）已
通过 PyTorch 2.11 CPU worker 健康检查，可作为目前验证过的硬件下限；这不是
对更老 CPU 的兼容保证。

### 3.1 Windows 原生安装

Windows 发行包内包含 `k3.exe`、`install-separator.ps1`、`separate.ps1` 和
worker 源码。在 PowerShell 中进入解压目录，临时允许本次会话执行本地脚本，再安装
GPU worker：

```powershell
Set-ExecutionPolicy -Scope Process Bypass
.\install-separator.ps1 -Backend gpu
```

没有 NVIDIA GPU 时使用 CPU 模式：

```powershell
.\install-separator.ps1 -Backend cpu
```

脚本通过 Windows Python Launcher（`py.exe`）寻找 Python 3.11，创建
`.venv-separator`，安装模型运行环境，并将 worker 和模型目录写入同目录的
`config.json`。默认模型目录为 `%LOCALAPPDATA%\k3\models`；模型在第一次分离时
下载。检查 worker：

```powershell
'{"id":"health","method":"health"}' |
  .\.venv-separator\Scripts\k3-separator.exe `
    --model-dir "$env:LOCALAPPDATA\k3\models"
```

用 `separate.ps1` 创建同名 project 并分离。省略 `-d` 时读取同目录
`config.json` 的 `projects_root`；多个 `-f` 参数值在 PowerShell 中用逗号分隔：

```powershell
.\separate.ps1 -f "H:\music\star\Creep.flac"

.\separate.ps1 `
  -f "H:\music\star\Creep.flac","H:\music\star\难舍难分.mp3" `
  -d "H:\music\k3"
```

参数位置不固定。环境变量 `K3_OUTPUT_DIR`、`K3_BIN`、`K3_WORKER`、
`K3_PROFILE`、`K3_MODEL`、`K3_MODEL_DIR`、`K3_SEGMENT_SIZE`、
`K3_AUTOCAST` 和 `K3_PRESERVE_BACKING_VOCALS` 可临时覆盖配置。默认保留和声，
因此输出包含 `vocals.wav`、`backing-vocals.wav` 和含和声的
`accompaniment.wav`。目标 project 已存在时，再次执行同一条命令会按当前配置覆盖
stem，但保留歌词、take、效果和其他 project 文件。

### 3.2 三栏媒体库配置

复制示例配置并修改两个根目录与 worker 路径：

```bash
mkdir -p ~/.config/k3
cp docs/library-config.example.json ~/.config/k3/config.json
target/release/k3 tui --config ~/.config/k3/config.json
```

界面左栏列出 `projects_root` 下的 project，右栏递归扫描 `music_root` 中的音频。
媒体库启动时不会自动打开或播放任何 project；需要在左栏选择后按 `Enter` 打开。
按 `Tab` 或 `Shift+Tab` 切换焦点；左栏按 `Enter` 打开 project。右栏未导入的
歌曲按 `Enter` 或 `s` 创建同名 project 并在后台分离；已导入的歌曲按 `Enter`
打开，按 `s` 则使用当前配置重新分离并覆盖 stem。重新分离不会删除歌词、take、
效果或其他 project 文件；若该 project 正在录音，需要先停止或取消录音。
一个 K3 进程同时只运行一个分离任务。
分离期间可以继续在右栏选择其他歌曲并按 `Enter`，任务会按加入顺序自动排队；重复
选择同一首不会重复入队。右栏用旋转的 `⠋` 表示正在处理、`◷` 表示排队等待、`+`
表示尚未加入任务、`✓` 表示已经存在可用 project。当前任务成功或失败后都会继续
处理下一首。分离完成只刷新并选中左栏中的新 project，不会自动打开或播放，因而
不会打断当前歌曲的播放或录音。右栏未选中的文件名始终只占一行；名称过长时会在
扩展名前使用 `…` 省略。当前选中的文件名会在原位置展开并保持高亮，可换行显示完整名称。
首次分离失败时，右栏底部会持续显示红色错误信息；失败的 project 会移到
`projects_root/.failed/` 归档，不会加入左栏，对应源文件仍可在右栏按 `Enter`
重试。已有 project 重新分离失败时，原有状态与 stem 会保留，不会归档整个
project。启动时扫描到的旧失败 project 也不会显示在左栏。
目录改名不会让右栏重复导入：K3 会用 project 中的源文件名和文件大小识别已导入
歌曲。配置中的分离参数与 `separate.sh`、`separate.ps1` 对应如下：

| JSON 字段 | 脚本环境变量 | 作用 |
|---|---|---|
| `separation.worker` | `K3_PYTHON`（脚本模式） | worker 可执行文件；脚本模式下为 Python 解释器 |
| `separation.model_dir` | `K3_MODEL_DIR` | 模型缓存目录 |
| `separation.log_dir` | `K3_LOG_DIR` | 分离日志目录；单个 `separate.log` 每次覆盖 |
| `separation.profile` | `K3_PROFILE` | `fast` / `balanced` / `quality` / `compatible` |
| `separation.model` | `K3_MODEL` | 明确指定模型 |
| `separation.segment_size` | `K3_SEGMENT_SIZE` | 推理分块大小 |
| `separation.autocast` | `K3_AUTOCAST` | GPU 混合精度开关；CPU 建议设为 `false` |
| `separation.preserve_backing_vocals` | `K3_PRESERVE_BACKING_VOCALS` | 默认 `true`；设为 `false` 才关闭和声保留 |

`scan.recursive` 和 `scan.extensions` 只影响右栏扫描，`lyrics.auto_download`
控制打开 project 时本地无歌词是否自动联网下载。媒体库直接调用 K3 的 project
与分离接口，并不启动 `separate.sh` 或 `separate.ps1`；三者共享相同的 worker
和模型参数。
worker 的完整输出不会直接写入 TUI，而是保存到单个 `separate.log`。媒体库优先使用
`separation.log_dir`；命令行模式可用 `K3_LOG_DIR` 覆盖。Linux 默认路径为
`$XDG_STATE_HOME/k3/logs/separate.log`，未设置 `XDG_STATE_HOME` 时使用
`~/.local/state/k3/logs/separate.log`。每次开始分离都会覆盖上一次日志，失败提示中会
包含日志路径和一小段尾部诊断。
`recording.default_effect` 设置新 take 的默认录后效果，可选 `clean`、`studio`、
`ktv`、`theater` 或 `church`。干声不会被覆盖；停止录音后使用该效果生成 mix。
原有单 project 模式仍可使用：

```bash
target/release/k3 tui --project /path/to/project
```

### 3.3 候选状态图标（待选）

以下图标只是后续 TUI 设计的候选清单，当前版本不会因此改变快捷键或显示方式。
优先考虑在常见终端中宽度稳定的 Unicode 符号，并与必要的英文文字组合使用：

| 状态或对象 | 候选显示 |
|---|---|
| 播放 | `▶` |
| 暂停 | `Ⅱ` |
| 停止 | `■` |
| 录音准备 | `◉` |
| 正在录音 | `●` |
| 麦克风空闲 | `○` |
| 麦克风监听 | `M●` / `M○` |
| 人声 | `V` |
| 原唱 | `♪1` |
| 伴奏 | `♪2` |
| 分离人声 | `♪3` |
| 录音 Take | `♪4` |
| 固定时间后退、前进 | `↤` / `↦` |
| 按歌词后退、前进 | `«` / `»` |
| 重新开始 | `↺` |
| 排队 | `◷` |
| 处理中 | `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` |
| 成功 | `✓` |
| 失败 | `✗` |
| 下载 | `↓` |
| 人声效果 | `FX` |
| 升降 Key | `K-2` / `K+2` |

`🎙️`、`🎧`、`🔊`、`🗣️` 等 emoji 可以作为可选主题，但不宜作为默认图标：
不同终端、字体和 Unicode 版本可能把它们渲染成一个或两个字符宽度，破坏 TUI
对齐。后续可考虑提供 `unicode`、`emoji`、`ascii` 三种图标主题。

## 4. 创建歌曲工程

一个工程只对应一首原始歌曲。创建工程时 K3 会复制歌曲和可选歌词，
不会修改原始文件。

```bash
target/release/k3 new \
  --root ./songs/example \
  --song "/path/to/song.flac" \
  --lyrics "/path/to/song.lrc" \
  --title "歌曲标题"
```

`--lyrics` 和 `--title` 可以省略。Windows WSL 路径示例：

```text
H:\CloudMusic\song.flac
```

在 WSL 中对应：

```text
/mnt/h/CloudMusic/song.flac
```

工程目录结构：

```text
example/
├── project.json
├── source/
├── stems/
├── takes/
├── lyrics/
└── exports/
```

## 5. 分离人声和伴奏

推荐使用明确的 Quality 模型：

```bash
target/release/k3 separate \
  --project ./songs/example \
  --profile quality \
  --model mel-band-roformer-kim-vocal-2 \
  --worker ./.venv-separator/bin/k3-separator \
  --model-dir ~/.cache/k3/models \
  --segment-size 256
```

模型首次使用时会自动下载，后续可以断网运行。分离进度和模型日志写入 K3 的
`separate.log`，不会直接覆盖 TUI；成功后 K3 会显示工程摘要。

### 5.1 Profile 与内置模型

| Profile | 默认模型 | 定位 |
|---|---|---|
| `fast` | `uvr-mdx-karaoke-2` | 低资源、快速预览 |
| `balanced` | `uvr-mdx-inst-hq-3` | 速度和质量平衡，也是 CLI 默认档 |
| `quality` | `bs-roformer-viperx-1297` | 高质量双轨分离 |
| `compatible` | `htdemucs-ft` | 成熟的 HTDemucs 兼容路径 |

Quality 还可以显式选择：

```text
mel-band-roformer-kim-vocal-2
```

这是当前在 RTX 5070 Ti 上完成实际歌曲测试的推荐模型。

### 5.2 常用参数

- `--model <ID>`：显式选择该 profile 下的模型；
- `--segment-size <N>`：控制分块；显存不足时从 `256` 降为 `128`；
- `--no-autocast`：关闭 CUDA 混合精度；
- `--model-dir <PATH>`：设置模型缓存目录；
- `--worker <PATH>`：指定 Python worker；
- `--no-preserve-backing-vocals`：关闭默认的二次分离，不再把和声混回伴奏；
- `--overwrite`：按当前参数重新分离 ready/failed project，并覆盖已有 stem；失败时
  保留重新分离前的 project 状态与正式 stem。

### 5.3 保留和声模式

普通的 vocals/instrumental 模型通常把主唱和和声一起归入人声。K3 默认保留和声，
worker 在一次原子任务中执行：

```text
原曲
  ├─ 主模型 ─→ 全部人声 ── Karaoke 2 ─→ 主唱
  │                                  └→ 和声
  └─────────→ 纯伴奏 + 和声 ─────────→ 最终伴奏
```

第二阶段固定使用 `uvr-mdx-karaoke-2`。调用方无需更改主模型或 profile；例如主模型
仍可使用 `quality` 下的 `mel-band-roformer-kim-vocal-2`。任一阶段失败、输出缺失或
provenance 不完整时，worker 都不会提交半套正式 stem。该模式会增加一次 MDX 推理；
如果 ONNX Runtime 无法加载匹配的 CUDA 库，会回退到 CPU，因此处理时间可能明显增加。
只有明确不需要该行为时，才使用 `--no-preserve-backing-vocals`；媒体库配置或
`K3_PRESERVE_BACKING_VOCALS` 环境变量设为 `false` 也可关闭。旧配置缺少该字段时
同样按 `true` 处理。

首次分离的状态转换为：

```text
not requested -> running -> ready/failed
```

已经进入 `ready` 或 `failed` 的工程可使用 `--overwrite` 重新分离。媒体库右栏按
`s`，或再次运行 `separate.sh` / `separate.ps1`，都会自动使用该模式，无需删除
project。成功后 provenance 更新为新模型和参数；失败时仍保留上一次可用结果。

## 6. 分离结果

成功后生成：

```text
stems/vocals.wav
stems/accompaniment.wav
```

启用保留和声模式时还会生成：

```text
stems/vocals.wav          # 主唱
stems/backing-vocals.wav  # 单独和声
stems/accompaniment.wav   # 纯伴奏加回和声
```

`project.json` 会把状态保存为 `ready`，并记录：

- provider；
- 模型架构；
- checkpoint ID；
- checkpoint SHA-256；
- 实际使用的 profile。

保留和声模式还会记录第二阶段模型的 provider、架构、checkpoint ID 和
checkpoint SHA-256。旧 project 没有这些可选字段，仍可直接打开。

Rust adapter 会拒绝以下 worker 结果：

- 输出不在当前工程的 `stems/` 目录；
- 输出文件为空或不存在；
- checkpoint SHA-256 格式错误；
- worker 返回的 profile 与请求不一致。

## 7. 查看工程和打开 TUI

查看摘要：

```bash
target/release/k3 show --project ./songs/example
```

输出示例：

```text
title: 歌曲标题
source: source/song.flac
separation: ready
takes: 0
```

打开 TUI。分离完成的工程默认选择伴奏，否则选择原曲；播放器初始保持暂停，按
`Space` 后才开始播放：

```bash
target/release/k3 tui --project ./songs/example
```

如果 project 没有本地歌词，或者 `project.json` 配置的歌词文件已经不存在，K3 会在
进入 TUI 和开始录音前查询 LRCLIB。下载成功后会把新路径写回 `project.json`。若在线
搜索失败或没有可靠匹配，缺失的旧路径会被当作“未加载歌词”，不会导致 K3 退出。
若同一来源返回多个时长匹配的同步歌词，K3 不会立即写入文件，而是在 TUI 中显示
候选选择框。使用 `↑` / `↓` 查看“歌手、歌名、时长、来源”，按 `Enter` 确认下载，
按 `Esc` 跳过；只有一个可靠候选时仍会自动下载。
project 处于空闲状态时可按 `l` 强制重新搜索，即使当前已经加载歌词也不会跳过。
按下 `l` 后会先显示搜索输入框，并自动填入音频标签中的歌名；没有可用标签时使用
project 标题。可直接输入中文或英文，使用 `←` / `→`、`Home` / `End`、`Backspace` /
`Delete` 编辑，按 `Enter` 开始搜索，按 `Esc` 取消。输入内容作为完整查询发送，不会
自动拆分 `歌名 - 歌手`。
重新搜索在后台进行，原歌词会一直保留。结果框上半部分列出候选，下半部分实时预览
当前选中项的同步歌词；使用 `↑` / `↓` 对比候选及预览，确认内容匹配后按 `Enter`
下载。按 `Esc`、没有匹配、网络失败或保存失败都不会改动原歌词。
每个候选、预览标题、下载完成状态和当前会话的歌词面板标题都会显示来源标签：服务名
之后的 `auto` 表示自动主搜索命中，`fallback` 表示标题或备用服务回退命中，`manual`
表示使用手工查询，`manual fallback` 表示手工查询通过备用服务命中。历史 project 没有
保存过该元数据时，不会猜测旧歌词的在线来源。
可显式启用网易云音乐作为备用源。启用后每次在线搜索都会同时查询 LRCLIB 和网易云，
即使 LRCLIB 已有可用结果也会继续取得备用候选，方便通过预览选择更匹配的版本。结果按
主来源在前、备用来源在后排列并标注来源；一个来源出错不会丢弃其他来源已经返回的候选。
查询优先使用音频文件内的歌名、歌手和时长标签；没有 title、artist 和 album 标签时，
K3 不会尝试拆分 `歌名 - 歌手` 等不可靠的文件名格式，而是把 project 标题作为自由
搜索文本交给来源。对于网易云返回的模糊搜索候选，候选歌名和至少一位歌手必须同时
出现在自由搜索文本中；有结构化 title/artist 标签时仍使用严格匹配。
只有带 LRC 时间轴、且与音频时长相差不超过 8 秒的结果才会被采用。下载结果保存
到 project 的 `lyrics/` 目录并写入 `project.json`。网络不可用或没有可靠匹配时，
启动过程会依次显示本地检查、LRCLIB 搜索、候选命中和保存位置。单次在线请求
最长等待 15 秒；首次失败会自动重试一次。两次请求都失败或没有可靠匹配时，
TUI 会显示具体提示，但仍可正常播放和录音。

如需完全离线启动或不希望进行在线查询：

```bash
target/release/k3 tui --project ./songs/example --no-lyrics-download
```

单 project 模式使用 `--netease-lyrics` 启用网易云回退；媒体库模式在配置中设置：

```json
"lyrics": {
  "auto_download": true,
  "netease_fallback": true
}
```

K3 不会覆盖同名但无法解析的本地歌词文件；此时应先检查或移走该文件。
TUI 以及进入 TUI 前的歌词检索进度统一使用英文提示；中文只用于本手册。
网易云回退使用其免登录网页接口，该接口没有面向第三方开发者的稳定性承诺；若服务
方调整接口，K3 会显示该来源错误并保留 LRCLIB 与本地歌词流程，不会覆盖已有歌词。
各来源的官方能力、限制与接入判断见
[歌词来源调研](lyrics-sources-research.md)。

如果录下的人声相对伴奏偏后，可用毫秒数将人声提前，并把设置保存到
`project.json`：

```bash
target/release/k3 tui --project ./songs/example --latency-ms 150
```

正值表示在生成 `mix.wav` 时把人声提前，负值表示延后；允许范围是
`-1000..1000`。该设置不改写原始 `dry.wav`，并会用于之后录制的 take。
录音预备阶段已经预加载伴奏，开始录音时不会再次解码，以减少每次录音不一致
的启动延迟。

如需在不改变播放速度的情况下升降调，可以在启动时指定半音数：

```bash
target/release/k3 tui --project ./songs/example --key -2
```

允许范围是 `-6..6`，并会保存到 `project.json`。负数降调，正数升调；每次变化
一个半音。该设置会应用到原曲、伴奏和分离人声的播放，以及新生成的 take
伴奏，但不会变调麦克风录下的 dry 人声。

播放按键：

底部快捷键仍保留完整文字说明；当前模式或当前 project 下不可用的操作会显示为
深灰色。例如录音期间，播放暂停、重新开始、take、效果和 Key 操作会变灰，而
歌词跳转、音量、监听音轨和停止录音仍保持正常颜色。

- `Space`：暂停或继续；
- `←` / `→`：后退或前进 5 秒；
- `r`：从头播放；
- `1` / `2` / `3`：切换原曲、伴奏、人声；
- `-` / `+`：调整音量；
- `,` / `.`：降调或升调一个半音；
- `/`：恢复原 Key（`+0`）；
- `a`：将录音置于预备状态并把当前音轨归零；
- `m`：开启或关闭麦克风实时监听（耳返）；
- `Enter`：从头播放并开始录音；再次按下则停止并保存；
- `Esc`：取消尚未开始的录音预备状态；
- `q`：退出。

### 人声效果与录后修改

每个 take 独立保存一个人声效果预设：

- `clean`：原声，不增加空间效果；
- `studio`：轻度棚录混响，声音较近；
- `ktv`：短回声和中等混响，适合常见包房演唱；
- `theater`：较宽的中型空间和更长尾音；
- `church`：大空间、长混响，尾音最长。

效果只处理 dry 人声，再与伴奏重新生成 mix；不会处理伴奏，也不会覆盖或反复
处理 dry。因此可以在录音结束后多次切换预设。

在 TUI 中：

- `[` / `]`：循环选择已有 take；
- `e` 后按 `1`～`5`：直接选择 `clean`、`studio`、`ktv`、`theater` 或
  `church`，重新生成 mix 并立即播放；按 `Esc` 取消选择；
- `4`：播放当前选择的 take mix。

如果修改 Key 后播放旧 take，K3 会使用原始 dry 人声、当前效果和新 Key 的伴奏
自动重建 mix，再开始播放。dry 人声始终保持不变，已经做进 mix 的人声也不会被
整体二次变调。

也可以退出 TUI 后使用命令行修改，`--take` 默认为最新 take：

```bash
target/release/k3 effect \
  --project ./songs/example \
  --take latest \
  --preset ktv
```

指定旧 take：

```bash
target/release/k3 effect \
  --project ./songs/example \
  --take take-1786272214377 \
  --preset church
```

渲染过程先写临时文件，成功后才替换旧 mix，并把预设保存到
`project.json`。剧场和教堂预设会在录音末尾增加混响尾音。

歌词由播放器的真实位置驱动，暂停和跳转时会同步变化。歌词区域会滚动展示
上下文：当前行以 `▶` 和黄色粗体高亮，已唱行变暗，并把更多空间留给后续
待唱歌词。终端越高，可提前看到的歌词越多。如果当前系统没有可用输出设备或
音频无法解码，TUI 仍会打开，并在 audio 状态行显示错误。

录音使用系统默认麦克风，停止后在 project 下生成两个文件：

- `takes/take-<时间戳>-dry.wav`：未经伴奏混合的麦克风原始干声；
- `takes/take-<时间戳>-mix.wav`：当前伴奏轨与增强人声组成的双声道试听混音。

两者都会登记到 `project.json`。保留 dry 文件是为了以后可以重新调整人声、
伴奏和效果；日常试听应打开 mix 文件。录音过程中 `←` / `→` 按歌词时间跳转，
`1` / `2` / `3` 可以切换监听音轨，但 take 始终只混入伴奏；按 `q` 会先停止并
保存当前 take。WSLg 使用 Windows 默认麦克风，需在 Windows
的“隐私和安全性 → 麦克风”中允许桌面应用访问。

麦克风监听默认关闭。按 `m` 开启后，录音时可以从输出设备实时听到自己的
声音；再次按 `m` 关闭。必须使用耳机，使用扬声器可能产生明显回声或啸叫。
监听使用 250 ms 的无锁环形缓冲。原生 Linux 在积累约 20 ms 音频后开始
输出，以降低监听延迟；WSL 为吸收 RDP 音频回调抖动，会积累约 100 ms，
因此仍可能有可感知的监听延迟。耳返信号会增加约 12 dB 并在输出前限幅，
便于盖过伴奏；保存的 dry take 不应用该增益，保留原始麦克风电平供后续混音。

## 8. 查看 worker 模型

Rust CLI 当前没有单独的 models 子命令，可以直接查询 worker：

```bash
printf '%s\n' '{"id":"models","method":"list_models"}' | \
  .venv-separator/bin/k3-separator --model-dir ~/.cache/k3/models
```

自定义模型注册表的格式和安全要求见
[`python/separator/README.md`](../python/separator/README.md)。PyTorch `.ckpt`
可能包含 pickle 数据，只应使用可信来源并固定 SHA-256。

## 9. 常见问题

### 找不到 `k3-separator`

激活环境，或显式传入 worker：

```bash
source .venv-separator/bin/activate

target/release/k3 separate \
  --project ./songs/example \
  --worker "$PWD/.venv-separator/bin/k3-separator"
```

### CUDA 不可用

先运行 `nvidia-smi` 和 worker `health`。如果 `nvidia-smi` 失败，应先修复
宿主机驱动或 WSL GPU 透传；不要退回 CPU 后误以为模型卡死。

### CUDA out of memory

降低分块并确保没有其他模型进程占用显存：

```bash
target/release/k3 separate \
  --project ./songs/example \
  --profile quality \
  --segment-size 128 \
  --worker ./.venv-separator/bin/k3-separator
```

检查占用：

```bash
nvidia-smi
```

### `output_exists`

worker 默认不会覆盖 `vocals.wav`、`backing-vocals.wav` 或
`accompaniment.wav`。确认文件可替换且
工程仍为 `not requested` 后使用 `--overwrite`。

### 工程显示 `failed`

失败信息会保存进 `project.json`。当前版本没有失败重试状态转换；保留原始
歌曲，修复环境后新建工程再运行。

## 10. 当前限制

当前版本尚未实现：

- 手动选择麦克风和输出设备；
- 设备延迟校准；
- 伴奏变调及人声共振峰变声；
- 混响、压缩等效果链；
- 离线最终混音导出；
- macOS 打包。

这些能力将继续通过音频和模型 seams 接入，不需要让 TUI 直接依赖具体模型
或音频后端。
