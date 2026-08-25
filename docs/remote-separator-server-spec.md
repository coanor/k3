# 远程音轨分离服务器规格

状态：规格已确认；第一阶段纵向功能已实现
固定点：`6cfe4af3a684ebd055171fc11b3c3627abc1dd5d`
日期：2026-08-26

## 目标

将音轨分离从 K3 本地 Python 子进程迁移为一个可独立部署的 separator server。K3
通过 HTTP 上传原始歌曲、创建与控制任务、下载结果，通过 WebSocket 接收实时进度。
server 可以与 K3 位于不同机器，也可以在同机通过 loopback 使用。

远程 server 最终是 K3 唯一长期分离架构。现有本地 Python worker adapter 保留一个
迁移周期，达到功能与质量 parity 后再删除。

## 非目标

- 首版不把模型推理重写为 Rust。
- 首版不支持多租户账号、匿名公网服务或客户端上传任意 checkpoint。
- 首版不支持分块断点续传上传；失败的上传从头重试。
- 首版不以 WebSocket 作为权威状态存储。
- 首版不允许客户端直接覆盖 `segment_size`、`overlap` 等底层模型参数。
- 首版不暴露模型任意原生 stems。

## 当前实现边界

本次提交完成可运行的第一阶段：HTTP 上传/查询/下载、WebSocket 状态事件、bearer token、
SQLite 持久任务与重启恢复、结果缓存、fake/CPU/CUDA/CoreML runtime、spawned worker pool、
模型 allowlist、并发 benchmark，以及 K3 的持久 remote operation、HTTP polling、结果校验与
原子 stem 替换。现有本地 Python adapter 仍是默认路径。

以下属于后续强化，不作为本次提交已完成能力：Docker 发布物、TTL/GC、运行中任务的
graceful/强制 worker 取消、runtime chunk 级进度、K3 WebSocket manager、Range 断点续传、
完整 retry/backoff、真实 CPU/CUDA 音质 parity fixture 与迁移周期后的本地 adapter 删除。

## 部署与信任模型

server 是私有、单租户系统，但同一 owner 可以从多台 K3 设备访问。每台设备使用
独立 bearer token；所有 token 访问同一 owner 的 inputs、jobs 和 artifacts。server
只保存 token hash，日志只记录 `token_id`。

TLS 由 Caddy、Nginx、Tailscale Serve 或 VPN tunnel 提供。应用默认只监听
loopback；绑定非 loopback 必须显式声明已经位于受保护代理或隧道之后。HTTP 和
WebSocket upgrade 都要求同一 bearer token。

发布方式：

- Python CLI 是基础 interface；
- Linux/CUDA 优先提供 GPU Docker image；
- Linux/CPU 与 macOS CPU/CoreML 支持原生 virtualenv/`uv` 安装；
- Docker 与原生安装共享配置 schema 和 data directory 结构。

运行 backend 是 server 启动级配置：

- `cuda`：CUDA 缺失时 readiness 失败，不自动降级；
- `coreml`：CoreML/MPS 缺失时失败；
- `cpu`：强制真实 CPU 推理；
- `fake`：用于协议、恢复与故障测试；
- `auto`：按 CUDA、CoreML/MPS、CPU 顺序解析，并公开最终选择。

## 总体架构

```text
K3
├── HTTP：上传、任务控制、状态查询、artifact 下载
└── WebSocket：结构化进度订阅
          │
          ▼
Python ASGI control process
├── HTTP/WebSocket adapter
├── bearer-token authentication
├── SQLite job store
├── content-addressed filesystem
├── resource-aware scheduler
└── spawned inference worker pool
          │
          ▼
每个 worker process 同时执行一个 job
├── persistent model cache
├── CUDA/CPU/CoreML/fake runtime
├── request-local processing state
└── scratch outputs
```

ASGI control process 是唯一数据库 writer。Inference workers 通过 Python
`multiprocessing` 的 `spawn` context 和结构化 queues/pipes 接收
`run/cancel/shutdown`，返回 `started/progress/completed/failed/heartbeat`。worker
不打开 SQLite，不发布最终 artifacts，也不在处理任务时访问外网。

每个 worker 同时只运行一个 job。并发通过多个长期 worker 实现；同模型并发会加载
多个模型实例，因此 benchmark 必须包含重复权重成本。worker crash、OOM、强制取消
只影响一个 job。

## 公开协议

所有资源位于 `/v1/`：

