# 内置设备

OpenCompose 自带一套混音器设备库。引擎、GUI 的设备选择器和 `ocp devices list` 读取的是同一个注册表（`crates/oc-device-schema/src/lib.rs`），所以在其中一处列出的设备，在其他地方同样可用。

在 GUI 中，点击设备架上的 `+` 添加设备。在 DSL 中，在 `effects:` 下添加一项：

```ocp
effects:
  - compression: ratio=3:1, threshold=-12dB, attack=5ms, release=80ms
```

## 参数写法

参数值可以是纯数字，也可以带单位（`dB`、`%`、`Hz`、`ms`、`s`、`ct`）。写了设备未定义的参数名会导致渲染错误。

任何一项加上 `bypass=true`，就会把该设备移出效果链，其控制值保持不变；参见 [.ocp 文件](dsl.md)中的“效果器”一节。

在 `compression`、`dynamic_eq` 和 `ducked_send` 上，`sidechain=<name>` 让检测器读取另一个通道的信号。轨道上的效果器可以用一条轨道作为键控源；总线或 master 上的效果器可以用轨道或总线作为键控源。键控信号取自源的插入效果器之后、推子之前、静音之前，并且永远不会进入音频输出。

### 小数与百分比

取值为小数的控制使用纯物理数值。`compression: mix=0.4` 和 `multiband: mix=0.4` 都表示 40 % 湿声，GUI 中显示为 `40%`。本身就是百分比的控制（例如混响的 mix）写作 `mix=40%`。OpenSynth 的小数控制遵循同样的显示规则，包括双极性的调制量和大于 1 的值。

静态混音效果器也接受在小数控制上写百分比：对压缩器和多段压缩来说，`mix=40%` 表示 `0.4`。因此 `mix=0.4%` 表示 `0.004`（0.4 % 湿声），绝不会被当作 40 % 湿声。新的编辑请写纯小数，这样存储的数字与目录一致。

有几种场合按各自的方式解读百分比写法：

- 这些小数控制上的效果器自动化把 `0.4%` 读作物理值 `0.4`。
- OpenSynth 的静态音色补丁参数保留百分比的原始数字。
- 乐器自动化有自己的归一化百分比换算。

在这些场合，给设备和合成器的小数控制写纯物理数值，并查看目标的参数目录。

## 选择设备

| 设备 | DSL 关键字 | 是什么 | 适用场合 |
|---|---|---|---|
| Gain | `gain` | 固定的线性增益（`factor`）。 | 只需要微调电平。 |
| Parametric EQ | `eq` | 三段均衡器：低频搁架、中频峰值、高频搁架。 | 用固定布局快速塑造音色。 |
| Passive EQ | `passive_eq` | 低频和高频段的峰值与搁架相互耦合，另有倾斜和空气感搁架。 | 想要控制相互耦合的节目均衡器。 |
| Dynamic EQ | `dynamic_eq` | 六个频段加高低切，每个频段都可设为动态。 | 某个频段只在变响时才需要处理。 |
| Compression | `compression` | 通用压缩器，带侧链输入和可滤波的检测器。 | 控制动态、粘合总线、跟随底鼓闪避。 |
| Multiband Compander | `multiband` | 二到四个频段，每段独立压缩，可调分频点。 | 单个压缩器会让某个频段抽吸或发闷。 |
| De-esser | `de_esser` | 分频或全频的齿音控制。 | 人声或镲片上有刺耳的 `s`、`sh` 音。 |
| Leveler | `leveler` | 以控制速率跟随响度调节增益，带门限。 | 让演奏电平均匀，又不压平瞬态。 |
| Opto Compressor | `opto` | 单旋钮光学压缩器。 | 人声或贝斯需要平滑、随节目变化的电平控制。 |
| FET Compressor | `fet` | 场效应管反馈压缩器，`bias` 决定压缩比。 | 人声或贝斯压得越狠，染色越多。 |
| Limiter | `limiter` | 带预读和真峰值检测的峰值限幅器。 | 在 master 或总线上抓峰值。 |
| Gate | `gate` | 带阈值、保持和范围的向下扩展器。 | 去除串音，或收紧有噪声的音源。 |
| Saturator | `saturator` | 驱动和混合。 | 增加厚度或谐波，电平变化不大。 |
| Iron | `iron` | 变压器风格的非对称饱和，带频带限制。 | 加厚低频，增加谐波密度。 |
| Distortion | `distortion` | 驱动、音色和输出。 | 需要明显的染色和棱角。 |
| Reverb | `reverb` | 带阻尼和滤波器的算法空间。 | 用少量控制得到一个可用的空间。 |
| FDN Reverb | `reverb_fdn` | 长混响，控制空间、衰减速率、明亮度、特性和距离。 | 不只调长度，还要塑造空间本身。 |
| Delay | `delay` | 左右时间独立的立体声延迟。 | 回声和节奏性的空间感。 |
| Chorus | `chorus` | 调制叠加。 | 给铺底、吉他或人声增加宽度和动感。 |
| Flanger | `flanger` | 带反馈的短调制延迟。 | 扫频的喷气式音色。 |
| Phaser | `phaser` | 调制的全通陷波。 | 比镶边更柔和的动感。 |
| Stereo Width | `stereo_width` | 中/侧宽度，100 % 时信号不变。 | 展宽立体声音源，或收成单声道。 |
| Doubler | `doubler` | 带聚焦高通的短失谐延迟声部。 | 加厚人声或吉他，同时低频保持单声道。 |
| Air Exciter | `exciter` | 从已有的中高频内容合成空气感频段。 | 不用均衡搁架也能让声音更通透。 |
| Ducked Send | `ducked_send` | 随干声压低的混响返回。 | 让空间感避开歌词。 |
| Resonance Suppressor | `resonance` | 削减窄共振的频谱动态处理。 | 有随音符移动的啸叫、哨音或箱声。 |
| Tuner | `tuner` | 吸附到音阶的音高校正，使用 PSOLA 重合成。 | 人声需要修音，又不能改变共振峰。 |
| Breath Split | `breath_split` | 带周期性检测器的线性相位分频。 | 减弱或提升呼吸声，而不对整条录音做门限。 |
| Plosive | `plosive` | 低频爆破检测器驱动动态低频搁架。 | 去掉 `p`、`b`、`t`、`k` 的喷麦声，又不让元音变薄。 |

