# 在线同步歌词来源调研

调研日期：2026-08-15

## 结论

K3 目前不宜再直接内置一个国内音乐平台的私有网页接口。严格按“免费、免登录、可程序化访问、返回带时间轴歌词、官方允许这种接入”筛选，当前只有 **LRCLIB** 明确满足条件。

推荐按以下顺序提高命中率：

1. 保留 LRCLIB 作为唯一默认在线歌词目录，但改进同一来源内的查询策略：先用歌名、歌手、专辑和时长精确查询，再按歌名与歌手搜索，最后只按歌名搜索；对候选结果统一按时长、规范化歌名和歌手评分。
2. 增加可配置的歌词提供者接口，允许用户自行配置获得合法授权的来源；默认发行版不携带网易云、QQ 音乐或酷狗的私有接口实现。
3. 可选使用 MusicBrainz 补齐 ISRC、规范歌手名和发行信息，再回查 LRCLIB；MusicBrainz 不能直接提供歌词。
4. 若未来需要明显提高中文曲库覆盖，应向网易云、腾讯音乐或酷狗申请书面 API/内容授权，而不是依赖抓包得到的网页接口。

网易云的 `/api/search/get/web` 和 `/api/song/lyric` 在本次实测中无需登录即可返回搜索结果和 LRC，但它们是 **undocumented web endpoints**：未在网易云开发者文档中公布，没有版本、限流、可用性或兼容性承诺。技术上当前可调用不等于获得了长期或再分发授权。网易云官方服务条款还限制复制服务交互数据和未经书面同意复制服务内容，因此不建议将其作为 K3 默认或备用来源。

## 筛选结果

| 来源 | 免费/免登录 | 同步歌词 | 中文歌曲适配 | 官方接入依据 | 限流与稳定性 | 建议 |
| --- | --- | --- | --- | --- | --- | --- |
| LRCLIB | 是，无需 API key 或注册 | `syncedLyrics`，标准 LRC | 有中文内容，但社区曲库覆盖和质量不保证 | 有公开 API 文档；服务端源码为 MIT | 返回 `429` 和 `Retry-After`；官方要求串行请求并间隔 200–500 ms；无 SLA | **默认接入** |
| 网易云网页接口 | 当前实测免登录 | `lrc.lyric`，LRC | 中文曲库通常更有优势 | 没有公开开发者 API 文档；服务条款不支持按网页交互数据接口抓取和复制内容 | 无公开限流、版本或稳定性承诺，随时可能改参数、加密或封禁 | **不要内置**，除非获得书面授权 |
| QQ 音乐 | 官方歌词能力不是通用免登录 API | 官方结构含普通歌词和 QRC 字段 | 适合中文歌曲 | 腾讯连连 H5 SDK 有官方歌词接口，但文档明确除“免登录专区”外均需登录授权；歌词不在免登录专区 | 依赖腾讯产品、账号授权及业务生命周期 | **不适合 K3 默认接入** |
| 全民 K 歌开放平台 | 否，需要开放平台身份 | 官方接口目前支持 LRC | K 歌场景和中文曲库匹配 | 请求要求 `X-Open-Access-Token`、`X-Open-ID`、`X-Open-App-ID` 等 | 需平台接入；公开页面未给出可供匿名桌面应用使用的配额 | 取得合作资格后可评估 |
| 酷狗开放平台 | 未找到匿名歌词 API | 未在公开开放组件文档中找到通用歌词下载接口 | 适合中文歌曲 | 官方开放平台面向小程序、曲库开放组件和商业版权合作 | 需要平台入驻/授权；未找到匿名接口的限流承诺 | **不要调用抓包接口**；获得官方授权后再评估 |
| Musixmatch | 否，需要 API key；同步字幕属于更高付费方案 | `track.subtitle.get` | 国际曲库较强，中文覆盖未获官方保证 | 有正式 API、地区限制和歌词展示追踪要求 | 方案和额度由商业计划决定 | 不符合免费免登录目标；可作为用户自带 key 的付费插件 |
| MusicBrainz | 非商业查询免费、免 API key | 不提供歌词正文 | 可辅助中文歌曲元数据匹配 | 官方 API 明确是音乐元数据服务 | 每个应用最多 1 次/秒并要求有效 User-Agent | 只作元数据增强，不作歌词来源 |
| Spotify / Apple Music | 需要开发者凭据或 token | 公开目录 API 没有歌词正文端点 | 无法解决 K3 的 LRC 需求 | 官方 API 主要提供目录元数据和播放能力 | 有认证、区域和平台政策约束 | 不接入 |