```text
POST /v1/inputs
PUT  /v1/inputs/{input_id}/content
GET  /v1/inputs
DELETE /v1/inputs/{input_id}

GET  /v1/models

POST /v1/jobs
GET  /v1/jobs
GET  /v1/jobs/{job_id}
POST /v1/jobs/{job_id}/cancel

GET  /v1/artifacts/{artifact_id}
DELETE /v1/jobs/{job_id}/artifacts

WS   /v1/events

GET  /v1/capabilities
GET  /healthz
GET  /readyz
GET  /metrics
```

HTTP contract 保存为 checked-in OpenAPI snapshot；WebSocket events 保存为 JSON
Schema。Python 和 Rust 使用相同 fixtures 做 contract tests。protocol major 不兼容时
K3 拒绝提交；minor 版本只能增加向后兼容的 optional 字段。

### Capabilities

`GET /v1/capabilities` 至少返回持久 `server_id`、protocol major/minor、feature
flags、input/queue limits、resolved backend 和 device。`server_id` 在初始化 data
directory 时生成，重启后不得改变。

### 输入上传

K3 上传原始 MP3、FLAC、WAV、M4A、AAC 或 OGG 文件；server 负责格式探测、解码、
重采样和声道标准化。

两步上传流程：

1. K3 先计算完整文件 SHA-256，并向 `POST /v1/inputs` 提交原文件名、字节长度和
   SHA-256；
2. server 可以根据 SHA-256 直接复用已有 input，否则返回 `input_id`；
3. K3 通过 `PUT /v1/inputs/{input_id}/content` 流式上传；
4. server 边写临时文件边重新计算 SHA-256；
5. 长度与 hash 一致并通过音频探测后，input 原子变为 `ready`。

客户端从 HTTP 已发送字节数计算上传进度。WebSocket 只报告 server 端的
`validating_input`、`probing_audio`、`input_ready` 或 `input_rejected`。

server 同时限制上传字节、解码后时长、输入声道、队列长度与最小磁盘余量。具体值
来自 server 配置，并通过 capabilities 公开。上传不支持首版断点续传；网络失败后
从头重传。

### 模型目录与任务规格

模型只能来自 server allow-listed registry。管理员通过
`k3-separator-server models install <model_id>` 显式下载、校验并原子安装；job
不能触发模型下载。`GET /v1/models` 返回：

- model ID 与显示名称；
- provider、architecture、checkpoint SHA-256；
- source URL、license 与 redistribution 状态；
- `ready | not_installed | invalid`；
- 支持的 backend、preset 与 output layout；
- 展开参数和 benchmark 摘要。

Job request：

```json
{
  "input_id": "input_...",
  "model_id": "bs-roformer-viperx-1297",
  "preset": "quality",
  "output_layout": "karaoke"
}
```

`model_id` 必填。preset 使用统一的 `fast | balanced | quality` 用户意图；每个模型
可只支持部分 preset。server 按 `model + backend + preset` 展开白名单参数。同名
preset 的质量参数应尽量一致，batch size、线程和 precision 等资源参数可以按 backend
变化。job 创建后 resolved spec 不再随注册表变化。

Output layout：

- `two_stem`：`vocals` 是全部人声，`accompaniment` 是纯器乐；
- `karaoke`：`lead_vocals`、`backing_vocals` 与包含和声的 `accompaniment`。

`karaoke` 的第二阶段模型和 preset 由 server registry 的 pipeline 配置解析。主模型、
第二模型、checkpoint 与展开参数都进入 resolved spec 和 provenance。

### Idempotency 与结果缓存

创建 job 必须携带 UUID `Idempotency-Key`。同一 token 下重复 key 返回同一 job，避免
网络超时造成重复排队。用户主动重新执行必须生成新 key。

完全相同的执行可以命中结果缓存；`force=true` 明确重新推理。cache key 至少包含：

- input SHA-256；
- 主模型与第二模型 checkpoint SHA-256；
- 展开后的 preset 参数；
- output layout；
- pipeline/runtime 版本；
- backend、provider、precision 与关键依赖版本；
- 随机 seed。

不同 CUDA、CPU 与 CoreML backend 不共享 cache。cache hit 仍创建可追踪 job，并在
provenance 中记录。

### Job 状态与进度

持久主状态：

```text
queued | running | cancelling | cancelled | completed | failed
```

`stage` 是独立、可扩展的执行细节：