`ocp devices list --json` 输出每个设备的完整参数列表。

## 动态

Compression 是通用压缩器。它的控制有 `threshold`、`ratio`、`attack`、`release`、`knee`、`range`、`makeup`、`lookahead`、`max_lookahead`、`detector`（可以写，但检测器始终是峰值检测）、`detector_hpf`、`stereo_link`、`hold`、`auto_release`、`auto_makeup` 和 `mix`。`ratio` 写作 `3:1`。

六个可选的检测器频段（`sidechain_eq1_on` 到 `sidechain_eq6_q`）只过滤检测器读取的信号，不影响音频本身。例如把 `sidechain_eq1_type` 设为 `high_shelf`、频率 6 kHz、`sidechain_eq1_gain` 为 12 dB，压缩器就像齿音消除器一样工作。`low_cut` 频段可以让低音不进入检测器。这些频段默认关闭，所以没有设置它们的工程渲染结果不变。

Multiband 把信号分成 `band_count` 个频段（`two_bands`、`three_bands`、`four_bands`），分频点为 `crossover_1..3`，另有 `slope`（`6db` 到 `48db`）和 `phase_mode`（`minimum`、`dynamic`、`linear`）。每个频段有 `enabled`、`threshold`、`range`、`ratio`、`attack`、`release`、`knee`、`makeup`、`lookahead`、`solo` 和 `mute`。`mix` 控制整个设备的干湿比。

- 设备的 `lookahead` 为零时，信号路径不增加延迟，设备也不报告延迟。非零时，所有频段经过同一条延迟线，并报告这段延迟。频段自己的 `lookahead` 只改变该频段检测器的读取位置。
- `solo` 把其他频段从总和中去掉，用来单独试听一个频段。`mute` 总是优先于 `solo`。

频段检测器和外部键控见 [Multiband compander](../../multiband.html)（英文）。

De-esser 使用 `threshold`、`range`、`attack`、`release`、`high_frequency`、`low_frequency`、`band_processing`（`wide_band` 或 `split_band`）、`stereo_link` 和 `lookahead`。它还有以下模拟模式：

- `threshold_mode`（`absolute` 或 `relative`）和 `relative_threshold`。相对模式把频段电平与缓慢变化的节目电平比较。
- `detector`（`sidechain` 或 `in_band`）。频段内模式把检测器移到分频之后。
- `sibilance_band`（`auto`、`5_khz` 到 `9_khz`）。选定频段会同时移动侧链拐点和分频点。
- `band_estimation`（`off` 或 `learn`）和 `band_override`（Hz）。`learn` 从录音中找出频段。
- `segment_detection`（`off` 或 `on`）。`on` 只在齿音片段上做衰减。

默认值与随附的设备一致。各模式的说明见 [De-esser modes](../../deesser-modes.html)（英文）。

Limiter 有以下控制：

- `ceiling`，−12 到 0 dB。
- `release` 和 `release_mode`（`fast`、`auto`、`punch` 或 `sustain`）。
- `lookahead`，0 到 20 ms。
- `true_peak`，默认打开。
- `oversampling`：`4x`、`8x` 或 `16x`。默认 4x，沿用原有的检测器。
- `link`，0 到 100 %。默认 100 % 时由一个检测器同时控制两个声道；0 % 时每个声道按自身电平限幅。

