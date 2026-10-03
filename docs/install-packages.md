# 安装 K3

## 在线安装（默认方式）

Linux/macOS 使用发行版中的 `install.sh`，Windows 使用 `install.ps1`。
安装器分别下载当前平台的 CLI、GUI（平台提供时）及分离启动器，再准备独立 Python、
CPU 分离依赖、FFmpeg 和 Fast / Balanced / Quality 模型，无需预先安装 Rust、Python 或 uv。
程序文件与模型分开下载，不需要先下载大型完整 ZIP。

| 平台 | 程序与分离支持 |
| --- | --- |
| Linux x86_64 / ARM64 | GUI、CLI/TUI、CPU 分离环境；glibc ≥ 2.39 |
| Windows x64 | GUI、CLI/TUI、CPU 分离环境；Windows 10/11 |
| Windows ARM64 | 原生 CLI/TUI；Windows 11；目前无 GUI 和完整分离依赖 |
| macOS Apple Silicon | CLI/TUI、CPU 分离环境；macOS ≥ 14；目前无 GUI |
| macOS Intel | 原生 CLI/TUI；macOS ≥ 14；目前无 GUI 和完整分离依赖 |

从 Release 下载脚本及对应 `.sha256`，校验后运行。以下示例使用私有源码仓库 `coanor/k3`；
公开发行时将 `--repo` / `-Repo` 改为实际配置的公开二进制仓库。私有仓库需要先在进程环境中
设置具有该仓库读取权限的 `GITHUB_TOKEN`，安装器通过 GitHub API 下载，不在命令参数中传递令牌。
脚本默认安装最新正式发行版，也可用 `--version v0.1.0` / `-Version v0.1.0` 固定版本。
此功能需要发布包含在线安装组件的新 Release；历史完整包 Release 不含这些组件。

Linux/macOS：

```bash
# Linux 使用 sha256sum；macOS 使用 shasum -a 256
sha256sum --check install.sh.sha256
bash install.sh --repo coanor/k3
# 可直接指定挂载盘上的目录，仍然会要求确认
bash install.sh --repo coanor/k3 --prefix /mnt/data/K3
```

Windows PowerShell：

```powershell
# 将输出与 install.ps1.sha256 中的摘要比较
Get-FileHash .\install.ps1 -Algorithm SHA256
powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -Repo coanor/k3
# 可直接指定磁盘，仍然会要求确认
powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -Repo coanor/k3 -InstallDir 'D:\Apps\K3'
```

Windows 会列出磁盘及剩余空间，先选择磁盘，再填写安装目录。Linux/macOS 会列出挂载点，
让你输入相应磁盘上的路径。完整安装目前估计下载约 1–2 GiB、安装后约 2–4 GiB，
要求至少预留 8 GiB 峰值空间；CLI 精简安装预留 512 MiB。实际大小随平台和依赖变化。
确认默认是“否”；拒绝时不会下载组件。`--yes` / `-Yes` 只用于自动化，并且必须同时明确指定安装目录。

模型、独立 Python、下载缓存及临时文件均写在所选磁盘；完成或失败后清理安装器临时目录。
每个程序均校验平台清单中的大小与 SHA-256；uv 固定为 0.12.13 并使用脚本内置摘要。
完整安装检查 CLI 启动、worker 健康、FFmpeg/模型文件摘要，并禁止联网加载全部默认模型。
只有检查成功才启用最终目录；已有非空目录会被拒绝，避免覆盖用户文件。更新时请选择新的空目录。

安装后在所选目录运行 `k3` / `k3.exe`，GUI 平台运行 `k3-gui` / `k3-gui.exe`。
安装器不修改全局 PATH 或系统应用登记。将歌曲工程、媒体库与录音保存到安装目录之外；
卸载时仅删除确认属于 K3 的安装目录，保留个人数据目录。

Linux 还需要系统图形/音频库。Ubuntu 24.04 示例：

```bash
sudo apt install libasound2t64 libfontconfig1 libxkbcommon-x11-0 libegl1 libgl1-mesa-dri
```

无 X11 键盘库时安装器会提示；Wayland 和 CLI 可继续使用。播放/录音需要可用的音频设备，
GUI 需要图形会话。M1/M2/M3/M4 及后续 ARM64 Apple Silicon 共用 macOS ARM64 程序。

### 本地准备与验证在线发行

```bash
make dist
# dist/online/support：两个入口、支持 ZIP 与校验文件
# dist/online/platform：本机独立原生程序、清单与校验文件
bash install.sh --source-dir "$PWD/dist/online" --prefix /mnt/data/K3-check --yes
```