```text
queued
loading_model
decoding
separating_primary
separating_backing_vocals
mixing
encoding
packaging
completed
```

WebSocket `/v1/events` 复用多个 job。客户端发送 subscribe/unsubscribe；每条事件包含
`job_id`、单调 `sequence`、stage、可选 completed/total/fraction/ETA 和稳定英文
message。progress 不伪造：只有 runtime adapter 能取得真实 chunk 数时才报告单位
进度，否则只报告 stage。

WebSocket 事件允许丢失，不持久化完整历史。control process 对外限频，并在 SQLite
保存最新 progress snapshot；重连时 K3 先通过 HTTP 获取 snapshot，再订阅后续事件。
WebSocket 不可用时 K3 自动 HTTP polling。

取消通过 `POST /v1/jobs/{job_id}/cancel`。queued job 立即取消；running job 先使用
cancellation token 在 chunk/stage 检查点协作停止，超过 grace timeout 后终止独占
worker。用户取消不得触发 retry。

### 错误

HTTP 和 WebSocket 使用稳定 error code、安全英文 message、`retryable` 和受控
details。客户端不得解析 message。traceback 只进入 server log。

至少支持：`invalid_request`、`unauthorized`、`input_not_found`、
`input_not_ready`、`input_rejected`、`model_not_found`、`model_not_installed`、
`unsupported_preset`、`unsupported_layout`、`queue_full`、`job_not_found`、
`job_cancelled`、`runtime_unavailable`、`cuda_out_of_memory`、`worker_crashed`、
`execution_timeout`、`separation_failed`、`artifact_unavailable`、
`checksum_mismatch`、`insufficient_storage`。

server 只自动重试明确的 infrastructure failure，例如 worker crash、server restart 或
IPC 中断，并限制 `max_attempts`。坏音频、checkpoint mismatch、OOM、确定性模型异常、
磁盘不足和取消不自动重试。server 绝不静默降低 preset。

### Artifact

Completed job 返回 manifest，列出每个 immutable artifact 的 role、artifact ID、
media type、size 与 SHA-256。K3 分别下载 artifacts；`GET /v1/artifacts/{id}` 支持
HTTP Range。

首版统一输出 44.1 kHz、stereo、32-bit float WAV。server 和 K3 都验证：

- WAV 可解码；
- sample rate、channel count 与 sample format；
- 非零帧、finite samples；
- 所有 stems 帧数/时长对齐；
- 与 input 时长处于允许误差；
- size 与 SHA-256。

只有整套 artifacts 验证并原子发布后，job 才进入 `completed`。

Artifacts 内容寻址并可被多个 jobs/cache entries 共享。删除 job 只删除引用；无活跃
引用且超过 TTL 后由 GC 删除文件。input 和 artifacts 都支持可配置 TTL 与主动删除。

## Persistence 与调度

SQLite WAL 保存 inputs、jobs、attempts、progress snapshots、artifacts、引用、cache
entries 和 token hashes。本地文件系统布局：

```text
data/
├── separator.db
├── inputs/sha256/...
├── jobs/<job_id>/scratch/
└── artifacts/sha256/...
```

数据库只保存相对路径。文件完成写入、`fsync`、校验并原子 rename 后，数据库事务才
发布资源。

server restart 后 queued jobs 继续排队；running jobs 清理 scratch，增加 attempt 并
在 `max_attempts` 范围内从头重跑。SIGTERM 使 server 进入 draining：readiness 失败、
拒绝新 upload/job、停止 dispatch、等待 running jobs 到 grace timeout，剩余 worker
终止并在下次启动恢复。

用户 jobs 严格 FIFO。Benchmark 和维护任务只在空闲 maintenance lane 运行。

Scheduler 同时限制：

- 全局 `max_concurrent_jobs`；
- CUDA device memory 与 host memory；
- CPU/CoreML host memory 与 compute slots；
- scratch disk logical reservations。

`k3-separator-server benchmark` 按 model/backend/preset/layout 测量模型权重、每 job
activation、host memory、执行时间与 scratch 峰值。管理员加安全余量后显式应用结果；
生产 telemetry 不自动提高并发。未 benchmark 规格默认独占 backend。

Job hard timeout 按音频时长和 model/backend/preset 计算。磁盘 reservation 在 job
启动前覆盖中间 stems、最终 artifacts 与安全余量。

## K3 client 与项目状态

