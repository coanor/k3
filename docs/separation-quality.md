# 分离质量与现有模型参数调查

调查日期：2026-10-02。本文针对现有 K3 worker。用户已确认双人合唱的分离正常；
目前要核对的是《流浪花》开头疑似萨克斯进入人声轨。模型效果需要用具体歌曲试听，不能仅凭模型名称或参数推断。

## 先确认输出目标

K3 默认开启 `preserve_backing_vocals`。它先用所选主模型把原曲分成“全部人声”和“乐器伴奏”，再用 `UVR-MDX-NET Karaoke 2` 把第一阶段的人声分成“主唱”和“和声”，最后**把第二阶段判为和声的声音加回伴奏**。`vocals.wav` 因而是第二阶段的“主唱”，`backing-vocals.wav` 是第二阶段的“和声”，`accompaniment.wav` 包含后者。来源：[K3 worker 的两阶段实现](../python/separator/src/k3_separator/runtime.py)、[K3 默认值与调用](../python/separator/src/k3_separator/service.py)、[模型注册表](../python/separator/src/k3_separator/models.py)。

**一般性推断：**双人同时唱时，Karaoke 2 可能将其中一人或部分重叠歌声判为“和声”，于是这部分歌声会进入最终伴奏。但用户已澄清当前双人合唱没有这个问题。K3 没有指定“歌手 A / 歌手 B”的身份输入；“把所有歌声移出伴奏”和“保留伴唱”是不同的输出目标。来源：[K3 实现](../python/separator/src/k3_separator/runtime.py)、[模型注册表](../python/separator/src/k3_separator/models.py)、[多歌手分离研究论文](https://arxiv.org/abs/2608.14516)。

**最有信息量的对照**：用同一音源和同一主模型各分离一次，分别设 `preserve_backing_vocals=true` 和 `false`；分别试听第一种的 `backing-vocals.wav`、两种的 `vocals.wav` 与 `accompaniment.wav`。若关闭和声保留后第二人的声音不再出现在伴奏，问题主要在第二阶段／输出策略；若仍有残留，再检查第一阶段模型。Windows 安装包的 `config.json` 可设 `separation.preserve_backing_vocals=false`，脚本也接受 `K3_PRESERVE_BACKING_VOCALS=false`；GUI 设置目前仅提供 profile，并没有该开关。来源：[Windows 分离脚本](../separate.ps1)、[GUI 设置结构](../crates/k3-gui/src/settings.rs)、[worker 行为](../python/separator/src/k3_separator/service.py)。

### 已提供的《吕方－流浪花》实例

只读检查 `H:\music\k3\吕方-流浪花\project.json`：本次状态为 `ready`，记录的主模型是 `mel-band-roformer-kim-vocal-2`（Quality），第二阶段是 `uvr-mdx-karaoke-2`；该安装包的 `config.json` 将 `separation.model` 设为这个 Mel-Band checkpoint，且 `preserve_backing_vocals=true`。这能确认**所用流程和模型**，不能从 manifest 确认用户听到的开头声音是否确实是萨克斯，也不能量化残留。来源：[K3 模型注册表](../python/separator/src/k3_separator/models.py)、[K3 provenance 写入逻辑](../python/separator/src/k3_separator/service.py)。

- **疑似萨克斯进 `vocals.wav`**：若试听属实，器乐必定先进入了主模型的“全部人声”输出，因为第二阶段只读取该输出，不读取第一阶段的伴奏。Karaoke 2 可能让该杂音留在主唱或转入和声，但无法凭空从原始伴奏引入它。因此应先比较主模型或检查片段，关闭和声保留**不保证**消除这种误分。来源：[K3 两阶段数据流](../python/separator/src/k3_separator/runtime.py)。
- 双人合唱的疑虑已由用户撤回，此次不调整和声保留策略。

GUI 的备歌窗口有 Fast、Balanced、Quality、Compatible 四个 profile，Quality 现可再选运行环境默认模型、Kim Vocal 2 或 BS RoFormer 1297。`Runtime default` 在**这份 H 盘配置**中使用 Mel-Band；其他安装的默认 Quality 由其 `config.json` 和 worker 注册表决定。Windows 仍可在安装包 `config.json` 调整 `separation.preserve_backing_vocals`，但本次没有为已确认正常的双人合唱增加 GUI 开关。来源：[GUI 模型选择](../crates/k3-gui/ui/app.slint)、[GUI 设置结构](../crates/k3-gui/src/settings.rs)、[GUI 启动脚本](../crates/k3-gui/src/separation.rs)、[Windows 分离脚本](../separate.ps1)、[模型注册表选择](../python/separator/src/k3_separator/models.py)。

### H 盘前奏对照

从原音源截取前 35 秒，另建 H 盘临时工程，不覆盖原工程或录音。为单独观察主模型，先关闭第二阶段和声保留，分别运行 Kim Vocal 2、Balanced 的 Inst HQ 3、BS RoFormer 1297；之后又用 BS 开启和声保留，与原工程的最终分轨对照。BS 权重首次由 worker 下载超时，随后用可续传下载保存到 H 盘模型目录，并按注册表 SHA-256 `5b84f37e…ae115aa` 校验通过。

歌词第一句在 33.57 秒，前 30 秒没有标注主唱。用 FFmpeg `volumedetect` 量得该段**第一阶段人声轨**平均电平：Kim `−21.0 dB`、Balanced `−21.1 dB`、BS `−27.1 dB`；原曲为 `−13.2 dB`。在实际两阶段输出中，原工程 Kim 最终人声轨为 `−24.4 dB`，BS 对照为 `−27.3 dB`；最终伴奏分别为 `−16.3 dB` 与 `−14.5 dB`。BS 是值得试听的候选，但电平无法辨认萨克斯音色，也不能证明完整歌曲的主唱保真度或伴奏质量更好。`H:\music\k3-data\local\quality-check` 下有 `kim-final-vocals-intro.mp3`、`bs-final-vocals-intro.mp3` 及对应的 `accompaniment` 试听文件；先听人声是否更干净，再听伴奏是否保留了乐器。

## 有依据的推理参数

下表的“可能作用”来自 `audio-separator` 官方说明和实现；属于可试验方向，**不是对某首歌改善的保证**。当前 K3 内置 profile：Fast=`UVR-MDX-NET Karaoke 2`，Balanced=`UVR-MDX-NET Inst HQ 3`，Quality 默认=`BS-RoFormer viperx 1297`、另有 `Mel-Band RoFormer Kim Vocal 2`，Compatible=`HTDemucs FT`。来源：[K3 模型注册表](../python/separator/src/k3_separator/models.py)、[audio-separator 官方 CLI 参数](https://github.com/nomadkaraoke/python-audio-separator/blob/main/README.md)。

| 模型架构 | K3 当前值 | 上游支持的相关参数与可能作用 | 主要代价／局限 |
|---|---|---|---|
| MDXC（Quality RoFormer） | `segment_size=256`、`overlap=8`、`batch_size=1`、`pitch_shift=0` | `segment_size` 是分块长度；更大可能改善上下文，但更耗内存。`overlap` 是**重叠预测窗口数量**；更多窗口可能减少分块边界问题，但更慢。`pitch_shift` 改变推理时音高，上游称可能帮助特定低／高人声。 | K3 强制覆盖模型 YAML 的 segment 值；并未验证 `256` 对这两个 checkpoint 都最佳。`batch_size` 主要影响资源／速度，不能当成音质旋钮；`pitch_shift` 对原歌的收益需要实听。[上游 MDXC 说明](https://github.com/nomadkaraoke/python-audio-separator/blob/main/README.md)、[上游 MDXC 实现](https://github.com/nomadkaraoke/python-audio-separator/blob/main/audio_separator/separator/architectures/mdxc_separator.py)、[K3 runtime](../python/separator/src/k3_separator/runtime.py) |
| MDX-Net（Fast、Balanced 和第二阶段 Karaoke 2） | `segment_size=256`、`overlap=0.25`、`batch_size=1`、`hop_length=1024`、`enable_denoise=false` | `overlap` 是**窗口重叠比例**，上游范围 `0.001–0.999`；提高可能减轻接缝，耗时增加。`segment_size` 增大可能改善结果但更耗资源。`enable_denoise` 可试验。 | 上游明确 `batch_size` 不影响输出质量；`hop_length` 不建议无根据地改。改变 ONNX 模型原生 segment 大小在 K3 的旧 CPU 路径可能引起性能或兼容问题。[上游 MDX 说明](https://github.com/nomadkaraoke/python-audio-separator/blob/main/README.md)、[上游 MDX 实现](https://github.com/nomadkaraoke/python-audio-separator/blob/main/audio_separator/separator/architectures/mdx_separator.py)、[K3 手册](user-manual.md) |
| Demucs（Compatible） | `segment_size=10`、`overlap=0.25`、`shifts=1` | `shifts` 多次随机时间偏移后平均，官方称可改善结果但近似按次数增加耗时；`overlap` 可减少分块接缝；`segment_size` 影响上下文和内存。 | 改参数不能使四轨模型识别具体歌手。HTDemucs 的人声轨仍是聚合的人声。 [Demucs 官方说明](https://github.com/facebookresearch/demucs/blob/main/README.md)、[audio-separator 官方 CLI 参数](https://github.com/nomadkaraoke/python-audio-separator/blob/main/README.md)、[K3 runtime](../python/separator/src/k3_separator/runtime.py) |
| 通用推理 | K3 `autocast=true`、44.1 kHz WAV | `autocast` 调整可用设备上的混合精度计算；可做兼容性／数值对照。 | 不是有证据的“去人声强度”参数；关闭它不保证更干净。 [audio-separator `Separator` 实现](https://github.com/nomadkaraoke/python-audio-separator/blob/main/audio_separator/separator/separator.py)、[K3 runtime](../python/separator/src/k3_separator/runtime.py) |

## 为什么只在部分伴奏出问题

现有系统在有限 stem 定义下学习频谱／波形模式。MUSDB18 的主要标注只有 vocals、bass、drums、other，没有“歌手 A”“歌手 B”或独立的混响／和声轨；Demucs 官方模型同样输出聚合 vocals。不同音色、效果器、混响和与歌声频段重叠的乐器可能造成残留或乐器损伤，尤其当输入与训练素材分布不同时。这里是**根据任务定义与数据标注做出的风险推断**，不是已测得 K3 在某类歌曲上的失败率。来源：[MUSDB18 数据集一手说明](https://sigsep.github.io/datasets/musdb.html)、[Demucs 官方说明](https://github.com/facebookresearch/demucs/blob/main/README.md)、[BS-RoFormer 原论文](https://arxiv.org/abs/2309.02612)、[Mel-Band RoFormer 原论文](https://arxiv.org/abs/2310.01809)。

多歌手目标分离本身是另一项研究任务。2026 年的 singer-informed 论文使用目标歌手的短参考录音指导提取；它报告特定实验集上的提升，不能外推为现有 K3 模型能识别歌手身份，也不能直接承诺对用户歌曲有效。来源：[论文](https://arxiv.org/abs/2608.14516)、[作者代码与权重](https://github.com/jocelynxu01/singer-separation-paper)。

## K3 当前可调范围与实现限制

- GUI 可选择 profile，Quality 档还可在两个内置权重间切换；Windows `config.json` / `separate.ps1` 还能指定主模型、`segment_size`、`autocast`、`preserve_backing_vocals`。CLI 对 worker 的额外 `options` 只传 `segment_size` 和 `autocast`；`overlap`、`shifts`、`pitch_shift` 等虽在 worker 协议和 runtime 中出现，**尚未成为 GUI／CLI 设置项**。来源：[GUI 设置](../crates/k3-gui/src/settings.rs)、[GUI 参数传递](../crates/k3-gui/src/separation.rs)、[Windows 脚本](../separate.ps1)、[CLI 请求结构](../crates/k3-cli/src/python_separator.rs)、[worker 选项白名单](../python/separator/src/k3_separator/service.py)。
- worker 把同一份请求 `options` 同时传给主模型与 Karaoke 2。**不要直接增加一个共用的 `overlap` 输入框**：MDXC 的 `8` 表示重叠窗口数，MDX 的 `0.25` 表示比例；`pitch_shift` 和 `shifts` 也分别属于不同架构。默认两阶段运行时，跨架构选项可能无效或触发 `unsupported runtime options`。需要调参界面时，应先按阶段和架构分配参数并做范围验证。来源：[K3 两阶段与参数转发](../python/separator/src/k3_separator/runtime.py)、[K3 选项验证](../python/separator/src/k3_separator/service.py)、[上游 MDX／MDXC 参数说明](https://github.com/nomadkaraoke/python-audio-separator/blob/main/README.md)。
- Quality 的两个 checkpoint 是不同权重，不能把论文架构级结论当作两份特定权重的逐曲保证。注册表记录了文件名与哈希；实际训练数据和单曲表现仍需另行核实。来源：[K3 模型注册表](../python/separator/src/k3_separator/models.py)、[当前 BS-RoFormer checkpoint 文件](https://huggingface.co/Politrees/UVR_resources/blob/main/models/Roformer/BandSplit/model_bs_roformer_ep_317_sdr_12.9755.ckpt)、[Kim Mel-Band checkpoint](https://huggingface.co/KimberleyJSN/melbandroformer/tree/main)、[BS-RoFormer 原论文](https://arxiv.org/abs/2309.02612)。

## 建议的验证顺序

1. 固定原始音源，记录 `project.json` 中的模型 ID、checkpoint SHA-256，以及若有写入的 `runtime_options`；从运行配置记录 `preserve_backing_vocals`。《流浪花》的前奏可作为无主唱的对照片段：歌词第一句标于 33.57 秒。现有 manifest 没有 `runtime_options`。来源：[K3 provenance 生成](../python/separator/src/k3_separator/service.py)、[K3 项目分离数据结构](../crates/k3-core/src/project.rs)。
2. 对这段前奏比较当前 Kim Vocal 2、另一款 Quality 权重和 Balanced 或 Compatible；固定输入片段和和声策略，试听人声轨里的乐器、伴奏轨里的乐器损伤和伪影。只有确认是分块边界问题时再逐项试 `segment_size`／`overlap`，记录时间与内存。来源：[K3 模型注册表](../python/separator/src/k3_separator/models.py)、[上游参数说明](https://github.com/nomadkaraoke/python-audio-separator/blob/main/README.md)。
3. 如果今后出现双人合唱问题，再用相同主模型对照“保留／不保留和声”：检查 `backing-vocals.wav`，区分第一阶段漏分与第二阶段回混。当前用户已确认没有该问题。来源：[K3 两阶段实现](../python/separator/src/k3_separator/runtime.py)。
4. 若未来目标是“仅移除指定歌手、保留另一歌手”，现有二轨／主唱和声模型的 stem 定义不够；需独立评估带歌手参考录音的多歌手分离方法及其权重、许可、性能和集成成本。来源：[多歌手分离论文](https://arxiv.org/abs/2608.14516)、[K3 模型注册表](../python/separator/src/k3_separator/models.py)。

本文做了该歌曲前奏的短片段电平对照，没有进行主观盲听或完整歌曲验收，因此不能判定最佳 checkpoint、参数或失真原因。上游对“更高 overlap／更多 shifts 可能更好”的说明是一般性经验，不等于 K3 的特定歌曲必然改善。