Windows 原生构建后的本地验证使用 `-SourceDir` 指向同样的发行目录。
`--model-cache` / `-ModelCache` 可复用现有模型，仍会校验固定模型摘要及完整性。
`--source-dir` / `-SourceDir` 仅替换 K3 发行文件来源；uv、Python 和第三方依赖仍从官方源准备。
共享支持文件仅包含安装逻辑、worker 源码、许可证和三份用户手册，不包含 Python 环境或模型。

GitHub `Build distributions` 默认生成这六种平台的在线组件，并在原生 runner 上运行安装验证；
Release 附加两个入口、共享支持文件、独立原生程序、平台清单及 SHA-256。
需要额外完整离线归档和系统安装器时，在手动构建中勾选 `offline_bundle`。

## 可选的完整离线系统安装包

安装包复用同平台便携包中的程序、Python、FFmpeg 和模型，不需要额外安装 Rust 或 Python。
下载与你的系统和架构相符的安装包及 `.sha256` 文件，校验后安装。
GitHub Actions 的 Artifacts 按原文件名列出安装包和校验文件，分别点击下载即可；
Windows 安装包下载后直接运行 `.exe`，无需解压。便携 ZIP 解压一次即可使用。

| 平台 | 安装包 | 内容与要求 |
| --- | --- | --- |
| Windows x86_64 | `k3-版本-windows-x86_64-setup.exe` | Windows 10/11；GUI、CLI 和完整离线分离环境 |
| Windows ARM64 | `k3-版本-windows-aarch64-cli-setup.exe` | Windows 11 ARM64；原生 CLI/TUI 精简包，不含 GUI 和离线分离环境 |
| Linux x86_64 | `k3_版本_amd64.deb` | Ubuntu 24.04 或兼容系统；GUI、CLI 和完整离线分离环境；glibc ≥ 2.39 |
| Linux ARM64 | `k3_版本_arm64.deb` | Ubuntu 24.04 ARM64 或兼容系统；GUI、CLI 和完整离线分离环境；glibc ≥ 2.39 |
| macOS Apple Silicon | `k3-版本-macos-aarch64.pkg` | macOS 14 或更高；CLI/TUI 和完整离线分离环境 |
| macOS Intel | `k3-版本-macos-x86_64-cli.pkg` | macOS 14 或更高；仅 CLI/TUI，需要另行配置分离 worker |

Windows 与 macOS 安装包目前没有代码签名，macOS 包也没有公证，系统可能要求手动确认来源。
正式分发前应使用发行者证书签名并完成 macOS 公证。

## Windows

运行 `.exe` 安装向导，完整包默认安装到当前用户的 `%LOCALAPPDATA%\Programs\K3`，
ARM64 精简包安装到 `%LOCALAPPDATA%\Programs\K3-ARM64-CLI`，无需管理员权限。
两种包使用独立安装目录和卸载记录，可共存。
x86_64 完整包在开始菜单中提供 K3 和卸载入口，可选创建桌面快捷方式；安装完成后启动 K3 GUI。
ARM64 精简包提供卸载入口，CLI/TUI 从 PowerShell 中启动。
CLI 未自动加入 PATH，可在 PowerShell 中使用完整路径：

```powershell
& "$env:LOCALAPPDATA\Programs\K3\k3.exe" --help
```

ARM64 精简包的命令路径为：

```powershell
& "$env:LOCALAPPDATA\Programs\K3-ARM64-CLI\k3.exe" --help
```

可通过 Windows 设置中的应用列表或开始菜单卸载。工程、媒体库和录音应保存在个人数据目录，
不要放在安装目录内；卸载器只删除登记的程序文件，保留用户额外创建的文件。

