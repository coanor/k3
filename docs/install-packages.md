# 系统安装包

安装包复用同平台便携包中的程序、Python、FFmpeg 和模型，不需要额外安装 Rust 或 Python。
下载与你的系统和架构相符的安装包及 `.sha256` 文件，校验后安装。

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

复用已有成功 CI 的便携包，可避免重新编译或下载模型：

```bash
gh workflow run dist.yml --ref build-package -f installer_source_run=37103318484
```

输入必须指向本仓库成功的 `Build distributions` 运行，并包含全部六种平台归档。
新增 ARM64 支持之前仅含四种归档的运行无法用于完整安装矩阵复用。通常的手动构建和 tag 构建会同时
生成便携包与安装包，安装检查通过后才上传安装包。本文中的版本和运行 ID 仅为示例。

复用运行时，CI 使用 uv 从当前源码重新安装 Python worker，不重新构建原生程序、
其他 Python 依赖或模型。本机复用旧便携包时同样应加 `--refresh-worker`。
这样可包含安装目录只读时的修复：模型仍在系统目录，转换临时文件写入本次工程的临时目录。

Windows 中文安装器翻译取自 [Inno Setup 官方资源](https://github.com/jrsoftware/issrc/blob/6ef32198ef1f7b7b375cd4b6b90896c2a58eb4c2/Files/Languages/ChineseSimplified.isl)，
按译者项目的 MIT 许可证保留原始头部、完整文本及 `ChineseSimplified-LICENSE.txt`，随构建脚本提供，不依赖编译器预装中文语言包。
