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

## 远程 Separator Server

同一套模型 runtime 也可以作为独立 HTTP/WebSocket 服务运行，让 K3 从其他机器上传
原始歌曲。安装、token、CPU/macOS、GPU、容量基准与 K3 配置见
[`docs/separator-server.md`](../../docs/separator-server.md)。最小协议测试命令：

```bash
export K3_SEPARATOR_TOKEN='development-only-token'
k3-separator-server \
  --data-dir ./separator-data \
  --runtime fake \
  --token-env test-client=K3_SEPARATOR_TOKEN
```

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

## 测试

测试使用进程内 fake runtime，不会下载模型。FastAPI 0.141 的测试客户端来自
`httpx2`，因此不要再注入旧 `httpx` 适配层。下面的命令显式建立隔离测试环境：

```bash
uv run --no-project --with-editable python/separator \
  --with 'fastapi>=0.141,<0.142' --with httpx2 \
  --with 'uvicorn>=0.52,<0.53' \
  --with numpy --with soundfile \
  python -m unittest discover -s python/separator/tests -v
```
