# K3 音轨分离 worker

worker 将模型 runtime 隔离在 Rust 进程之外。它从标准输入逐行读取 JSON 请求，
并为每个请求向标准输出写入且仅写入一行 JSON 响应；依赖库日志重定向到标准错误。

## Linux 安装

RTX 5070 Ti 开发机可使用仓库内安装脚本。脚本使用 `uv`、PyTorch 2.11 和
CUDA 12.8，并避开需要 Ubuntu Python 开发头文件的可选 `diffq` 扩展：

```bash
bash python/separator/scripts/install-gpu.sh
source .venv-separator/bin/activate
```

系统无 FFmpeg 时，这种安装方式会提供用户态 FFmpeg，并只将其暴露给 worker
进程。

已安装 Python 开发头文件的系统也可使用普通 pip：

```bash
python3 -m venv .venv-separator
source .venv-separator/bin/activate
python -m pip install -e './python/separator[gpu]'
```

使用 NVIDIA GPU 时，应先安装与机器匹配的 PyTorch wheel，再安装 editable
package。worker 首次使用模型时将 checkpoint 下载到 `~/.cache/k3/models`。

## Windows 安装

Windows 发行目录包含仓库根目录下的 `install-separator.ps1`、
`separate.ps1` 和本目录源码。需要 64 位 Python 3.11；GPU 模式还需要 NVIDIA
驱动。请在发行目录的 PowerShell 中运行：

```powershell
Set-ExecutionPolicy -Scope Process Bypass
.\install-separator.ps1 -Backend gpu
```

没有 NVIDIA GPU 时使用 `-Backend cpu`。脚本会创建 `.venv-separator`、安装
PyTorch 与 worker、准备用户态 FFmpeg，并更新同目录 `config.json` 的 worker
和模型路径。Windows 不需要预装 `uv` 或系统 FFmpeg。
worker 会将 JSON-lines 协议的标准输入和标准输出强制设为 UTF-8，因此中文歌曲名、
project 名和路径不依赖 Windows 当前代码页。

验证安装：

```powershell
'{"id":"health","method":"health"}' |
  .\.venv-separator\Scripts\k3-separator.exe `
    --model-dir "$env:LOCALAPPDATA\k3\models"
```

## 运行

```bash
python -m k3_separator --model-dir ~/.cache/k3/models
```

请求示例：

```json
{"id":1,"method":"health"}
{"id":2,"method":"list_models"}
{"id":3,"method":"separate","params":{"input_path":"/music/song.flac","output_dir":"/project/stems","profile":"quality","model_id":"bs-roformer-viperx-1297","options":{"autocast":true,"segment_size":256}}}
```

成功分离一定会写入 `vocals.wav` 和 `accompaniment.wav`，并返回它们的绝对路径
及准确的模型 provenance。只有明确请求 `"overwrite": true` 才会覆盖已有输出。

## 保留和声模式

`preserve_backing_vocals` 默认为 `true`。worker 会先用选定的主模型分离全部人声，
再固定使用 `uvr-mdx-karaoke-2` 把人声拆成主唱与和声；只有在请求的 `params` 中
明确设置 `"preserve_backing_vocals": false` 才会关闭。默认输出为：

- `vocals.wav`：主唱；
- `backing-vocals.wav`：单独和声；
- `accompaniment.wav`：主模型伴奏与和声之和。

返回的 provenance 会同时包含主模型和 `backing_vocals_model`。两个阶段、合成、
文件校验与 provenance 校验属于同一次原子任务；任一步失败都不会提交新 stem。

内置 profile：

- `fast`：UVR MDX karaoke 模型；
- `balanced`：UVR MDX instrumental HQ 模型；
- `quality`：BS-RoFormer（默认）或 Mel-Band RoFormer；
- `compatible`：HTDemucs fine-tuned ensemble。

## 新增或覆盖模型

传入 `--models /path/to/models.json`。条目会按 ID 与内置注册表合并，因此既可
新增 checkpoint，也可替换内置定义：

```json
{
  "models": [
    {
      "id": "my-roformer",
      "filename": "my-roformer.ckpt",
      "architecture": "bs-roformer",
      "profiles": ["quality"],
      "output_stems": ["Vocals", "Instrumental"],
      "license": "MIT",
      "source_url": "https://example.invalid/model",
      "download_url": "https://example.invalid/model.ckpt",
      "expected_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
      "runtime_options": {"segment_size": 256, "overlap": 8, "batch_size": 1}
    }
  ]
}
```

只可添加可信 checkpoint。PyTorch `.ckpt` 可能包含 pickle 数据。当
`download_url` 和 `expected_sha256` 同时存在时，worker 会先下载到临时文件并
校验，再允许 `audio-separator` 反序列化。生产注册表应始终固定这两个字段，并
保留许可证快照。

## 进度观测

GUI 为每首分离任务在工程根目录下建立独立的 `.k3-progress-<UUID>` 临时目录，
通过 `K3_SEPARATION_PROGRESS_PATH` 将其中的 `progress.json` 路径传给 worker。
正常完成或失败后 GUI 都会清理该目录；模型缓存与原有诊断日志目录不变。
独立运行 worker 时不设置此变量即可关闭进度文件输出。

进度文件采用 UTF-8 JSON，以临时文件替换方式更新，格式如下：

```json
{"schema_version": 1, "phase": "separating_vocals", "fraction": 0.37}
```

`phase` 可以是 `preparing`、`loading_vocals`、`separating_vocals`、
`loading_backing_vocals`、`separating_backing_vocals`、`writing_audio`、`saving_project`。
`fraction` 是当前推理阶段的真实完成比例；无法得知比例时为 `null`。
百分比从模型运行时的 tqdm 输出提取，原始 stderr 日志完整保留；此信息不会混入
标准输出的请求响应协议。GUI 只有在脚本退出并成功保存工程后才认定任务完成。
旧 worker 没有进度文件时，GUI 仍显示活动条与已用时。进度文件无法写入也不会中断分离。

## 测试

测试使用进程内 fake runtime，不会下载模型：

```bash
PYTHONPATH=python/separator/src \
  python -m unittest discover -s python/separator/tests -v
```
