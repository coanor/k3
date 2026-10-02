# K3 GUI 首期验收记录

本文件记录 `docs/gui-spec.md` 中不能只靠代码审查判定的运行结果。每次 Slint、Rodio、
Linux 打包或 GUI 关键交互发生变化时，应重新执行对应项目并更新日期与证据链接。

## 自动化门禁

以下项目由 workspace 测试和 `.github/workflows/dist.yml` 持续验证：

- `cargo fmt --all -- --check`；
- `cargo clippy --workspace --all-targets --locked -- -D warnings`；
- `cargo test --workspace --locked`；
- Ubuntu 24.04 X11（Xvfb）窗口启动；
- Ubuntu 24.04 Wayland（Weston headless）窗口启动；
- 1000 个工程扫描和虚拟列表首帧小于 1 秒；
- Linux 发行包包含 CLI、GUI、桌面文件、图标和许可证。

PR 会执行 `verify` job；跨平台发行构建只在手动运行 workflow 或推送版本 tag 时执行。

## 当前记录

| 环境或预算 | 状态 | 日期 | 证据 |
| --- | --- | --- | --- |
| Arch Linux Wayland，femtovg/OpenGL ES | 通过 | 2026-08-23 | 本地窗口与播放界面启动 |
| Arch Linux X11，femtovg/OpenGL ES | 通过 | 2026-08-23 | 本地强制 X11 启动 |
| 1000 工程扫描小于 1 秒 | 通过 | 2026-08-23 | `performance_budget`，本机约 0.42 秒 |
| 1000 工程首帧小于 1 秒 | 通过 | 2026-08-23 | `window_and_thousand_project_first_frame_stay_within_budget`，本机约 0.62 秒 |
| Linux GUI 发行包内容 | 通过 | 2026-08-23 | `scripts/verify-linux-gui-package.sh` |
| Ubuntu 24.04 X11 CI | 待运行 | — | 合并前 PR `verify` job |
| Ubuntu 24.04 Wayland CI | 待运行 | — | 合并前 PR `verify` job |
| 真实设备 Play 到出声小于 300 ms | 待验收 | — | 需要声卡和本地音频 |
| 歌词与听感位置误差不超过 100 ms | 待验收 | — | 需要已知时间码测试音频 |
| 100% / 150% / 200% HiDPI | 待验收 | — | 需要实际桌面会话 |

## 实机验收步骤

1. 使用包含 Original、Accompaniment、Vocals 和同步歌词的工程启动 `k3-gui`。
2. 分别在 Wayland 与 X11 下验证播放、暂停、前后跳转、拖动进度、切轨和歌词跳转。
3. 将桌面缩放依次设为 100%、150% 和 200%，确认 CJK、控制区、工程列表和弹层没有裁切。
4. 使用外部录音或回环设备测量点击 Play 到首个非静音采样的时间。
5. 使用带可听节拍与对应 LRC 时间码的测试工程，测量歌词高亮误差。
6. 断开或占用音频设备，确认错误条、Retry、日志路径和 TUI 回退诊断可用。
7. 手动运行 `Build distributions` workflow，下载 Linux artifact 并执行：

   ```bash
   scripts/verify-linux-gui-package.sh dist/k3-linux-x86_64.tar.gz
   ```

只有表中三个“待验收”项目和两个 Ubuntu CI 项目取得通过证据后，首期发布门槛才算完成。
