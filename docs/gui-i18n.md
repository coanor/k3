# GUI 国际化

GUI 可在 `Settings` 中即时切换简体中文、English、繁體中文。选择保存在 GUI 专用的
`gui.json` 的 `language` 字段，允许值为 `zh-Hans`、`en`、`zh-Hant`。旧配置缺少该字段时
默认使用英文。TUI 继续使用英文；歌曲名、歌词、文件名、设备名和外部错误原文不翻译。

## 新增界面文案

1. 在 `crates/k3-gui/ui/strings.slint` 新增英文源文案，使用 `@tr("...")`。
2. 在 `crates/k3-gui/translations/zh-Hans/LC_MESSAGES/k3-gui.po` 和
   `crates/k3-gui/translations/zh-Hant/LC_MESSAGES/k3-gui.po` 中加入同一 `msgid` 的翻译。
3. 在 Slint UI 中通过 `Strings.属性名` 引用。构建脚本会把语言包编入程序，发布时无需
   另行复制 `.po` 文件。

Rust 生成的状态消息先保留英文源文案，再由 `crates/k3-gui/src/i18n.rs` 在显示时查找
语言包。静态状态只需加入两份 `.po`；含变量的状态还要在 `TEMPLATES` 中登记，使用
`{name}` 形式标记变量，并在翻译中保留相同的变量名。Slint 的
`localize-message` 回调将状态消息转换为当前语言，切换语言后会立即刷新界面文本。
新增的动态模板应补充不依赖 UI 组件的翻译检查。遇到无法识别的消息时会保留英文原文，
避免改写文件路径或第三方错误详情。

运行 `cargo check -p k3-gui --locked` 验证 Slint 和语言包可编译，再运行
`cargo test -p k3-gui --lib i18n --locked` 验证动态消息模板。
