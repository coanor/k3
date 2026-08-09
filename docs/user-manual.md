# K3 用户手册

K3 是一个本地优先的终端 K 歌工作区。目前可创建歌曲工程、导入 LRC
歌词、使用本地 GPU 模型分离人声和伴奏，并在 TUI 中播放和切换音轨。

本文以 Ubuntu 24.04 或 Windows WSL2 为当前支持环境。Windows 原生和
macOS 的音频、模型安装与打包仍属于后续工作。

## 1. 系统要求

基础程序需要：

- Rust stable；
- ALSA 开发库（Ubuntu/WSL 使用 `sudo apt install libasound2-dev`）；
- Python 3.10 或更高版本；
- `uv`；
- 足够存放模型和 WAV stem 的磁盘空间。

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

模型首次使用时会自动下载，后续可以断网运行。分离进度和模型日志输出到
stderr，成功后 K3 会显示工程摘要。

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
- `--overwrite`：允许 worker 覆盖已经存在的 stem 文件。

当前工程状态只允许：

```text
not requested -> running -> ready/failed
```

已经进入 `ready` 或 `failed` 的工程不能再次执行分离。当前版本尚无 reset
命令，因此 `--overwrite` 主要用于工程仍为 `not requested`、但 stem 文件已
存在的恢复场景。若要比较另一模型，请新建一个工程。

## 6. 分离结果

成功后生成：

```text
stems/vocals.wav
stems/accompaniment.wav
```

`project.json` 会把状态保存为 `ready`，并记录：

- provider；
- 模型架构；
- checkpoint ID；
- checkpoint SHA-256；
- 实际使用的 profile。

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

打开 TUI。分离完成的工程会默认播放伴奏；否则播放原曲：

```bash
target/release/k3 tui --project ./songs/example
```

播放按键：

- `Space`：暂停或继续；
- `←` / `→`：后退或前进 5 秒；
- `r`：从头播放；
- `1` / `2` / `3`：切换原曲、伴奏、人声；
- `-` / `+`：调整音量；
- `q`：退出。

歌词由播放器的真实位置驱动，暂停和跳转时会同步变化。如果当前系统没有
可用输出设备或音频无法解码，TUI 仍会打开，并在 audio 状态行显示错误。

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

worker 默认不会覆盖 `vocals.wav` 或 `accompaniment.wav`。确认文件可替换且
工程仍为 `not requested` 后使用 `--overwrite`。

### 工程显示 `failed`

失败信息会保存进 `project.json`。当前版本没有失败重试状态转换；保留原始
歌曲，修复环境后新建工程再运行。

## 10. 当前限制

当前版本尚未实现：

- 麦克风真实录音；
- 实时耳返和设备延迟校准；
- 伴奏变调及人声共振峰变声；
- 混响、压缩等效果链；
- 离线最终混音导出；
- Windows 原生和 macOS 打包。

这些能力将继续通过音频和模型 seams 接入，不需要让 TUI 直接依赖具体模型
或音频后端。
