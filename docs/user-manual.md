# K3 用户手册

K3 是一个本地优先的 K 歌工作区。目前可创建歌曲工程、导入 LRC 歌词、使用本地 GPU
模型分离人声和伴奏，并在 Linux GUI 或 TUI 中播放、切换音轨和查看同步歌词。

本文以 Ubuntu 24.04、Windows WSL2 和 Windows 11 原生环境为当前支持环境。
Windows 原生版本支持播放、录音和 GPU/CPU 音轨分离。完整离线包覆盖 Windows、
Linux 与 macOS Apple Silicon，包含独立 Python、CPU 依赖、FFmpeg 和默认模型。
Intel macOS 提供明确标注的纯 CLI 包；macOS 音频设备尚未完成实机验证。
解压运行步骤见[离线发行包说明](offline-package.md)。

## 1. 系统要求

从源码构建和安装分离环境需要：

- Rust 1.99.0（仅从源码构建需要，版本由仓库的 `rust-toolchain.toml` 固定）；
- ALSA 开发库（Ubuntu/WSL 使用 `sudo apt install libasound2-dev`）；
- Python 3.10 或更高版本；
- `uv`；
- 足够存放模型和 WAV stem 的磁盘空间。

Linux GUI 还需要可用的 Wayland 或 X11 会话，以及支持 OpenGL ES 2.0 或更高版本的
图形驱动。GUI 不提供软件渲染 fallback；桌面后端不可用时可继续使用 TUI。
Ubuntu 24.04 的 GUI 运行库可通过
`sudo apt install libfontconfig1 libxkbcommon-x11-0 libgl1-mesa-dri` 安装。

