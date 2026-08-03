# 本地 K 歌音源分离模型调研

调研日期：2026-08-04

## 结论

本项目不应只绑定 Demucs。面向“从完整歌曲生成尽量干净的伴奏”这一目标，建议采用三个档位：

1. **Quality：Mel-Band RoFormer 或 BS-RoFormer 的人声/伴奏专用权重**
2. **Balanced：SCNet 或 TFC-TDF-UNet v3 / MDX23C**
3. **Compatible：HTDemucs；Open-Unmix 只作为稳定基线**

Spleeter 可以用于速度对照，但不值得作为新项目的主要 GPU 路径。BandIt 面向影视对白/音乐/音效分离，Apollo 面向音频修复，都不是本项目的直接候选。

需要严格区分：

- **架构开源**不表示存在官方预训练权重；
- **推理代码采用 MIT**不表示第三方权重也可以随安装包再分发；
- 同一架构的不同社区权重，训练数据、目标 stem、音质与许可证可能完全不同。

## 候选对比

| 候选 | 典型输出 | 质量定位 | 资源/速度 | 预训练权重与维护 | 对 K 歌的判断 |
|---|---|---|---|---|---|
| Mel-Band RoFormer | 通常为目标源二轨，也可训练四轨 | 最高质量候选 | Transformer，通常较重；需要切块和 overlap | 论文与实现公开，但常用权重多来自社区，需逐个审核 | **Quality 首选候选** |
| BS-RoFormer | 二轨或四轨 | 最高质量候选 | 较重，论文使用 FlashAttention | 公开实现与社区权重较多；权重来源需审核 | **Quality 首选候选** |
| SCNet | 四轨为主，也可训练目标源 | 高质量、较高效率 | 论文称 CPU 推理时间为 HTDemucs 的 48% | 官方代码 MIT；具体发布权重仍需逐个核验 | **Balanced 首选候选** |
| TFC-TDF-UNet v3 / MDX23C | 二轨或四轨 | 良好，偏实用 | 比 RoFormer 更适合作为快速档 | 官方挑战代码可复现；社区存在大量不同权重 | **Balanced/快速档候选** |
| HTDemucs | 四轨，二轨模式最终合并 | 成熟高质量 | 约 7GB 默认显存；二轨模式不节省推理量 | 官方仓库已归档，依赖老化；官方权重成熟 | **兼容档与比较基准** |
| Open-Unmix | 四轨，按目标分别运行模型 | 稳定但质量落后 | 实现简单、资源较低 | 官方项目清晰；`umxl` 权重为 CC BY-NC-SA 4.0 | **测试基线，不作为默认成品** |
| Spleeter | 原生二/四/五轨 | 老一代基线 | 速度快 | TensorFlow 技术栈偏旧，Windows GPU 路径不理想 | **仅作速度基线** |
| Band-SCNet | 实时四轨研究方向 | 实时模型中较好 | 论文为 92ms 延迟、2.59M 参数 | 需进一步确认成熟权重与工程支持 | 不用于离线 MVP；92ms 也过高于演唱监听目标 |

## 证据与解释

### BS-RoFormer

BS-RoFormer 在频谱子带内和子带间使用带 RoPE 的层级 Transformer。论文报告：仅使用 MUSDB18HQ 的较小版本平均 SDR 为 9.80dB；使用额外 500 首训练歌曲的系统获得 SDX'23 音乐分离赛道第一名。

