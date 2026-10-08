# K3

[English](README.md) | [简体中文](README.zh-Hans.md) | **繁體中文**

K3 是一個優先在本機運作的 K 歌工作區，提供 Linux/Windows 桌面 GUI 和跨平台 CLI/TUI。它可以
儲存歌曲專案、匯入 LRC 歌詞、呼叫本機分軌 worker，播放 Original、Accompaniment
與 Vocals 音軌，並在桌面 GUI 中錄製預設麥克風、進行即時監聽和搜尋同步歌詞。

## 快速安裝

使用 `install.sh`（Linux/macOS）或 `install.ps1`（Windows）安裝，無需預先安裝 Rust、
Python 或 uv。安裝程式會讓你選擇磁碟和目錄，確認後下載目前平台的程式，並在平台支援時
準備獨立 Python、分離相依套件及模型，最後執行啟動和模型檢查。完整安裝需預留 **12 GiB**
峰值空間，安裝後約占 **2–10 GiB**，實際大小依 GPU 支援情況而異；
Windows ARM64 和 Intel macOS 的精簡 CLI 安裝需預留 **512 MiB**。

**目前正式版本：[v0.1.2](https://github.com/coanor/k3/releases/tag/v0.1.2)。
六平台線上安裝已通過驗證；請選擇下面對應系統的一行命令。**

系統需求：Linux 需 glibc ≥ 2.39（如 Ubuntu 24.04），macOS 需 14 或更高，Windows x64
需 Windows 10/11，Windows ARM64 需 Windows 11。Ubuntu 24.04 安裝前準備執行階段函式庫：

```bash
sudo apt update && sudo apt install libasound2t64 libfontconfig1 libxkbcommon-x11-0 libegl1 libgl1-mesa-dri
```

### 一行安裝：v0.1.2

複製對應系統的一行命令即可開始安裝，無需登入 GitHub 或手動下載平台元件。
以下命令明確選擇正式版 `v0.1.2`。

Linux/macOS：

```bash
curl -fsSL https://github.com/coanor/k3/releases/download/v0.1.2/install.sh | bash -s -- --version v0.1.2
```

Windows PowerShell：

```powershell
irm -ErrorAction Stop https://github.com/coanor/k3/releases/download/v0.1.2/get.ps1 | iex
```

腳本從終端機讀取選擇磁碟和確認輸入。Windows 啟動腳本自動驗證安裝入口的 SHA-256 並處理 UTF-8 編碼；
安裝程式繼續驗證程式、uv、支援檔案與模型。Bash 腳本內部已有 `pipefail`，但它無法改變外層
`curl | bash` 的結束狀態：下載失敗時 curl 會報錯；若需自動化擷取該失敗，請先執行
`set -o pipefail`。手動查看與驗證入口腳本的步驟可在下面展開。

<details>
<summary>可選：下載入口腳本，核對 SHA-256 後執行</summary>

Linux/macOS：

```bash
release_url='https://github.com/coanor/k3/releases/download/v0.1.2'
curl --proto '=https' --proto-redir '=https' -fL "$release_url/install.sh" -o install.sh &&
curl --proto '=https' --proto-redir '=https' -fL "$release_url/install.sh.sha256" -o install.sh.sha256 &&
if command -v sha256sum >/dev/null; then
    sha256sum --check install.sh.sha256
else
    shasum -a 256 --check install.sh.sha256
fi &&
bash ./install.sh --version v0.1.2
```

Windows PowerShell：

```powershell
& {
    $ErrorActionPreference = 'Stop'
    $releaseUrl = 'https://github.com/coanor/k3/releases/download/v0.1.2'
    Invoke-WebRequest -UseBasicParsing "$releaseUrl/install.ps1" -OutFile .\install.ps1
    Invoke-WebRequest -UseBasicParsing "$releaseUrl/install.ps1.sha256" -OutFile .\install.ps1.sha256
    $expected = ((Get-Content .\install.ps1.sha256 -Raw).Trim() -split '\s+')[0]
    if ((Get-FileHash .\install.ps1 -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expected) {
        throw 'Installer script SHA-256 verification failed'
    }
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -Version v0.1.2
}
```

</details>

安裝程式完成後會輸出啟動路徑；Linux 和 Windows x64 可啟動 `k3-gui` / `k3-gui.exe`，
所有平台均可用 `k3 --help` / `k3.exe --help` 查看命令。安裝程式不會自動修改系統 PATH。

### 平台支援

| 系統 | 建置元件（自動選擇） | GUI | 音軌分離 |
| --- | --- | --- | --- |
| Linux x64 | `online-k3-linux-x86_64` | 支援 | 支援 |
| Linux ARM64 | `online-k3-linux-aarch64` | 支援 | 支援 |
| Windows x64 | `online-k3-windows-x86_64` | 支援 | 支援 |
| Windows ARM64 | `online-k3-windows-aarch64-cli` | 暫不支援 | 暫不提供完整分離環境 |
| macOS Apple Silicon（M1/M2/M3/M4 等） | `online-k3-macos-aarch64` | 暫不支援 | 支援 |
| macOS Intel | `online-k3-macos-x86_64-cli` | 暫不支援 | 暫不提供完整分離環境 |

<details>
<summary>可選：從 Actions 元件安裝</summary>

1. 登入 GitHub，開啟[v0.1.2 發行版建置](https://github.com/coanor/k3/actions/workflows/dist.yml?query=branch%3Av0.1.2)，
   在頁面底部 Artifacts 下載 **`online-support`** 和上表中與你的系統對應的一組元件。
   建置產物保留 14 天；若已過期，請從 [Actions](https://github.com/coanor/k3/actions/workflows/dist.yml)
   選擇較新的成功六平台建置，並從同一次建置下載兩組檔案。
2. 將 `online-support` 的外層 ZIP 解壓縮到 `k3-install/support/`，平台元件的外層 ZIP 解壓縮到
   `k3-install/platform/`。**保留全部 `.sha256` 檔案；內部的 `k3-install-support.zip` 無需手動解壓縮。**
3. 開啟終端機，進入包含 `k3-install` 資料夾的目錄，執行對應安裝命令。

Linux/macOS：

```bash
bash ./k3-install/support/install.sh --source-dir "$PWD/k3-install"
```

Windows PowerShell：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\k3-install\support\install.ps1 -SourceDir "$PWD\k3-install"
```

這兩條命令會提示選擇磁碟／安裝目錄和確認下載。這種方式從本機元件安裝 K3 程式，
Python、第三方相依套件和模型仍需連線下載。選擇新的空目錄，安裝程式會拒絕覆寫非空目錄。

</details>

如果匿名下載遇到 GitHub API `403` / `429` 流量限制，請等待額度恢復；目前安裝程式
支援在處理程序環境中設定個人 `GITHUB_TOKEN` 進行驗證，勿將權杖寫入命令參數或公開檔案。
磁碟選擇、指定目錄／版本、解除安裝及可選離線套件見[安裝說明](docs/install-packages.md)，
GUI 加歌、播放和錄音的操作見[繁體中文使用者指南](docs/user-manual.zh-Hant.md)。
其他安裝說明與技術文件目前使用簡體中文。

## 從原始碼建置

桌面介面的技術決策見 [GUI 規格](docs/gui-spec.md)，平台與效能證據見
[GUI 驗收紀錄](docs/gui-acceptance.md)。

### 建置與測試

儲存庫透過 `rust-toolchain.toml` 固定 Rust 1.99.0，以及對應的 Clippy 和 rustfmt；
本機與 GitHub Actions 使用同一版本。使用 rustup 時，在儲存庫中執行下列命令會自動
選擇並安裝該工具鏈。升級編譯器時應同時更新此檔案並通過全部檢查。

```bash
rustup show active-toolchain
cargo test --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo run -p k3 -- --help
cargo run -p k3-gui
```

Linux 桌面發行套件同時包含 `k3` 和 `k3-gui`。`k3-gui` 使用 Slint 的
femtovg/OpenGL ES 硬體繪製器，不啟用軟體繪製器；無法啟動桌面後端時仍可執行
`k3 tui`。

### 建立並開啟專案

```bash
cargo run -p k3 -- new \
  --root ./songs/example \
  --song /path/to/song.flac \
  --lyrics /path/to/song.lrc \
  --title "Example"

cargo run -p k3 -- show --project ./songs/example
cargo run -p k3 -- tui --project ./songs/example
cargo run -p k3-gui
```

首次啟動 GUI 時只需選擇儲存現有 K3 專案的目錄。GUI 與 TUI 共用各專案中的
`project.json`，但 GUI 的視窗、音量和專案根目錄設定另行儲存。

### 本機分軌 worker

Rust 透過小型 `StemSeparator` 介面呼叫 `python/separator` 下的 JSON-lines
worker。它支援品質設定、明確的模型檢查點、SHA-256 來源追蹤，以及由
`project.json` manifest 參照的版本化音軌輸出。安裝與通訊協定說明見
[worker README](python/separator/README.md)。

範例：

```bash
cargo run -p k3 -- separate \
  --project ./songs/example \
  --profile quality \
  --model mel-band-roformer-kim-vocal-2 \
  --worker ./.venv-separator/bin/k3-separator \
  --model-dir ~/.cache/k3/models
```

線上安裝與升級會偵測顯示卡並準備相容的 PyTorch；首次使用依硬體推薦分離品質，既有設定優先。750 Ti 使用 CUDA 12.6 與 Fast；Blackwell 使用 CUDA 12.8。Apple Silicon 使用 MPS，AMD/Intel 顯示卡目前使用 CPU。相依套件於安裝階段下載，詳見 [英文硬體選擇說明](docs/install-packages.md#hardware-selection)。


分離頁面和設定中可選擇自動、僅 CPU、僅 GPU。CPU 模式停用 GPU 推論；GPU 模式無法使用時會報錯，不會退回 CPU。命令列支援 `--device auto|cpu|gpu`，PowerShell 批次處理支援 `-Device cpu`，也可設定 `K3_DEVICE` 或 `separation.device`。切換裝置不會下載相依套件。

## 可選的離線發行套件

Windows x86_64、Linux x86_64/ARM64 與 macOS Apple Silicon 的完整套件包含程式、獨立
Python、CPU 分離相依套件、FFmpeg 和預設模型；解壓縮後無需安裝 Python 或連線下載模型。
Intel macOS 與 Windows ARM64 提供原生 CLI/TUI 精簡套件，不包含 GUI 和離線分離環境。
M1/M2/M3/M4 等 Apple Silicon 晶片共用 macOS ARM64 套件，要求 macOS 14 或更高。
離線建置入口為 `make dist-offline` 和 GitHub Actions 的 `offline_bundle`，具體用法見
[離線發行套件說明](docs/offline-package.md)。

## 授權條款

K3 自有程式碼使用 [MIT 授權條款](LICENSE)，允許使用、修改和散布，但需保留著作權與授權聲明；
軟體依現狀提供，不附帶擔保。相依函式庫、字型和模型分別遵循各自的授權條款；儲存庫內的第三方
授權聲明仍然適用，其中 Slint 的授權原文見 [Slint 授權聲明](crates/k3-gui/assets/licenses/LicenseRef-Slint-Royalty-free-2.0.md)。
