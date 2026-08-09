# K3 本地 K 歌软件：MVP 纵向切片规格

状态：已确认需求的首个实现切片  
日期：2026-08-04

## 1. 产品目标

K3 是完全本地运行的终端 K 歌软件。最终产品支持 Windows、macOS 和 Linux；首个开发目标为 Linux，Windows 与 macOS 通过音频和外部进程适配器接入。

完整产品应支持：本地歌曲导入、人声/伴奏离线分离、LRC 歌词、耳机监听、麦克风干声录制、设备延迟补偿、轨道无损移动、伴奏升降调、人声变声、内置效果链和离线高质量导出。

## 2. 本次实现范围

本次交付建立一个可以编译、运行和持续扩展的纵向切片：

1. Rust workspace 和纯 TUI 应用骨架；
2. 创建、保存、打开本地歌曲工程；
3. 导入歌曲及可选 LRC 歌词，并保存规范化工程元数据；
4. 解析 LRC，根据播放位置选择当前歌词，支持全局偏移；
5. 定义并使用本地分轨 worker seam，业务层不依赖 Demucs、RoFormer 或 Python；
6. 建立录音会话状态机并通过默认输入设备写入 WAV 干声 take；
7. 工程永久区分原始歌曲、分轨结果、原始干声 take 和试听混音；
8. TUI 展示工程、歌曲准备状态、录音状态与当前歌词；
9. 提供非交互 CLI，方便自动化创建和检查工程。

## 3. 明确不在本次范围

- 手动音频设备选择以及 Windows/macOS 原生输入 adapter；
- 调用真实 Demucs/RoFormer 权重；
- 监听效果 DSP、变调、共振峰和混响；
- 离线混音与 WAV/FLAC/MP3 导出；
- 在线曲库、在线歌词和评分。

这些能力必须能在不修改领域模型调用方的情况下，通过已定义 seam 继续实现。

## 4. 领域模型

### Project

每个工程位于独立目录，至少包含：

```text
project.json
source/
stems/
takes/
lyrics/
exports/
```

`project.json` 保存：稳定工程 ID、标题、原始歌曲相对路径、可选歌词相对路径、分轨状态、take 清单、延迟补偿和效果参数版本。

所有保存到 JSON 的素材路径必须是相对工程目录的安全路径，不允许绝对路径或 `..` 穿越。

### Separation

状态为：

```text
NotRequested -> Running -> Ready
                        -> Failed
```

成功结果至少包含 `vocals` 和 `accompaniment` 两个相对路径，并记录 provider、architecture、checkpoint ID、checkpoint SHA-256 和推理 profile。

### RecordingSession

状态为：

```text
Idle -> Armed -> Recording -> Idle
```

只允许：

- `Idle.arm()`；
- `Armed.start()`；
- `Recording.stop(take)`；
- `Armed.cancel()`。

非法转换返回领域错误，不能静默忽略或 panic。

### LyricsTimeline

- 支持 `[mm:ss.xx]`、`[mm:ss.xxx]` 和一行多个时间戳；
- 忽略已知元数据行和无法识别的普通行；
- 同时刻歌词保持输入顺序；
- 查询时间应用全局偏移；
- 第一条歌词之前返回空。

## 5. 公共 seams 与测试面

### ProjectRepository

```text
create(request) -> Project
open(project_dir) -> Project
save(project) -> ()
```

验收：创建目录结构；复制本地歌曲/LRC；原子保存 JSON；拒绝危险路径；保存后可重新打开且语义一致。

### LyricsTimeline

```text
parse(text) -> LyricsTimeline
line_at(position, offset) -> optional LyricsLine
```

验收：解析多时间戳、毫秒精度、偏移以及第一行前空值。

### StemSeparator

```text
separate(request) -> SeparationManifest
```

`SongPreparation` 只依赖此 interface。验收：成功时写回 Ready 与完整模型溯源；失败时写回 Failed 且保留原始歌曲。

### RecordingSession

```text
arm / start / stop / cancel
```

验收：合法状态转换成功；非法顺序返回明确错误；停止后把干声 take 加入工程。

## 6. CLI/TUI 行为

```text
k3 new --root <dir> --song <file> [--lyrics <file>] [--title <title>]
k3 show --project <dir>
k3 tui --project <dir>
```

- `new` 成功后输出工程目录；
- `show` 输出人类可读摘要；
- `tui` 使用终端 alternate screen，滚动展示歌词上下文并高亮当前行，`q` 退出；
- CLI 错误输出到 stderr，并返回非零退出码。

## 7. 质量约束

- Rust stable，`cargo fmt --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test --all` 全部通过；
- 音频实时线程相关实现未来不得依赖 TUI 或工程 JSON；
- 模型 worker 崩溃不得破坏已有工程；
- 不覆盖用户已有源文件；
- 测试通过公共 seam，不测试私有函数。

## 8. 后续里程碑

1. 音频输入设备选择和录音电平显示；
2. JSON-lines Python worker 与首个分轨模型；
3. 播放、歌词时钟与设备延迟校准；
4. 离线效果链和 WAV 渲染；
5. 伴奏升降调及人声共振峰变声；
6. Windows/macOS adapter 与打包。
