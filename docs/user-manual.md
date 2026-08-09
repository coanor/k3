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
- `e`：切换到下一个效果，重新生成 mix 并立即播放；
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
伴奏和效果；日常试听应打开 mix 文件。录音过程中不能跳转或切换音轨；按
`q` 会先停止并保存当前 take。WSLg 使用 Windows 默认麦克风，需在 Windows
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

worker 默认不会覆盖 `vocals.wav` 或 `accompaniment.wav`。确认文件可替换且
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
- Windows 原生和 macOS 打包。

这些能力将继续通过音频和模型 seams 接入，不需要让 TUI 直接依赖具体模型
或音频后端。
