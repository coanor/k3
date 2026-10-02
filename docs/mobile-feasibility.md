# K3 移动端构建可行性

## 结论

K3 的工程数据模型和部分音频逻辑可以复用到移动端，但当前 `k3-gui` 是桌面程序，
不能直接把现有构建命令换成 Android 或 iOS 目标并得到可用的安装包。建议首个移动端版本
先支持导入已分离的工程、播放、歌词和录音，再决定手机端分离方式。

[Slint 官方 Android 文档](https://docs.slint.dev/latest/docs/slint/guide/platforms/mobile/android/)
提供 Rust 应用入口和 APK 构建方式；[iOS 文档](https://docs.slint.dev/latest/docs/slint/guide/platforms/mobile/ios/)
提供 Rust、Xcode 和 Skia 的集成方式。K3 当前固定使用 Slint `1.17.1`，实际移植时
需要验证该版本的目标平台组合，必要时先升级并重新验收桌面版。

| 部分 | 当前实现 | 移动端所需改动 |
| --- | --- | --- |
| 界面 | Slint 桌面布局，最小窗口宽 960px；使用鼠标、键盘和三栏布局 | 手机竖屏布局、触控尺寸、安全区域、虚拟键盘适配；平板可保留多栏 |
| 应用入口 | `k3-gui.exe` / 桌面 Rust `main`，直接使用 Winit 窗口事件 | Android 增加 Activity 入口和 APK 包装；iOS 增加 Xcode 工程、场景和签名配置 |
| 文件访问 | `rfd::FileDialog`、普通文件系统路径 | 使用平台文件选择器，把选中的文件导入应用可访问的工程目录 |
| 播放与录音 | Rodio/CPAL 与系统默认设备 | 在真机验证输入输出、耳机切换、后台与中断恢复、录音权限和延迟补偿 |
| 分离 | `separate.ps1`/`separate.sh` 调用 Python worker 与模型 | 另定移动端执行方式；现有桌面脚本及 Python 运行环境不能直接打包成手机功能 |
| 网易云登录 | 桌面 Chrome Cookie 导入或扫码 | 移动端单独设计登录与会话路径；Chrome Cookie 导入入口不适用 |

Android 的 Rust 目标需要 Android SDK/NDK；可参考
[Rust Android 平台说明](https://doc.rust-lang.org/rustc/platform-support/android.html)。
文件导入可基于 Android 系统的
[Storage Access Framework](https://developer.android.com/guide/topics/providers/document-provider)。
iOS 构建需要 macOS、Xcode 和 iOS SDK，参见
[Rust iOS 平台说明](https://doc.rust-lang.org/stable/rustc/platform-support/apple-ios.html)。
iOS 麦克风访问还需在应用中配置
[`NSMicrophoneUsageDescription`](https://developer.apple.com/documentation/BundleResources/Information-Property-List/NSMicrophoneUsageDescription)。

## 建议的实施顺序

1. 把工程扫描、播放控制和录音状态与桌面文件选择器、Winit 事件和进程脚本分开。
2. 做手机尺寸的 Slint 界面与平台文件导入；先读取桌面已生成的工程，验证播放和歌词。
3. 增加 Android 应用入口与打包工程，在真机验证麦克风、音频路由和录音保存。
4. 在 macOS/Xcode 环境完成 iOS 工程、音频会话和真机验收。
5. 为分离确定独立方案：导入桌面分离结果、远程处理，或另行研究设备端模型执行。

当前工作环境没有 Android SDK/NDK、移动端 Rust 目标或 macOS/Xcode，因此没有生成
APK 或 IPA。移动端功能需要在设备上完成音频和文件访问验收，桌面编译通过不能代替。