延迟对齐会补偿预读，所以经过限幅的 master 不会错位。释放方式和测量数据见 [Mastering limiter](../../limiter.html)（英文）。

Gate 是噪声门，控制有 `threshold`、`attack`、`hold`、`release` 和 `range`。

Leveler 以控制速率调节增益。控制有 `target`（目标响度）、`speed`（dB/s）、`range`、`window`（响度积分时间）、`gate` 和 `gate_release`。它保留音源的峰值因数。

Opto 是光学压缩器。`peak_reduction` 决定压缩力度，另有 `release` 和 `makeup`。参见 [Vocal compression devices](../../vocal-comp.html)（英文）。

FET 的控制有 `drive`、`attack`、`release`、`output` 和 `bias`（0 到 100 %）。`bias` 决定压缩比所处的位置；具体规律见 [FET compressor](../../vocal-fet.html)（英文）。

## 均衡器 (EQ)

Parametric EQ（`eq`）是固定的三段布局：`low_freq`、`low_gain`、`mid_freq`、`mid_gain`、`q`、`high_freq` 和 `high_gain`。

Passive EQ（`passive_eq`）包含以下部分：

- 共用一个拐点的低频提升峰值和低频衰减搁架（`low_freq`、`low_boost`、`low_cut`）。
- 高频峰值（`high_freq`、`high_boost`、`high_q`）。
- 高频搁架衰减（`high_cut_freq`、`high_cut`）。
- 以 1 kHz 为中心的 `tilt`。
- `air` 搁架（`air_freq`、`air`）。

Dynamic EQ（`dynamic_eq`）有六个相同的频段槽位，外加专用的高低切。

- 每个频段有 `enabled`、`solo`、`channel`（`stereo`、`mid`、`side`、`left`、`right`）、`type`（`bell`、`low_shelf`、`high_shelf`、`low_cut`、`high_cut`、`notch`、`band_pass`）、`freq`、`q` 和 `gain`，还可以设置 `dynamic`、`threshold`、`range`、`attack`、`release` 和 `sidechain`。
- 一个 `sidechain=` 源供给所有打开了 `sidechain` 开关的频段。频段的 `channel` 决定它处理哪一部分信号。
- 切滤波器有 `low_cut_enabled`、`low_cut_freq` 和 `low_cut_q`，`high_cut` 也有同样三项。
- `auto_gain` 补偿电平。

在 GUI 中，这个设备打开后是完整的曲线编辑器（见[主窗口](gui.md)）。

已停用的 `eq: high_shelf=…`/`low_cut=…`/`notch=…` 简写会展开到这个设备的静态频段和切滤波器上；参见 [DSL compatibility](../../dsl-compat.html)（英文）。

## 饱和

Saturator 只有两个控制：`drive` 和 `mix`。Iron 增加了一级变压器，控制有 `drive`、`bias`、`ceiling`、`low_freq`、`high_freq` 和 `output`。Distortion 有 `drive`、`tone`、`output` 和 `mix`。

## 混响、延迟与调制

Reverb 有 `decay`、`predelay`、`damping`、`mix`、`modulation`、`early_late`、`stereo_width`、`low_cut` 和 `high_cut`。`predelay` 范围为 0 到 300 ms，所以湿声最多可以比干声晚 0.3 秒进入。

FDN Reverb（`reverb_fdn`）有六个实测衰减频段和一个六段输出均衡器。

- `space` 是大小控制，`decay` 以 ms 回读它（200 ms 到 10 s）。
- `brightness`（0 到 100 %）倾斜六个衰减频段。0 % 时高频暗，50 % 时与实测参考一致，100 % 时尾音平直、完全打开。
- `character` 承载调制，`distance` 承载早期反射的比例。
- `damping`（100 − `brightness`）、`modulation`（等于 `character`）和 `early_late`（100 − `distance`）是这些控制的兼容读数。每一对只设置其中一个。存储的字段是 `space`、`brightness`、`character` 和 `distance`；预设若同时带有一对的两边，以存储字段为准。
- 其他控制有 `decay_rate`、`predelay`、`stereo_width` 和 `mix`；六个 `decay_band1…6` 微调（dB，默认 0）及其位置（`decay_bandN_freq`、`decay_bandN_q`、`decay_bandN_shape`：`none`、`low_shelf`、`bell` 或 `high_shelf`）；以及六段后置均衡器（`post_eqN_freq`、`post_eqN_gain`、`post_eqN_q`、`post_eqN_shape`：`bell`、`low_shelf`、`high_shelf`、`low_cut`、`high_cut` 或 `notch`）。

设计和测量见 [FDN reverb](../../reverb-fdn.html)（英文）。