K3 使用显式 server profile，不做 mDNS 或自动负载均衡。profile 保存 URL、稳定
server ID 和 token 来源；token 不进入 project.json。连接时先检查 capabilities 与
server identity。

首版保持同步 I/O 与后台线程：HTTP 继续使用 `ureq`，WebSocket 使用单独 manager
thread，通过 channel 向 TUI 发送事件；不引入 Tokio。

现有阻塞式 `StemSeparator::separate()` 无法持久化 remote job reference，必须将
seam 改为可恢复 operation：

```text
start → persist operation reference → resume/complete
```

Project schema 提升版本，并将当前结果与正在执行的操作拆开：

```text
current_separation: Option<SeparationManifest>
separation_operation:
  idle
  remote_pending { server_id, input_id, job_id, resolved_spec }
  downloading { ... }
  failed { ... }
```

重新分离期间旧 manifest 和 stems 保持可播放。server job completed 但本地未下载完时，
project 仍不是新的 Ready。K3 将 partial artifacts 保存到项目 scratch，支持 Range
恢复；全部验证后才备份旧 stems、原子替换并保存新 manifest。

K3 retry 规则按 HTTP 操作语义区分：安全 GET 有限指数退避；input PUT 从头重传；
create job 使用同一 idempotency key；cancel 是幂等请求；认证、checksum 与确定性 4xx
不重试。停止本地等待不能隐式取消 server job。

最终 project provenance 必须自包含，不依赖可能过期的 server metadata，至少保存：

- server ID 与 job ID；
- input SHA-256；
- pipeline 版本与 output layout；
- 主/第二模型 identity、checkpoint、preset 与展开参数；
- backend、provider、device/precision 与 runtime versions；
- cache-hit 状态。

现有 schema 迁移：旧 NotRequested、Ready、Failed 分别映射到新 current/operation；旧
Running 因缺少 job ID 映射为 interrupted failure。迁移必须有 fixtures，并在写回前
保留可恢复备份。

## 安全

- worker 非 root 运行；
- 不使用 shell 拼接用户输入；
- 用户文件名只作 metadata，不作磁盘路径；
- 所有路径由 server 生成并做 containment 检查；
- checkpoint 必须预安装并校验；
- worker 不下载模型或访问外网；
- FFmpeg 只读取 server 管理的本地文件；
- 上传、解码、执行、磁盘、队列和 worker 数都有上限；
- 日志不记录 token、原始文件名、音频内容或 traceback 响应。

## 可观测性

server 提供结构化日志、liveness、readiness 与可选 Prometheus metrics。metrics 不使用
job ID、input ID 或文件名等高基数标签。至少覆盖队列/运行数、按 model/preset/backend
聚合的完成和失败、耗时、worker restart、模型加载时间、存储字节、资源 reservation
与 WebSocket connection 数。

## TDD 与验收

实现采用 vertical tracer bullets，不先批量写测试。最低测试矩阵：

1. fake runtime：HTTP、WebSocket、持久 queue、restart、cancel、cache、TTL 和 GC；
2. real CPU MDX：原始音频上传到最终 WAV 下载的端到端 smoke test；
3. CUDA：自托管/发布前测试与 benchmark；
4. local vs remote：固定 fixture 的 artifact 结构和音频质量 parity；
5. Rust client：断线、poll fallback、Range resume、project recovery 与原子替换；
6. protocol fixtures：Python 与 Rust 双向兼容和 unknown optional fields。

大型 checkpoints 不进入普通 CI。测试音频必须短、可再分发，并覆盖 stereo、非
44.1 kHz 与至少一种压缩格式。

## 实施与发布顺序

1. Contract、fake server 与 Rust client happy-path tracer bullet；
2. SQLite/filesystem persistence 与 restart recovery；
3. WebSocket progress、HTTP polling fallback 与 cancel；
4. real CPU worker；
5. CUDA worker、benchmark CLI 与 resource admission；
6. karaoke 两阶段 pipeline；
7. cache、TTL、GC、多设备 token 与 metrics；
8. legacy local-worker parity 与迁移；
9. remote preview → beta → default；
10. 经过一个迁移周期后删除 legacy adapter。

Remote 只有在现有模型类别、`two_stem`、`karaoke`、CPU/CUDA、provenance、原子重新
分离全部达到当前本地能力 parity 后才能成为默认。

具体 TTL、上传/时长限制、并发预算、取消 grace、job timeout 与磁盘安全余量由配置和
benchmark 决定，不写死在公开协议中。
