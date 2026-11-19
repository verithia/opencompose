# OpenCompose 文档

OpenCompose 是一款以文本文件保存歌曲的数字音频工作站。一个 `.ocp` 文档用可读的文本行写下轨道、乐段、音符、混音器设置和效果器。同一个文件可以手工编辑，可以交给模型生成，可以用命令行渲染，也可以在桌面应用里播放。

## 你会用到的部分

`.ocp` 语言是工程格式。它可以直接阅读，能在版本控制里比较差异，也不依赖某一个引擎。详见 [.ocp 文件](manual/dsl.md)。

`ocp` 命令行用来解析、校验、格式化、导出和渲染工程，还能分析音频、转换 MIDI。详见 [命令行](manual/cli.md)。

桌面应用是基于同一引擎的 Tauri 2 窗口，包含时间线、钢琴卷帘、混音器、设备架、音色补丁编辑器、插件浏览器、DSL 编辑器和 AI 助手。详见 [主窗口](manual/gui.md)。

Rust 工作区中的二十个 crate 实现了语言、命令行和应用。详见 [Component reference](../components/index.html)（英文）。

## 本站的组织方式

侧边栏按 [Diátaxis](https://diataxis.fr) 的四类文档分组，打开页面之前就能知道它的用途。教程带你从刚检出的代码走到第一次成功渲染。操作指南解决一个具体问题，例如窗口空白、渲染失败或检查不通过。参考页面给出准确的语法、控件名称、子命令、类型或工具。功能说明解释引擎为什么这样设计。

API 参考由 `cargo doc` 从源码生成，发布在 [`api/`](../api/index.html)（英文）。每个公开类型和函数都以它为准。文字页面描述行为，并链接到 API 参考。

## 接下来读什么

- 想构建命令行并渲染一个示例工程，请读 [快速上手](manual/getting-started.md)。
- 想手工编写或阅读工程，请读 [.ocp 文件](manual/dsl.md)。
- 修改某个 crate 之前，请先读 [Component reference](../components/index.html)（英文）中的对应条目。
- 想了解工程如何变成 WAV 文件，请读 [The engine pipeline](../architecture.html)（英文）。
- 想用脚本或模型驱动正在运行的应用，请读 [智能体接口](agent-surface.md)。
- 想参与仓库本身的开发，请读 [Contributing](../contributing.html)（英文）和 [Testing and verification](../testing.html)（英文）。
- 本站遵循的写作规则见 [Documentation style](../style.html)（英文）。

## 完整目录

下表列出 `docs/` 中的产品与设计文档，每一篇也都是本站的一章。设计文档说明功能如何工作、为什么这样做，适合对照代码阅读。测量日志和设计来源记录不放在仓库里。

请先读 [Compatibility and originality](../compatibility.html)（英文）。它规定了哪些内容可以写在哪里，所有合成与设备文档都以它为前提。

### 声音与乐器

| 文档 | 内容 |
|---|---|
| [`patch-system.md`](../patch-system.html)（英文） | 什么是音色补丁，DSL、合成器和图形界面如何共用它。 |
| [`modulation.md`](../modulation.html)（英文） | 包络、曲线 LFO（480 点波形、生成器、同步档位）和调制矩阵。 |
| [`filter-character.md`](../filter-character.html)（英文） | 滤波器类型、阻尼与驱动规律、共振上限。 |
| [`osc-granular-spectral.md`](../osc-granular-spectral.html)（英文） | 颗粒与频谱声源振荡器。 |
| [`synth-character.md`](../synth-character.html)（英文） | 振荡器读取、齐奏、滤波器、调制和采样层。 |
| [`fx-laws.md`](../fx-laws.html)（英文） | 合成器效果架中的信号流和效果器顺序。 |
| [`fx-delay-distortion.md`](../fx-delay-distortion.html)（英文） | 延迟线、它的时间映射和上报的延迟；多级波形整形器。 |
| [`fx-hyperd-conv.md`](../fx-hyperd-conv.html)（英文） | 声场扩展器和卷积插入效果。 |
| [`fx-utils-split-bode.md`](../fx-utils-split-bode.html)（英文） | 实用处理、信号分离和移频。 |
| [`drum-kit.md`](drum-kit.md) | 打击垫网格乐器：层、互斥组、轮询。 |
| [`drum-synthesis.md`](../drum-synthesis.html)（英文） | 合成鼓音色：没有采样层的打击垫目前如何发声，以及将取代它的音色设计。 |
| [`sampler.md`](sampler.md) | 采样器和多采样引擎。 |
| [`sf2-player.md`](../sf2-player.html)（英文） | SoundFont 2 读取器和播放器：数据块、支持的生成器子集、电平规律，以及它不做的事。 |
| [`factory-presets.md`](factory-presets.md) | 66 个 OpenSynth 出厂音色补丁，以及每个的构成。 |

### 设备

| 文档 | 内容 |
|---|---|
| [`vocal-chain.md`](../vocal-chain.html)（英文） | 人声链的顺序和理由；延迟账目。从这里开始。 |
| [`vocal-analysis.md`](../vocal-analysis.html)（英文） | 离线单音音高分析器：帧与音符约定、缓存键、实测精度和 CPU 占用。 |
| [`vocal-tune.md`](../vocal-tune.html)（英文） | `@vocal_tune` 音符模型：保存的决定、目标解析、amount=0 直通和 `ocp vocal apply`。 |
| [`vocal-comp.md`](../vocal-comp.html)（英文） | 电平调节器（包括 `ocp level`）和光电压缩器。 |
| [`vocal-tone.md`](../vocal-tone.html)（英文） | 变压器级和无源 EQ。 |
| [`vocal-fx.md`](../vocal-fx.html)（英文） | 加倍器、空气感激励器和闪避发送。 |
| [`vocal-fet.md`](../vocal-fet.html)（英文） | FET 压缩器：决定压缩比、拐点和失真的沟道电导规律。 |
| [`deesser-modes.md`](../deesser-modes.html)（英文） | 齿音消除器的相对阈值、全频段、检测器位置、可选频段、自动频段和分段门模式，以及逐位一致性与延迟的证明。 |
| [`plosive.md`](../plosive.html)（英文） | 爆破音检测器和动态低架：阈值、频段形状、实测验收、零延迟。 |
| [`resonance.md`](../resonance.html)（英文） | 频谱共振抑制器：STFT、突出度检测器、倒谱最小相位 FIR、实测深度和开销。 |
| [`multiband.md`](../multiband.html)（英文） | 多段压扩器及其分频器。 |
| [`limiter.md`](../limiter.html)（英文） | 真峰值母带限制器：上限、前视、释放和实测增益衰减。 |
| [`eq-editor.md`](../eq-editor.html)（英文） | 参数 EQ 和曲线编辑器，包括动态频段。 |
| [`fx-compressor.md`](../fx-compressor.html)（英文） | 合成器效果架的三段 RMS 压扩器、其输出端的前视限制器，以及混音器压缩器的检测侧链 EQ。 |
| [`fx-reverb-fdn.md`](../fx-reverb-fdn.html)（英文） | 合成器效果架的 FDN 混响：拓扑、两个频段衰减微调和记录在案的失效模式。 |
| [`breath-split.md`](../breath-split.html)（英文） | 呼吸声分离器：互补的线性相位分离，以及决定哪条路径占主导的检测器。 |
| [`tuner.md`](../tuner.html)（英文） | 吸附到音阶的音高校正器：YIN 检测、音符跟踪和重合成。 |
| [`reverb-fdn.md`](../reverb-fdn.html)（英文） | 长篇幅说明的 FDN 混响：两个网络、分频段衰减和早期反射表。 |
| [`plugin-support.md`](../plugin-support.html)（英文） | 托管插件：参数如何发现和建模，“已验证”的含义，以及已验证列表。 |
| [`devices/params/compression.md`](../devices/params/compression.html)（英文） | 七个长篇幅说明的信号设备的参数表：名称、单位、范围、默认值和刻度。 |

### 工程、编排与编辑

| 文档 | 内容 |
|---|---|
| [`arrangement.md`](arrangement.md) | 段落、重复，以及渲染长度如何确定。 |
| [`autosave.md`](autosave.md) | 恢复副本：存放位置、写入时机、保留数量、恢复和丢弃。 |
| [`export-format.md`](export-format.md) | 采样格式（`i16`/`i24`/`f32`）、抖动、确定性，以及哪个设置优先。 |
| [`audio-clip.md`](../audio-clip.html)（英文） | 音频片段：导入、峰值、裁剪、分割、流式播放。 |
| [`audio-clip-stretch.md`](../audio-clip-stretch.html)（英文） | 时间伸缩：派生媒体缓存、WSOLA、相位声码器。 |
| [`mixer-routing.md`](mixer-routing.md) | 轨道、总线与主输出的路由，发送，以及声像与平衡规律。 |
| [`multi-output-routing.md`](../multi-output-routing.html)（英文） | 多输出乐器和辅助返回轨道。 |
| [`midi-import-export.md`](midi-import-export.md) | MIDI 导入与导出，包括可取消的导入。 |
| [`midi-live.md`](midi-live.md) | 实时 MIDI 设备与路由。 |
| [`mpe.md`](../mpe.html)（英文） | 逐音符表情。 |
| [`automation.md`](automation.md) | 三种自动化通道形式、它们的曲线和平滑，以及它们如何作用到效果器参数。 |
| [`comping.md`](../comping.html)（英文） | 离线录音对齐和 `ocp clip align`：起音点、偏移、伸缩、极性和拼接交叉淡化。 |
| [`dsl-compat.md`](../dsl-compat.html)（英文） | 解析器仍然接受的旧效果器简写，以及它打印的警告。 |
| [`playback-latency.md`](../playback-latency.html)（英文） | 按下空格、跳转或试听如何到达第一个音频块，以及每种启动路径的开销。 |

### 平台与产品

| 文档 | 内容 |
|---|---|
| [`branding.md`](../branding.html)（英文） | 标志、窗口外框和文字标识。 |
| [`plugin-index.md`](../plugin-index.html)（英文） | 远程插件目录：格式、签名、发布。 |
| [`security.md`](../security.html)（英文） | 凭据存储和智能体的权限模型。 |
| [`release.md`](../release.html)（英文） | 如何构建安装包、冒烟测试、签名与公证，以及发布更新。 |
| [`agent-loop.md`](../agent-loop.html)（英文） | 智能体的分析、区间渲染和事务工具。 |
| [`content-and-programs.md`](../content-and-programs.html)（英文） | 内容库注册和插件音色程序选择，附各格式能力表和已知限制。 |
| [`au-hosting.md`](../au-hosting.html)（英文） | 进程内的 AudioUnit（AUv2）托管：哪些组件可以离线渲染，哪些只能扫描。 |
| [`vst2.md`](../vst2.html)（英文） | VST 2.4 宿主：发现、参数、预设和已知限制。 |
| [`vst3-editor.md`](../vst3-editor.html)（英文） | VST 3 编辑器：它需要的宿主约定，以及传送其音频的无锁监听。 |
| [`vst3-conformance.md`](../vst3-conformance.html)（英文） | 逐个插件包的一致性矩阵：测试组、子进程、JSON 结构和约定上的决定。 |
| [`diagnostics.md`](../diagnostics.html)（英文） | 诊断开关：哪种构建能报告什么，由哪个标志开启。 |
| [`compatibility.md`](../compatibility.html)（英文） | 兼容范围，以及代码和文档的原创性规则。 |

### 测量

| 文档 | 内容 |
|---|---|
| [`performance-targets.md`](../performance-targets.html)（英文） | 吞吐量和延迟目标、参考工程和测量方法。 |