从源码安装 Windows 原生分离环境需要 64 位 Python 3.11、PowerShell 5.1 或更高版本以及网络。
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
target/release/k3-gui
```

开发时也可以把后文的 `k3` 替换为：

```bash
cargo run -p k3 --
```

查看命令：

```bash
target/release/k3 --help
```

启动桌面界面：

```bash
target/release/k3-gui
```

首次启动只需选择已有 K3 工程所在目录。GUI 与 CLI/TUI 共享工程目录中的
`project.json`，但 GUI 的工程根目录、主音量、录音默认效果、默认分离档位与 Quality 模型、界面语言、
窗口尺寸和上次工程保存在独立的 `gui.json` 中，不复用 TUI 配置。
点击左上角 K3 标志右侧、信息图标左侧的齿轮（设置），可查看配置文件路径、更换工程目录并调整音量、默认分离档位和语言；
录音效果仍使用播放区底部唯一的 `Effect` 下拉框。设置自动保存，无需手改 JSON。
语言可在简体中文、English 和繁體中文之间即时切换，旧版 `gui.json` 默认使用英文。
GUI 再次启动时会加载上次工程，但始终从头保持暂停。
顶层信息按钮会打开 About 界面，其中显示 GUI 滚动诊断日志的位置。音频设备、媒体读取和
工程错误会写入该日志；GUI 无法初始化显示后端时，终端诊断也会给出日志路径和 `k3 tui`
回退命令。Windows 双击 `k3-gui.exe` 不会附带命令行窗口；启动失败时会显示含诊断日志路径的
错误弹窗。GUI 后台分离也不会弹出命令行窗口。维护者的平台与性能验收步骤见
[GUI 验收记录](https://github.com/coanor/k3/blob/main/docs/gui-acceptance.md)。

要在 GUI 中加入新歌，点击右侧 `Separate song`，可一次选择多首本地音频，选择 Fast、Balanced、
Quality 或 Compatible 档位，再点击 `Queue selected`。首次默认使用 Quality，之后记住
上次选择的档位；所选歌曲按顺序进入
分离队列，也可以在分离期间继续选歌追加。只选一首时按钮显示 `Create and separate`。
选择 Quality 时可进一步选择 `Runtime default`、`Kim Vocal 2` 或 `BS RoFormer 1297`。
`Runtime default` 沿用分离运行包 `config.json` 指定的模型；未指定时使用 worker 的
Quality 默认模型。新选项会随 GUI 设置自动保存，入队时固定到每首任务；
更改后要对已有工程点击 `Replace stems` 才会生成新分轨。不同模型可能对特定乐器有不同
误分，但不能保证某一款对所有歌曲更好；首次使用新模型可能需要下载权重。
分离时，备歌页和左侧工程列表下方都会显示当前歌曲、处理阶段、已用时和等待歌曲数。
主唱分离与和声分离显示各自的真实阶段百分比，切换阶段时百分比重新开始；加载模型、
写入音轨和保存工程时显示活动条。这里显示的是当前阶段进度，并非整首歌的预计完成百分比。
按 `Esc` 可从备歌页返回歌词；后台下载与分离任务继续执行，左侧进度卡仍保持可见。
点击左侧进度卡可重新打开备歌页。
每首分离完成后左侧工程库会自动刷新。此功能调用发行包中的 `separate.sh`（Linux）或 `separate.ps1`
（Windows），需要先按第 3 节安装分离 worker；首次使用所选模型时可能下载权重。
若批量选择中有多个文件映射到同名工程，GUI 会跳过重复项。批量选择也可包含已有工程，
界面会显示替换警告和 `Queue & replace` 按钮；只选一首已有工程时按钮为 `Replace stems`。
替换使用该工程已保存的源音频，
不会重新导入所选文件，也不会删除歌词和 take。若当前已载入该工程，替换前会停止并释放音频；
该首分离结束后自动重新载入，保持暂停；若期间已切到其他工程，则保留当前选择。
分离期间可切换、播放和录制其他已有工程；正在重分离的工程显示“分离中”，完成前暂不能打开或录制。
队列会继续处理其他工程；若下一首要替换正在录音或渲染 take 的工程，会等待保存结束再继续。
运行期间暂不能切换工程目录。失败时可在 About 中找到
GUI 诊断日志；若只复制 `k3-gui` 二进制而未保留同包的分离脚本，界面无法启动分轨。

右侧 `Separate song` 下方也提供网易云音乐实验性来源。首次使用须阅读提示并点击
`Enable experimental NetEase source`；该来源使用未公开网页接口，可能随时失效，
仅应下载账号有权访问的歌曲。可从 Chrome 导入已有登录，或点击 `QR login` 后用网易云音乐
手机 App 扫码确认。GUI 与 TUI 使用同一份本地会话；登录凭据不会写入工程。登录后可搜索歌曲
或打开 `Liked songs`，点击多首歌曲前的方框，再点 `Queue selected songs`。下载期间仍可
继续搜索、选择其他歌曲并追加到队列；同一首歌不会重复入队。下载任务依次执行，每首下载
完成后进入逐首执行的分离队列，下一首下载可与当前分离同时进行；
下载的音频保存在工程根目录的 `.netease-audio/NetEase`。搜索结果中，待排队的勾为珊瑚色、
已入队的勾为黄色、已下载的勾为绿色；
`DOWNLOADED` 只表示本地音频存在，不表示分离成功。同名工程已存在且分离成功时不会覆盖工程或
自动改用新下载的音频；若工程尚未分离成功、保存的音源与缓存下载完全一致，
再次加入队列会重试分离。`Replace stems` 仍使用该工程保存的旧音源；若要用
下载的新音频创建工程，须先移走同名旧工程。不可用歌曲或会话失效会在右栏显示原因；
会话失效时下载队列暂停，重新登录后继续。关闭窗口会取消仍在进行的下载。
网易云状态、分轨状态及其他主要错误提示旁的 `Copy` 按钮可将完整文字复制到剪贴板，
便于反馈被界面截断的错误。

GUI 打开具有伴奏音轨的工程后，可点击底部 Play 左侧的 Record 圆形图标，或按 `r`，从头播放伴奏并使用
系统默认麦克风录音。开始录音时会默认开启实时麦克风监听；点击 Play 右侧的 Monitor 耳机图标或按 `m`
可随时关闭或重新开启，建议佩戴耳机以避免扬声器回授。再次点击变为 Stop 的录音图标或按 `r` 会停止并保存录音。录音写入
`takes/take-<时间>-dry.wav`，同时更新 `project.json`；保存后 GUI 会重新加载工程，点击
`Take` 或按 `4` 可立即试听当前所选 take 的 mix；若 mix 不存在则回退到 dry。
如果提前结束录音，mix 中的伴奏仍会播放到歌曲结束。
底部唯一的 `Effect` 下拉框可选择 `Clean`、`Studio`、`KTV`、`Theater` 或 `Church`。
选中后立即写入 GUI 的 `gui.json` 中的 `recording.default_effect`，之后开始的 GUI 录音会使用该效果；
如果当前有 take，K3 同时在后台用原始 dry 重建它的 mix，更新 `project.json` 中该 take 的效果并播放。
停止新录音时也会用选定效果渲染 mix，原始 dry 不会被覆盖。
`Take` 下拉框可选择任何已有录音。切换 take 时，K3 会在需要时为新选中的 take
应用同一效果并播放。渲染期间不能切换 take 或开始录音。

停止录音后默认自动保存，无需再确认。要删除录音，先在 `Take` 下拉框选中它，再点击旁边的
`⋯` 录音操作菜单，选择“删除所选录音”；也可以按 `Delete`。确认框会显示录音 ID，默认按钮是
“保留录音”，按 `Enter` 或 `Esc` 均保留。明确点击“删除录音”后，K3 会释放播放占用，
删除这条 take 的 dry、mix 文件及工程记录，并选中相邻的剩余录音；删除最后一条后可继续录新 take。
其他录音、歌曲、分轨、歌词与导出文件保持不变；其他 take 共用的音频文件也会保留。

录音期间仍可调整进度、暂停或继续、切换音轨、升降调、调整音量和切换监听；工程切换会暂时禁用。若关闭窗口，
正在进行的录音会先停止并自动保存，保存和混音完成后程序退出。伴奏、默认输入设备或输出设备不可用时，GUI 会在
错误条和诊断日志中显示原因。

打开工程后，点击歌词区右上角的 `Find lyrics` 或按 `l` 可打开在线歌词搜索。输入歌名或
“歌名 - 歌手”后，GUI 会依次查询 LRCLIB 与网易云音乐，并只展示包含同步时间戳且时长
匹配的候选。点击候选可预览前三行；确认后点击 `Use this version`，歌词会原子写入工程并
立即重新加载。搜索、取消、无匹配或保存失败都不会覆盖当前歌词，录音期间不能启动搜索。

### 2.1 Makefile

仓库根目录提供统一的常用构建入口。Ubuntu/WSL 若尚未安装 GNU Make，先运行
`sudo apt install make`：

```bash
make help
make check
make build
make dist
```

`make dist` 根据当前主机生成 Linux x86_64/ARM64 或对应架构的 macOS 包。也可明确指定：

```bash
make dist-linux
make dist-windows  # Windows 原生环境
make dist-windows-aarch64-cli  # Windows ARM64 原生精简包
make dist-windows-cli  # Linux/WSL 交叉构建纯 CLI
make dist-macos
```

完整包在本机通过 uv 准备独立 Python、CPU 依赖和默认模型，并在打包后执行真实
分离检查。已有模型可用 `MODEL_CACHE=/path/to/models` 复用。
`dist-windows-cli` 在 Linux/WSL 中使用 `cargo-xwin`；缺少工具时先运行
`cargo install cargo-xwin`。如果 `llvm-lib` 不在 `PATH`，通过
`make dist-windows-cli LLVM_BIN=/path/to/llvm/bin` 指定。macOS 目标必须在 macOS
主机或 GitHub macOS runner 上运行，因为构建需要 Apple SDK。

要从本机触发 GitHub 上的全部平台构建，先安装并登录 GitHub CLI，然后运行：

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
- `k3-linux-aarch64.tar.gz`；
- `k3-windows-x86_64.zip`；
- `k3-windows-aarch64-cli.zip`；
- `k3-macos-x86_64-cli.tar.gz`；
- `k3-macos-aarch64.tar.gz`。

手动运行时，文件保存在该 workflow run 的 Artifacts 中 14 天，直接以原文件名列出。
Windows 用户点击 `k3-windows-x86_64.zip` 或 `k3-windows-aarch64-cli.zip` 下载，
解压一次即可看到包含程序的目录；校验文件需单独下载对应的 `.sha256`。
安装包也直接下载为 `.exe`、`.deb` 或 `.pkg`，无需先解压外层 ZIP。
旧运行中以 `dist-` 开头的 artifact 仍有外层 ZIP，请使用新运行的产物。
推送版本 tag 时，
workflow 会创建或更新同名 GitHub Release，并附加各平台包、各自的 `.sha256` 文件
和汇总的 `SHA256SUMS`。例如：

```bash
git tag v0.1.0
git push origin v0.1.0
```

Windows 包使用静态 MSVC C runtime。ARM64 Windows 包仅包含原生 CLI/TUI；
精简包不捆绑 GUI 或离线 runtime；完整离线 runtime 所需的当前 PyTorch 依赖
缺少原生 Windows ARM64 wheel。完整包统一包含原生 worker 启动器、独立
Python、CPU 分离依赖、FFmpeg、默认 Fast / Balanced / Quality 模型及所需配置。
Windows 额外包含 `separate.ps1` 和 `install-separator.ps1`，便于安装可选 GPU 环境。
Intel macOS 仅提供 `-cli` 包，因为新版官方 PyTorch 不支持该平台。所有包都先解压
到新目录验证启动；完整包还会禁止网络加载全部随包模型，并实际分离短音频。

Linux 与 Windows 完整包还包含 GUI（`k3-gui` / `k3-gui.exe`）；Linux 同时包含桌面入口、图标和 `separate.sh`。macOS 包目前只包含 CLI/TUI。

Linux 包不执行需要 root 权限的安装器。需要在桌面启动器中显示 K3 时，可从解压后的
包目录手动安装用户级入口；整个包目录需保留在固定位置：

```bash
mkdir -p ~/.local/bin
ln -s "$(pwd)/k3-gui" ~/.local/bin/k3-gui
install -Dm644 share/applications/k3.desktop ~/.local/share/applications/k3.desktop
install -Dm644 share/icons/hicolor/scalable/apps/k3.svg \
  ~/.local/share/icons/hicolor/scalable/apps/k3.svg