## 逐项依据

### LRCLIB

[LRCLIB API 文档](https://lrclib.net/docs)明确说明：API 对所有用户和应用开放，不需要 API key 或注册；`/api/search` 返回 `plainLyrics` 和 `syncedLyrics`，`/api/get` 可按歌名、歌手、专辑和时长精确匹配。调用方必须提供可识别的 `User-Agent`。超过限流时服务返回 `429 Too Many Requests` 和 `Retry-After`，客户端必须遵守；批量操作应串行请求并间隔 200–500 ms。

其[官方服务端源码](https://github.com/tranxuanthang/lrclib)采用 MIT 许可证并可自行部署。需要区分：MIT 明确覆盖服务端代码，但官方页面没有为数据库里的第三方歌词文本给出同等的内容许可证。K3 应只为用户当前歌曲按需下载并本地缓存，不应打包、镜像或再分发整库歌词。

稳定性方面没有 SLA。官方项目的[性能讨论](https://github.com/tranxuanthang/lrclib/discussions/91)记录了响应缓慢和 User-Agent 被阻止的实际情况，维护者也再次要求使用可识别的 User-Agent。因此实现必须有超时、`429` 退避、缓存和“找不到不影响录音”的降级行为。

LRCLIB 对精确查询中的时长要求严格，官方文档称通常只接受约 ±2 秒；但 `/api/search` 最多返回 20 个结果，适合 K3 自己按实际音频时长挑选候选。这比只增加一个高风险私有来源更值得先做。

### 网易云音乐

第一方网页接口当前存在：

- [歌曲搜索接口示例](https://music.163.com/api/search/get/web?csrf_token=&s=%E6%98%A8%E5%A4%9C%E6%98%9F%E8%BE%B0&type=1&offset=0&total=true&limit=2)
- [歌词接口示例](https://music.163.com/api/song/lyric?id=347230&lv=1&kv=1&tv=-1)

本次请求无需账号即可分别得到歌曲 ID 和 `lrc.lyric`。然而，网易云没有为这些地址提供公开的开发者契约，也没有公布请求字段、版本、配额、错误模型、可用性或弃用政策。因此只能把它们认定为网页产品内部使用的 undocumented endpoints，不能据此推断官方支持第三方桌面应用。

[网易云音乐服务条款](https://music.163.com/html/web2/service.html)第 8 节限制未经同意使用、复制或基于软件信息建立相关衍生服务，并明确限制复制客户端与服务器端交互数据；第 10 节也限制未经书面同意复制服务提供的歌曲图文和文字内容。基于这些一手条款，默认内置并长期保存该接口返回的歌词存在明显合规风险。只有网易提供书面授权和稳定的正式 API 后才应启用。

### QQ 音乐与全民 K 歌

腾讯云的[腾讯连连 H5 SDK 音乐服务文档](https://cloud.tencent.com/document/product/1081/67456)提供 `describeLyric`，返回结构包含普通歌词、翻译、罗马音和 QRC 字段。但同一文档明确：除“登录授权”和“免登录专区”外，其他接口都需要 QQ 音乐登录授权；免登录专区只列播放列表和播放链接，并不包含歌词。该能力还绑定腾讯连连小程序 H5 环境，不是面向任意 Rust 桌面程序的匿名 REST API。

[全民 K 歌 IOT 开放平台歌词接口](https://share.apifox.cn/apidoc/docs-site/951819/api-25890496)明确支持 LRC，但请求需要 Access Token、Open ID 和 App ID。它可作为未来商务合作路径，不能满足当前免费免登录目标。

互联网上常见的 `i.y.qq.com/lyric/...`、`u.y.qq.com/cgi-bin/musicu.fcg` 等调用方式来自非官方项目对网页或客户端的复刻，不应因为接口暂时可用就视为腾讯公开 API。

### 酷狗音乐

[酷狗官方开放平台](https://open.kugou.com/docs)公开展示的接入方向是酷狗小程序、曲库开放组件等；[曲库开放组件](https://open.kugou.com/docs/open-player/)定位于嵌入播放器和正版曲库合作。公开页面没有提供适合 K3 匿名调用的歌词搜索/LRC 下载 API，也提示版权合作应联系官方。

互联网上可见的酷狗“歌词搜索/下载 API”文档主要来自第三方逆向项目，不是酷狗承诺稳定的开放接口。默认内置这类接口会同时带来封禁、协议变化和歌词内容授权风险，因此不建议接入。

### Musixmatch

Musixmatch 的[普通歌词接口](https://docs.musixmatch.com/api-reference/lyrics-catalog/track-lyrics-get)要求每次请求携带 API key，并要求遵守内容地区限制和歌词展示追踪规则。[同步字幕接口](https://docs.musixmatch.com/api-reference/lyrics-catalog/track-subtitle-get)同样需要 API key，官方页面标记为 Scale 方案，而非匿名免费接口。因此它适合作为未来“用户自带商业 key”的可选提供者，不符合 K3 当前免费免登录目标。

### MusicBrainz、Spotify 与 Apple Music

[MusicBrainz API](https://musicbrainz.org/doc/MusicBrainz_API)可免 API key 查询录音、发行、艺术家、ISRC 等元数据，非商业使用免费；它要求有意义的 `User-Agent` 且每个应用不超过每秒一次请求。官方资源类型中没有歌词正文，[Picard 官方文档](https://picard-docs.musicbrainz.org/)也说明歌词不在 MusicBrainz 数据库中。因此它只能帮助 K3 纠正歌手、标题或 ISRC，再用 LRCLIB 查询。

[Spotify Web API](https://developer.spotify.com/documentation/web-api)要求应用凭据和授权，其公开 API 目录只提供曲目元数据，没有歌词正文端点。[Apple Music API](https://developer.apple.com/documentation/applemusicapi)同样需要 developer token，官方 Songs API 只描述目录和用户资料，没有可取得 LRC/同步歌词正文的公开端点。两者都不应作为 K3 歌词来源。

## 建议的 K3 实现边界

后续业务代码可采用统一 `LyricsProvider` 接口，但默认只注册 `LRCLIB`：

```text
本地 LRC
  -> LRCLIB 精确查询（title + artist + album + duration）
  -> LRCLIB 搜索（title + artist）
  -> LRCLIB 宽松搜索（title）
  -> 可选的用户授权 provider
  -> NotFound，不阻塞播放或录音
```

匹配与网络策略：

- 优先读取音频 tag，并保留 project 标题作为候选，而不是二选一。
- 清理 `live`、`remaster`、括号版本名、全角/半角差异，但最终必须校验时长。
- 对同名歌曲按规范化歌名、歌手、专辑、ISRC、时长差综合排序。
- 缓存成功结果和短期 `NotFound`，避免每次进入 TUI 重复请求。
- 遵守 LRCLIB 的 `Retry-After`，不要把普通网络失败当成“歌词不存在”。
- 日志记录 provider、查询参数摘要、HTTP 状态和候选淘汰原因，但不要记录完整歌词。
- 第三方 provider 必须显式配置；凭据只从用户配置/环境变量读取，不写入 project。

这套设计可以先提升现有公开来源的命中率，同时保留未来合法接入中文曲库的扩展点，不把 K3 的可用性建立在无文档、无授权、随时可能变化的私有接口上。