- 论文：[Music Source Separation with Band-Split RoPE Transformer](https://arxiv.org/abs/2309.02612)
- 公开 PyTorch 实现：[lucidrains/BS-RoFormer](https://github.com/lucidrains/BS-RoFormer)
- SDX'23 总结：[The Sound Demixing Challenge 2023 — Music Demixing Track](https://arxiv.org/abs/2308.06979)

对 K 歌而言，应优先测试“vocals/instrumental”专用权重，而不是为了得到二轨先完整预测四轨。主要风险不是 5070 Ti 的算力，而是社区权重的训练数据和许可证不统一。

### Mel-Band RoFormer

Mel-Band RoFormer 将经验定义的非重叠频带改为重叠 Mel 频带。论文结果显示，它在 vocals、drums 和 other 上优于对应的 BS-RoFormer；低频集中的 bass 并非其明显优势。

- 论文：[Mel-Band RoFormer for Music Source Separation](https://arxiv.org/abs/2310.01809)
- 可运行的通用实现同样位于：[lucidrains/BS-RoFormer](https://github.com/lucidrains/BS-RoFormer)

它尤其匹配本项目的人声/伴奏二轨目标。但论文架构本身不等于一个可直接发布的官方 checkpoint；选择权重时必须记录作者、训练目标、输入采样率、模型哈希和许可证。

### SCNet

SCNet 使用按频带稀疏压缩的频域网络。论文报告在没有额外训练数据时，MUSDB18-HQ 平均 SDR 为 9.0dB，CPU 推理时间约为 HTDemucs 的 48%。

- 论文：[SCNet: Sparse Compression Network for Music Source Separation](https://arxiv.org/abs/2401.13276)
- 官方实现：[starrytong/SCNet](https://github.com/starrytong/SCNet)

它适合承担 Balanced 档：四轨能力完整，复杂度比 RoFormer 更容易控制。代码仓库为 MIT，但应用发布前仍应核查实际采用 checkpoint 的条款。

### TFC-TDF-UNet v3 / MDX23C

TFC-TDF-UNet v3 是 KUIELab 的 SDX'23 获奖方案基础。论文将其描述为时间效率较高的模型，并提供 vocals-only 配置；最终挑战系统通常会用多个模型及 Demucs 做 ensemble，因此不能把 ensemble 成绩归因于单个模型。

- 技术报告：[TFC-TDF-UNet v3](https://arxiv.org/abs/2306.09382)
- KUIELab 研究主页：[KUIELab](https://kuielab.github.io/)
- 通用训练/推理框架：[ZFTurbo/Music-Source-Separation-Training](https://github.com/ZFTurbo/Music-Source-Separation-Training)

“MDX-Net”常同时指 KUIELab 架构和 UVR 社区中的大量 ONNX checkpoint，二者不能混为一个固定模型。它适合速度/质量平衡档，但每个权重必须独立评测。

### Open-Unmix

Open-Unmix 是清晰、稳定的研究基线，每个目标源使用独立的双向 LSTM 频谱模型。项目本身强调可理解性与参考实现价值，而不是追逐当前最高质量。

- 官方仓库：[sigsep/open-unmix-pytorch](https://github.com/sigsep/open-unmix-pytorch)
- JOSS 论文：[Open-Unmix — A Reference Implementation](https://joss.theoj.org/papers/10.21105/joss.01667)

需注意默认 `umxl` 权重明确标为 CC BY-NC-SA 4.0，不适合未经审查地用于商业发行；`umxhq` 等权重也应分别确认。

### 不进入当前短名单的模型

- **BandIt/Banquet**：主要研究影视对白、音乐和音效，或开放类别 stem 查询，不直接优化 K 歌伴奏。[BandIt](https://github.com/kwatcharasupat/bandit)、[Banquet](https://github.com/kwatcharasupat/query-bandit)
- **Apollo**：高采样率音频修复模型，不是把音乐混音拆成人声与伴奏的模型。[Apollo 论文](https://arxiv.org/abs/2409.08514)
- **BSMamba2**：有公开研究与代码，但相对新，成熟预训练权重、部署生态和长期维护性尚不足以替代首批候选。[论文](https://arxiv.org/abs/2409.06245)
- **Band-SCNet**：值得关注实时分离，但论文的 92ms 算法延迟不适合本项目的实时返听目标；离线预处理也没有必要牺牲质量换实时性。[Interspeech 2025 论文页](https://www.isca-archive.org/interspeech_2025/yang25d_interspeech.html)

## 针对 RTX 5070 Ti 16GB 的落地建议

5070 Ti 足以测试上述模型，但 RoFormer 的显存消耗高度依赖 checkpoint、chunk size、overlap、stem 数量和 test-time augmentation，不能只按架构名承诺一定装入 16GB。Worker 必须支持：

- chunk size 降级；
- overlap 分档；
- CUDA OOM 后清理缓存并用更小配置重试；
- FP32/混合精度按模型白名单控制；
- 模型下载、哈希校验与版本固定；
- 推理前显存预算检查；
- 将原始解码 PCM 与分离结果缓存，避免重复推理。

建议首轮实测以下四条路径：

1. HTDemucs：现有成熟基准；
2. 一个许可证明确的 Mel-Band 或 BS-RoFormer 二轨权重：Quality；
3. SCNet 四轨权重：Balanced；
4. TFC-TDF/MDX23C vocals-only 权重：Fast/Balanced。

## 不应直接比较的指标

不同论文常使用不同训练数据、额外私有歌曲、单模型或 ensemble、不同 SDR 实现和不同测试集。不能把所有公开数字排成一个统一排行榜。

本项目应建立自己的 20–50 首测试集，覆盖：

- 中文流行歌、男女声、说唱、合唱与和声；
- 强混响现场录音；
- 人声与失真吉他/合成器重叠；
- 老录音和高压缩 MP3；
- 立体声宽混音和居中人声。

K 歌主指标应是：伴奏中的主唱残留、人声移除造成的乐器损伤、和声保留策略、伪影的主观可闻度、处理时间、峰值显存和失败率。平均四轨 SDR 只能作为辅助指标。

## 建议的模型接口

模型选择应隐藏在 worker 内，主程序只认识质量档和输出语义：

```text
Fast       -> 低耗时 vocals/instrumental 模型
Balanced   -> SCNet 或 TFC-TDF/MDX23C
Quality    -> Mel-Band/BS-RoFormer
Compatible -> HTDemucs
```

工程必须记录实际的 provider、architecture、checkpoint ID、SHA-256、推理参数和许可证快照，而不能只记录 `quality`。