Windows ARM64 暂不提供完整离线包：固定的 PyTorch 2.11.0、torchvision 0.26.0、
torchaudio 2.11.0 在官方发布源中缺少与 Python 3.13 匹配的原生 Windows ARM64 wheel。
这是离线分离环境的依赖限制，原生 CLI/TUI 包不包含这些 Python 依赖。
参见 [PyTorch 发布文件](https://pypi.org/project/torch/2.11.0/#files)与
[官方 CPU wheel 索引](https://download.pytorch.org/whl/cpu/torch/)。

## Linux

按 `uname -m` 选择架构：`x86_64` 使用 `amd64.deb`，`aarch64` 使用 `arm64.deb`。
以下示例用于 x86_64，ARM64 将文件名中的 `amd64` 替换为 `arm64`。

```bash
sha256sum --check k3_0.1.0_amd64.deb.sha256
sudo apt install ./k3_0.1.0_amd64.deb
k3 --help
k3-gui
```

程序安装到 `/opt/k3`，命令入口位于 `/usr/bin`，桌面菜单中提供 K3 图标。
APT 自动补齐 ALSA、Fontconfig、键盘与 EGL 系统库；离线安装前需要已装好这些系统依赖。
默认模型可在只读安装目录中使用。工程输出写入你指定的个人目录。
下载额外模型时用 `--model-dir` 指定可写目录，并将默认模型复制到该目录后再使用各档位。

```bash
sudo apt remove k3
```

卸载只删除包管理器登记的文件，个人目录中的工程、配置与录音保留。

## macOS

M1、M2、M3、M4 及后续采用 ARM64 的 Apple Silicon 芯片共用 `macos-aarch64` 包，
包括 Pro、Max 和 Ultra 型号。它们不需要分别打包；要求 macOS 14 或更高。
CI 在 Apple Silicon runner 验证，不代表逐代芯片都经过实机测试。

双击对应架构的 `.pkg`，或在终端安装：

```bash
sudo installer -pkg k3-0.1.0-macos-aarch64.pkg -target /
/usr/local/bin/k3 --help
```

程序安装到 `/usr/local/lib/k3`，命令入口位于 `/usr/local/bin`，通过脚本执行包内真实路径，
确保分离启动器能找到同包 Python；默认模型在安装目录内。
如果 `/usr/local/bin` 不在你的 PATH 中，请使用完整路径或将其加入 PATH。
macOS 包提供 CLI/TUI；Intel 包不包含 Python、模型和分离启动器。
额外模型同样需要指定个人可写的 `--model-dir`。

macOS 的 `.pkg` 没有自动卸载入口。可用 `pkgutil --files io.github.coanor.k3` 查看登记文件，
逐项删除确认属于 K3 的文件及命令入口后，再执行 `sudo pkgutil --forget io.github.coanor.k3`。
`--forget` 只移除安装记录，不会删除文件。不要删除个人工程目录。

## 构建与检查

先在目标平台构建并验证便携包，然后执行：

```bash
python3 scripts/build-installer.py dist/k3-linux-x86_64.tar.gz
```

脚本校验输入摘要、平台、布局和程序版本。Linux 使用 `dpkg-deb`，macOS 使用系统自带
`pkgbuild`/`productbuild`；Windows 需安装 Inno Setup 6.5 或更高并将 `ISCC.exe` 加入 PATH，
或使用 `--iscc` 指定路径。产物及校验文件默认写到 `dist/installers/`。
也可用 `make installer` 从头构建当前平台的便携包与安装包。

各平台 CI 实际安装包后，以普通用户执行程序启动、worker 健康检查、禁止网络的默认模型
加载及真实短音频分离。Intel macOS 与 Windows ARM64 精简包只检查 CLI 启动。Windows 与 Linux 还验证卸载保留
安装目录中额外创建的文件。CI 只在临时 runner 安装，不修改开发者机器。

复用已有 CI 中构建成功的便携包，可避免重新编译或下载模型：

```bash
gh workflow run dist.yml --ref build-package -f installer_source_run=37103318484
```

输入必须指向本仓库的 `Build distributions` 运行，其中六个便携包构建 job 必须全部成功，
并包含全部六种平台归档。仅安装器失败的运行也可复用，因此修复安装脚本后无需重编译程序或下载模型。
CI 同时兼容旧运行的 `dist-` 外层 ZIP 和新运行的直接文件；复用后会在当前运行重新上传
可直接下载的便携包及校验文件，旧运行已经上传的文件不会改变。
复用时，便携包和安装包都会换成当前源码的三份用户说明，并移除旧开发文档。
CI 会重新压缩便携包并生成新校验文件；程序、第三方依赖和模型仍使用来源运行的版本。
新增 ARM64 支持之前仅含四种归档的运行无法用于完整安装矩阵复用。通常的手动构建和 tag 构建会同时
生成便携包与安装包，安装检查通过后才上传安装包。本文中的版本和运行 ID 仅为示例。

复用运行时，CI 使用 uv 从当前源码重新安装 Python worker，不重新构建原生程序、
其他 Python 依赖或模型。本机复用旧便携包时同样应加 `--refresh-worker`。
这样可包含安装目录只读时的修复：模型仍在系统目录，转换临时文件写入本次工程的临时目录。

Windows 中文安装器翻译取自 [Inno Setup 官方资源](https://github.com/jrsoftware/issrc/blob/6ef32198ef1f7b7b375cd4b6b90896c2a58eb4c2/Files/Languages/ChineseSimplified.isl)，
按译者项目的 MIT 许可证保留原始头部、完整文本及 `ChineseSimplified-LICENSE.txt`，随构建脚本提供，不依赖编译器预装中文语言包。