Delay 有 `time_l`、`time_r`、`feedback` 和 `mix`。Chorus、Flanger 和 Phaser 都有 `rate`、`depth` 和 `mix`；Flanger 和 Phaser 另有 `feedback`。

Stereo Width 的 `amount` 是百分比，范围 0 到 200 %。`100 %` 时信号不变，`0` 为单声道，`200 %` 时侧信号加倍。设备面板像显示其他控制一样显示它。`eq` 和 `passive_eq` 的曲线视图以及 `dynamic_eq` 的频谱分析仪见 [Parametric EQ curve editor](../../eq-editor.html)（英文）。

## 人声

Doubler 有 `voices`、`delay`、`rate`、`depth`、`detune`（音分）、`focus`（让低频保持单声道的高通）和 `mix`。

Air Exciter 有 `amount`、`drive`、`character`、`low_freq`、`high_freq` 和 `output`。

Ducked Send 有 `predelay`、`depth`、`attack`、`release`、`threshold` 和 `width`。用 `sidechain=` 选择干声键控源。

Resonance Suppressor 有 `threshold`（突出程度，dB）、`depth`（最大衰减量）、`selectivity`（频段数的代理量，12 到 52 个频段）、`attack`、`release`、`mix` 和 `enabled`。它运行 2048/256 的 STFT，报告 48 ms 延迟；旁通时报告 0。规律和实测表格见 [Resonance suppressor](../../resonance.html)（英文）。

Tuner（`tuner`）是吸附到音阶的音高校正器。

- `amount` 范围 0 到 100 %；为 0 时设备是逐位一致的旁通。
- `retune` 是 10 到 300 ms 的单极点时间。
- `key` 和 `scale` 决定目标音。共有十二种音阶，从 Chromatic 到 Blues。
- `mode` 在范围与延迟之间取舍。**Full Range** 最低检测到 80 Hz，报告 40 ms 延迟；**Low Latency** 从 120 Hz 起检测，报告 20 ms 延迟。
- `enabled` 开关设备。

检测在信号的 16 kHz 副本上运行 YIN。目标音只在起音处、或音高离开 60 音分的范围时才改变，PSOLA 重合成保留共振峰。实测检测误差在 880 Hz 以下不超过 0.39 音分，信噪比 6 dB 的气声音也不会产生高八度的帧。规律、常数和表格见 [Tuner](../../tuner.html)（英文）。

Plosive（`plosive`）有以下控制：

- `amount`，0 到 100 %；为 0 时是逐位一致的旁通。
- `threshold`，2 到 24 dB 的上升量（默认 6）。
- `frequency`，低频搁架拐点，80 到 150 Hz（默认 120）。
- `release`，10 到 300 ms（默认 40）。
- `monitor`。

设备把 24 dB/oct 的低频段和 400 Hz 的瞬态频段分别与缓慢包络比较。只有多出的能量集中在低频段时，爆破才会触发，因此元音起音或齿音不会触发它。触发后保持 20 ms，驱动一个起音 1.5 ms 的最小相位低频搁架。

在 amount 为 100 时实测：一个 S8 爆破在 120 Hz 以下读数为 −7.94 dB，500 Hz–2 kHz 的元音主体变化 −0.000 dB。在 110 Hz 的唱音上出现爆破时，基频偏差在 0.000 音分以内。报告的延迟为 0，`process` 不分配内存。规律、阈值和完整的验收表见 [Plosive](../../plosive.html)（英文）。

Breath Split（`breath_split`）有 `breath_level`、`voice_level`、`threshold`、`smoothing`、`monitor` 和 `split_frequency`（1.5 到 6 kHz，默认 2 kHz）。一个 257 抽头的线性相位低通及其精确互补滤波器把录音一分为二。周期性检测器（30 ms 窗口，10 ms 步进）判断哪些窗口是呼吸声，只在这些窗口上应用 `breath_level`。增益为 1 时，两路相加等于延迟 128 个采样点的输入。默认设置下设备是逐位一致的旁通，报告的延迟为零。在 4–8 kHz 的呼吸声上请求 −12 dB，实测为 −11.998 dB。规律、检测器和限制见 [Breath split](../../breath-split.html)（英文）。

建议的人声顺序是：tuner、leveler、清理用 EQ、plosive、breath split、opto、de-esser、passive EQ、iron、resonance suppressor、doubler、exciter、空间效果。这个顺序中的每个设备都已随附。各设备的规律和这样排序的理由见 [Vocal chain](../../vocal-chain.html)（英文）。

## 设备架

效果器按它们在设备架中从上到下的顺序运行。新添加的设备放在效果链末尾。要调整顺序，右键点击一个槽位，选择**上移**或**下移**；设备架不支持拖拽排序。要删除设备，点击槽位上的 `x`，或在同一菜单中选择**移除效果器**。总线的效果器在检查器中编辑；总线没有单独的插入设备架。
