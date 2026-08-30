# K3 补丁说明

K3 仅使用 `rookie-cookies` 读取 Chromium/Chrome 登录 Cookie，不需要读取
Internet Explorer 的 ESE 数据库。

上游 `0.5.9` 在 Windows 目标上无条件依赖 `libesedb`。其
`libesedb-sys 0.2.1` 构建脚本通过构建主机的 `cfg!(windows)` 选择配置，
导致从 Linux 使用 `cargo-xwin` 交叉编译 Windows 目标时错误选择 Unix 配置，
并因缺少 `unistd.h` 和 `libintl.h` 而失败。

此 vendored 补丁增加了 `internet-explorer` feature：

- 默认启用，以保持上游 crate 的默认行为；
- K3 关闭默认 feature，仅启用 Windows Chrome 登录所需的 `appbound`；
- 未启用时保留 Internet Explorer API，但调用会返回明确的功能未启用错误。

升级 `rookie-cookies` 时，应先检查上游是否已将 Internet Explorer 支持改为
可选依赖，或 `libesedb-sys` 是否已改为依据 Cargo 目标平台选择构建配置；若已
修复，可以删除本目录及 workspace 的 `[patch.crates-io]` 配置。