```

桌面文件通过 `PATH` 查找 `k3-gui`。如果 `~/.local/bin` 尚未在 `PATH` 中，请先将其加入
登录环境，或把二进制安装到已有的用户级 `PATH` 目录。

### 2.3 私有源码仓库发布公开二进制

私有仓库中的 GitHub Release 只有获准访问仓库的人能下载。若要让所有人下载发行包、
同时保持 `coanor/k3` 私有，可另建一个只有说明文件的公开仓库（例如
`coanor/k3-binaries`），将四个平台的发行包上传到该仓库的 Release。公开仓库自动生成的
源码压缩包只包含公开仓库自身的内容，不包含私有 `k3` 源码。

1. 创建公开仓库，例如 `gh repo create coanor/k3-binaries --public --add-readme`。
2. 创建仅授权该公开仓库、具有 **Contents: Read and write** 权限的 GitHub fine-grained
   personal access token。按 [GitHub 官方说明](https://docs.github.com/en/rest/releases/releases)
   配置令牌的到期时间和仓库范围；私有仓库 workflow 自带的 `GITHUB_TOKEN` 不能写入另一仓库。
3. 在私有 `coanor/k3` 仓库设置 Actions 变量 `PUBLIC_RELEASE_REPO=coanor/k3-binaries`，
   并将令牌保存为 Actions secret `PUBLIC_RELEASE_TOKEN`。可使用 `gh variable set` 和
   `gh secret set`，也可在仓库 Settings → Secrets and variables → Actions 中配置。
4. 将待发布的版本合并到主分支、推送新的 `v*` tag。`Build distributions` 会先测试、
   构建并校验发行包，然后保留原有私有 Release，同时向公开仓库发布同名 Release。
   公开 Release 若已存在，任务会报错，不会悄悄覆盖已发布的文件。

未设置 `PUBLIC_RELEASE_REPO` 时，公开发布任务会跳过。发行包包含 README、文档、
分离脚本、独立 Python、运行依赖、模型及组件许可证清单；公开发布前应检查这些内容。仓库目前尚未
配置公开发行目标及令牌，因此只完成了发布流程，尚未公开任何发行包。

## 3. 从源码安装本地分离 worker

使用完整离线发行包时可跳过本节，默认 worker 与模型已随包提供。需要 GPU 加速
或维护源码环境时，再按以下说明安装独立环境。

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

老旧 CPU 建议使用 `fast`、`uvr-mdx-karaoke-2`，省略 `segment_size` 并关闭
autocast。该 MDX 模型的原生分块大小是 `256`；覆盖为 `64` 或 `128` 会把 ONNX
模型转换为 PyTorch 执行，不但更慢，还可能在不支持较新指令集的 CPU 上导致 worker
异常退出。CPU 分离可能明显慢于歌曲时长，不建议运行 quality RoFormer。

把 `separate.sh` 和运行包复制到另一台 Linux 机器时，最低要求是 x86-64
Linux、Bash 4（脚本使用关联数组）、`realpath`、FFmpeg、可执行的 K3 二进制，
以及已安装依赖的 Python worker。安装 worker 还需要 `uv` 和网络；运行时不需要
Rust 工具链。当前 Arch 老机器的 i7-2620M（AVX、4 线程、约 10 GiB 内存）已
通过 PyTorch 2.11 CPU worker 健康检查，可作为目前验证过的硬件下限；这不是
对更老 CPU 的兼容保证。

### 3.1 Windows 原生安装

Windows 完整发行包默认自带 CPU worker；以下步骤用于额外安装 GPU 或独立
Python 环境。在 PowerShell 中进入解压目录，临时允许本次会话执行本地脚本，再安装
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
`config.json`。默认模型目录为发行包内的 `models`，分离日志位于 `logs`，安装缓存位于
`.cache`；可在运行安装脚本前用 `K3_MODEL_DIR`、`K3_LOG_DIR` 和 `PIP_CACHE_DIR`
指定其他盘符。模型在第一次分离时下载。检查 worker：

```powershell
$config = Get-Content .\config.json -Raw | ConvertFrom-Json
'{"id":"health","method":"health"}' |
  .\.venv-separator\Scripts\k3-separator.exe `
    --model-dir $config.separation.model_dir
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
`K3_AUTOCAST` 和 `K3_PRESERVE_BACKING_VOCALS` 可临时覆盖配置。
当 `K3_PROFILE` 与配置中的 `separation.profile` 不同时，脚本使用新档位的默认模型和
分块大小，不沿用原档位专用的 `separation.model` 与 `separation.segment_size`；
仍可用 `K3_MODEL`、`K3_SEGMENT_SIZE` 显式指定。默认保留和声，因此输出包含版本化的
`vocals-<id>.wav`、`backing-vocals-<id>.wav` 和含和声的
`accompaniment-<id>.wav`。脚本会从 `project.json` 的 separation manifest 输出本次
实际文件路径。目标 project 已存在时，再次执行同一条命令会按当前配置覆盖 stem，
但保留歌词、take、效果和其他 project 文件。

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
| `separation.segment_size` | `K3_SEGMENT_SIZE` | 可选的推理分块大小覆盖；通常应省略并使用模型原生值 |
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

### 3.3 网易云音乐实验性来源

网易云音乐登录、搜索和音频下载依赖未公开网页接口，没有稳定性或兼容性承诺，默认
关闭。只有接受这一风险并确认自己的下载行为符合账号权限及当地规则时，才在媒体库配置
中显式启用：

```json
{
  "netease": {
    "enabled": true
  }
}
```

启用后，把焦点切到右栏并按 `n`，可在 `Local Music` 与 `NetEase` 间切换。首次进入
会显示一次英文风险提示。接受后，推荐先在本机 Chrome 登录 `music.163.com`，再回到 K3
按 `c` 导入会话；登录成功后会自动加载“我喜欢的音乐”。K3 会按 Chrome 最近使用顺序
检查各个 Profile，并使用第一个经网易云验证成功的会话。

如果 Chrome 导入不可用，按 `i` 生成二维码，用网易云音乐手机客户端扫描并确认。二维码
直接显示在终端内，过期后按 `r` 刷新；二维码弹窗内也可以按 `c` 改用 Chrome。

网易云来源快捷键：

| 快捷键 | 作用 |
|---|---|
| `n` | 切换本地音乐与网易云来源 |
| `c` | 未登录时从本机 Chrome 导入登录（推荐） |
| `i` | 未登录时开始二维码登录 |
| `l` | 加载“我喜欢的音乐” |
| `/` | 搜索单曲 |
| `↑` / `↓` | 移动选择 |
| `←` / `→` | 切换结果页 |
| `Space` | 勾选或取消当前歌曲 |
| `a` | 勾选当前页 |
| `A` | 勾选全部喜欢歌曲 |
| `Enter` | 确认并开始下载所选歌曲 |
| `x` | 退出网易云账号并删除本地凭据 |

下载一次只处理一首歌。每首新下载成功的歌曲会自动进入 project 创建队列；全部下载结束
后，K3 使用当前 `separation` 配置依次创建 project 并分离人声与伴奏。临时网络错误会
有限重试；账号会话失效时队列暂停，重新从 Chrome 导入或扫码后继续。不可下载歌曲会
跳过并计入结束摘要。下载期间仍可使用 TUI，但退出 K3 会要求确认取消队列。
已有歌曲升级到更高音质时，K3 会更新原 project 内复制的音源并重新生成 stems，同时保留
project ID、歌词、takes 和其他用户文件；若更新失败，原 project 音源和 stems 保持不变。

K3 自动尝试账号当前可用的最高音质，并在状态信息中显示实际音质。文件保存到
`music_root/NetEase/`，使用平铺命名：

```text
<主歌手>-<歌名>.<扩展名>
```

文件包含歌名、完整歌手列表、专辑、网易云歌曲 ID 和封面标签。下载索引按歌曲 ID 去重；
已有同等或更高音质时跳过，获得更高音质后可安全升级。取消喜欢不会删除本地文件。
下载完成后右栏自动刷新，并在全部下载结束后启动上述 project 创建与分离队列。分离失败
不会删除已下载音频。对于启用自动 project 创建之前已经下载的歌曲，按 `n` 切回
`Local Music`，选中歌曲后按 `Enter`，即可手动创建 project 并开始分离。

每个操作系统用户只保存一个网易云账号。凭据位于系统标准 K3 配置目录中的
`netease-session.json`：Linux 使用 `$XDG_CONFIG_HOME/k3/`（默认
`~/.config/k3/`），Windows 使用 `%APPDATA%\k3\`，macOS 使用
`~/Library/Application Support/k3/`。Unix 文件权限为 `0600`。这是可直接使用的
Cookie 文件；它不会写入日志，但不能抵御已经可以读取当前用户文件的恶意程序。

Chrome 导入只会在你按 `c` 时读取 `music.163.com` 的 Cookie，并只把 `MUSIC_U` 与可选
的 `__csrf` 复制到上述 K3 会话文件；不会扫描或导出其他站点，也不会修改 Chrome 数据。
按 `x` 只删除 K3 的副本，Chrome 仍保持登录。macOS 可能询问钥匙串权限；Linux 需要
桌面钥匙串服务处于解锁状态；现代 Windows Chrome 的 App-Bound 加密可能要求以管理员
权限运行，部分设备绑定会话仍可能无法导入。遇到这些情况可继续使用二维码登录。

完整边界、失败语义和验收标准见
[网易云音乐实验性集成规格](https://github.com/coanor/k3/blob/main/docs/netease-music-integration-spec.md)。

### 3.4 候选状态图标（待选）

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
stems/vocals-<id>.wav
stems/accompaniment-<id>.wav
```

