# Separator Server 部署与使用

K3 可以把原始歌曲上传到另一台机器完成音轨分离。服务器使用 HTTP 处理上传、任务
查询与 artifact 下载，并提供 WebSocket 进度订阅。HTTP 中的 job 状态是权威状态；
WebSocket 断开不会影响任务，当前 K3 客户端会自动使用 HTTP polling。

服务器是 Python ASGI 应用。ASGI 是 Python Web Server Gateway Interface 的异步版本，
因此同一个应用可以同时提供普通 HTTP 与长期 WebSocket 连接；Uvicorn 是实际监听
端口并驱动该应用的进程。

## 安装

只测试协议和 K3 连接，不运行模型：

```bash
python3 -m venv .venv-separator
source .venv-separator/bin/activate
python -m pip install -e './python/separator[server]'
```

Linux 或 macOS 的真实 CPU 推理：

```bash
python -m pip install -e './python/separator[server,cpu]'
```

Linux/NVIDIA GPU 使用 `gpu` extra，并应先按显卡驱动安装匹配的 PyTorch wheel：

```bash
python -m pip install -e './python/separator[server,gpu]'
```

macOS 可以使用 `--runtime cpu`。`--runtime coreml` 会让 runtime 使用可用的
Apple 加速路径；依赖或设备不支持时任务会明确失败，不会静默换成 CPU。CI、协议联调
和没有模型依赖的机器使用 `--runtime fake`。

生产模型应在接收 job 前由管理员安装。registry 同时提供固定下载 URL 与 SHA-256 的
模型可使用以下命令原子下载和校验；没有固定下载信息的模型会给出应放置的文件路径：

```bash
k3-separator-models \
  --model-dir /srv/k3-separator/models \
  install bs-roformer-viperx-1297
```

## 启动私有服务器

token 从环境变量读取，避免写入配置文件。可以重复传入 `--token-env`，为每台 K3
设备分配独立 token：

```bash
export K3_SEPARATOR_LAPTOP='replace-with-a-long-random-token'
export K3_SEPARATOR_KTV='replace-with-another-token'

k3-separator-server \
  --data-dir /srv/k3-separator \
  --runtime cuda \
  --model-dir /srv/k3-separator/models \
  --max-jobs 1 \
  --token-env laptop=K3_SEPARATOR_LAPTOP \
  --token-env ktv=K3_SEPARATOR_KTV
```

默认只监听 `127.0.0.1:8765`。远程访问应通过 Caddy、Nginx、Tailscale Serve 或 VPN
提供 TLS；直接绑定非 loopback 地址还必须传 `--public-behind-proxy`，这是防止误把
bearer token 暴露在明文公网的保护开关。

快速检查：

```bash
curl http://127.0.0.1:8765/healthz
curl -H "Authorization: Bearer $K3_SEPARATOR_LAPTOP" \
  http://127.0.0.1:8765/v1/capabilities
```

数据目录使用 SQLite WAL 保存 inputs、jobs、幂等 key 和 artifact 元数据；原始输入与
结果按 SHA-256 存放。单 dispatcher 直接管理 `--max-jobs N` 个 spawned process future，
不再叠加每槽一个阻塞 scheduler thread。整套 artifact link、job result 和 completed
状态在同一事务提交。没有 CUDA 时，fake server 和真实 CPU server 都可启动。

## K3 命令行

先创建 project，再指定远程 URL、token 环境变量、模型、质量和输出布局：

```bash
export K3_SEPARATOR_TOKEN='replace-with-device-token'

k3 separate \
  --project /music/k3/example \
  --server-url https://separator.example.com \
  --server-profile studio-gpu \
  --token-env K3_SEPARATOR_TOKEN \
  --model bs-roformer-viperx-1297 \
  --profile quality \
  --output-layout karaoke
```

`two-stem` 下载 `vocals.wav` 与 `accompaniment.wav`；`karaoke` 还下载
`backing-vocals.wav`，且伴奏包含和声。上传前后以及下载后都会校验 SHA-256。K3 还会
拒绝非 44.1 kHz、双声道、32-bit float WAV。

创建远程 job 后，K3 会先把 `server_profile`、`input_id` 与 `job_id` 写入
`project.json`，再等待结果。因此进程退出或网络中断不会丢失任务引用；以同一 server
profile 再次运行命令即可恢复。重新分离期间旧 stem 保持可播放，新文件全部验证通过后
才把整个目录发布为 `stems/{job_id}/`，随后原子更新 project manifest。旧 manifest
引用的文件不会被就地覆盖。

媒体库模式可使用 [`library-config.remote.example.json`](library-config.remote.example.json)。
token 的值只从 `token_env` 指定的环境变量读取，不会序列化进 project 或配置文件。

## 为固定 GPU 选择 `--max-jobs`

不要按显存标称值猜并发。使用代表性的长歌曲、实际模型、preset 与输出布局，在目标机器
直接运行容量基准：

```bash
k3-separator-benchmark \
  --input /music/representative-long-song.flac \
  --backend cuda \
  --model-dir /srv/k3-separator/models \
  --model bs-roformer-viperx-1297 \
  --preset quality \
  --output-layout karaoke \
  --max-jobs 4
```

命令会分别以 1 到 N 个 spawned worker 同时运行，输出每档 wall time、吞吐、失败数和
可取得时的单 worker CUDA peak memory，并推荐本轮没有失败的最高并发。生产配置应比
一次测试结果保守：用多首长歌重复测试，同时观察系统显存、温度、磁盘和延迟；任何 OOM、
worker crash 或不可接受的尾延迟都应降低 `--max-jobs`。同一模型的并发也需要多个模型
实例，不能把一个 GPU model object 安全地同时交给多个请求。

## 质量与模型

客户端必须同时指定 `model_id` 和统一的 `fast`、`balanced` 或 `quality` preset。
preset 是质量/成本意图，不是另一个模型名；服务器 registry 决定该模型允许哪些 preset
以及展开后的底层参数。`GET /v1/models` 用于发现可用组合。CPU、CUDA 与 macOS backend
不会互相共享结果缓存。

完整的协议、恢复规则、安全边界和后续管理端点见
[`remote-separator-server-spec.md`](remote-separator-server-spec.md)。