启用保留和声模式时还会生成：

```text
stems/vocals-<id>.wav          # 主唱
stems/backing-vocals-<id>.wav  # 单独和声
stems/accompaniment-<id>.wav   # 纯伴奏加回和声
```

`<id>` 是每次成功分离生成的唯一版本标识。请以 `project.json` 的 separation manifest
和包装脚本成功后打印的路径为准；重新分离成功后，旧版本会被新 manifest 原子替换。

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
[歌词来源调研](https://github.com/coanor/k3/blob/main/docs/lyrics-sources-research.md)。

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

### 删除已保存录音

GUI 与 TUI 停止录音后均自动保存，之后可以删除当前选中的 take。CLI 也提供独立命令：

```bash
# 默认针对最新录音；提示时直接回车会保留
k3 delete-take --project ./songs/example

# 删除指定旧录音；仍需在提示中输入 y
k3 delete-take --project ./songs/example --take take-1786272214377

# 明确确认删除最新录音，供脚本使用
k3 delete-take --project ./songs/example --take latest --yes
```

删除会同时更新 `project.json` 并清理该 take 的原始干声与混音。保存工程失败时会恢复暂存的音频文件；
若确认期间工程被其他程序修改，操作会拒绝删除，重新打开工程后再选择即可。删除后不提供撤销。

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
- `4`：播放当前选择的 take mix；
- `Delete`：请求删除当前选择的 take，确认框中按 `y` 才删除，`Enter`、`Esc`、`n` 或 `q` 均保留。

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
[`python/separator/README.md`](https://github.com/coanor/k3/blob/main/python/separator/README.md)。PyTorch `.ckpt`
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

worker 默认不会在已有 stem 时开始新的分离。确认现有结果可替换且
工程仍为 `not requested` 后使用 `--overwrite`。

### 工程显示 `failed`

失败信息会保存进 `project.json`。修复环境后，可重新运行分离脚本对已有工程
重新分离；GUI 网易云来源在缓存音频与工程保存的音源完全一致时，也可重新加入队列重试。

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
