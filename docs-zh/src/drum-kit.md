# 鼓组 {#drum-kit}

`builtin.drumkit` 是一个鼓采样器，每个 MIDI 音符对应一个打击垫。它与 `builtin.sampler` 使用同一个采样解码器，并增加了打击垫网格、力度层、确定性轮换、止音组、逐垫包络、反向播放、声部抢占，以及为没有采样的打击垫提供的合成后备音色。

```ocp
@kit basic
  max_voices: 32
  pad kick        | note: C2, gain: -2dB, decay: 0.35s, one_shot: true, choke: 1
  pad snare       | note: D2, decay: 0.2s
  pad hat_closed  | note: F#2, gain: -3dB, choke: 2
  pad hat_open    | note: A#2, gain: -4dB, choke: 2, release: 0.08s
  layer kick  | low: 1,  high: 127, sample: "samples/kick.wav"
  layer snare | low: 1,  high: 63,  sample: "samples/snare_soft_a.wav"
  layer snare | low: 1,  high: 63,  sample: "samples/snare_soft_b.wav"
  layer snare | low: 64, high: 127, sample: "samples/snare_hard.wav"

@track drums | instrument: drumkit, preset: basic
  @pattern beat [1:1 - 2:1]
    kick 1:0 ff
    snare 2:0 mf
```

`instrument: drumkit` 是完整写法 `instrument: drums/drumkit` 的简写。`@kit` 块会变成一个 `source: builtin`、`plugin: "builtin.drumkit"` 的预设，其 `params` 以 `pad.<name>.*` 和 `layer.<name>.<index>.*` 条目保存打击垫和层。所有渲染路径（离线、逐轨、流式、JSON 导出）都以同样的方式读取鼓组。`ocp fmt` 会把鼓组写回为 `@kit` 块。

## 打击垫 {#pads}

| 字段 | 范围 | 默认值 | 含义 |
|---|---|---|---|
| `note` | MIDI 编号或音名 | 36 加上该打击垫在块中的位置 | 触发该打击垫的 MIDI 音符。 |
| `gain` | dB | `0` | 打击垫音量，线性，不做归一化。 |
| `pan` | −1 到 1 | `0` | 平衡：居中时两侧都是满音量，偏向一侧时降低另一侧。 |
| `tune` | 整数半音 | `0` | 移调。 |
| `cents` | 音分 | `0` | 微调。 |
| `decay` | 秒 | `0` | 在这段时间内衰减到 −60 dB。`0` 保留采样自身的包络。 |
| `release` | 秒 | `0.005` | 门限模式下音符关闭后的释放时间。 |
| `reverse` | `true` 或 `false` | `false` | 反向播放采样。采样在解码时反转一次。 |
| `one_shot` | `true` 或 `false` | `true` | 忽略音符关闭。`false` 为门限模式。 |
| `choke` | 0 到 127 | `0` | 止音组。`0` 表示不属于任何组。 |
| `voice` | `kick`、`snare`、`hat_closed`、`hat_open`、`tom`、`clap`、`rim` | 先看打击垫名称，再看音符 | 打击垫没有可播放的层时使用的合成音色。参见 [Drum synthesis](../drum-synthesis.html)（英文）。 |

没有可播放层的打击垫会播放合成音色，所以只写了打击垫的鼓组也能发声。音色依次取自 `voice`、打击垫名称和 General MIDI 鼓音符。如果打击垫名称不是音色名，就写上 `voice`，例如 `pad snare_top | note: D2, voice: snare`。未知的 `voice` 会被报告并忽略。既没有可播放的层、也没有音色的打击垫保持无声，并报告一次。

每个打击垫有一组层：

| 层字段 | 范围 | 默认值 | 含义 |
|---|---|---|---|
| `low`、`high` | MIDI 力度 | `1`、`127` | 力度范围。 |
| `sample` | 路径 | 必填 | WAV 或 FLAC 文件，相对于工程文件夹解析。 |
| `gain` | dB | `0` | 层的音量微调。 |

所有采样都在构建鼓组时解码。缺失或无法读取的文件，每个路径只产生一条警告，对应的层保持无声；工程照常加载和渲染。`DrumKit::warnings()` 返回同样的列表。

乐段中的一次击打写的是打击垫的名称，例如 `kick` 或 `snare`。鼓组中没有该名称的打击垫时，这次击打使用该名称对应的 General MIDI 音符（不是 General MIDI 鼓名的名称使用音符 38）。渲染时会按轨道和击打名称各报告一次，附上击打次数，并说明是否有打击垫占用该音符，还是这次击打没有声音。参见 [Render warnings](../render-warnings.html)（英文）。

## 力度层与轮换 {#velocity-layers-and-round-robin}

层的选择是确定的，与时间无关：

1. 取该打击垫中力度范围 `low` 到 `high` 包含当前力度的可播放层。
2. 每个打击垫有一个触发计数器。选中的层为 `matching[counter % matching.len()]`，然后计数器加一。
3. 如果没有任何范围包含该力度，就使用范围最接近的可播放层；距离相同时，先声明的层优先。此时计数器不变。

因此：

- 互不重叠的范围就是力度层：轻击播放轻的采样，重击播放重的采样。
- 范围相同的多个层组成一个轮换组：以相同力度反复击打时，按声明顺序轮流播放。
- 播放顺序只取决于击打的先后。同一个乐段渲染两次，得到的顺序相同；不涉及时钟、随机数或音符时值。
- 只有可播放的层参与选择，所以缺了一个文件不会让还有其他采样的打击垫失声。

音量为 `velocity / 127` 乘以打击垫增益和层增益。各层不做电平匹配：两个以不同电平录制的采样会保持原有的差距。

## 止音组 {#choke-groups}

- 每个声部记住启动它的打击垫所属的 `choke` 组。
- 在 `choke` 不为 `0` 的打击垫上触发音符时，会先在 4 ms（`CHOKE_FADE_SECONDS`）内淡出同组所有活动的声部，再启动新声部。闭镲会切断开镲；同一组中某个打击垫的第二次击打会切断第一次。
- 止音组在整个鼓组内共享，与打击垫名称无关。
- 被止音的声部会移到一个淡出槽中，线性衰减到零，所以切断时不会有爆音。声部抢占也使用这些槽。
- `choke: 0` 的打击垫既不会切断其他打击垫，也不会被切断。

## 复音与声部抢占 {#polyphony-and-voice-stealing}

- 一个鼓组最多 64 个声部（`MAX_VOICES`）。`max_voices` 设定每个鼓组的声部数，默认 32。
- 另外还有 32 个淡出槽，用来存放正在被止音或被抢占的声部，新音符从不使用它们。
- 所有声部都占满时，抢占当前输出最小（包络电平 × 增益）的声部；输出相同时抢占最早的那个。它在淡出槽中用 8 ms（`STEAL_FADE_SECONDS`）淡出，新音符占用它腾出的位置。
- 淡出槽也全部占满时，会丢弃输出最小的那个正在淡出的声部。只有这种情况才会截断正在淡出的声部，而这需要同时有超过 32 次止音或抢占。

## 包络与电平规律 {#envelope-and-level-laws}

下表数值来自 48 kHz 下的 `oc-synth` 和 `oc-engine` 测试。

| 量 | 规律 | 测试结果 |
|---|---|---|
| 起音 | 线性，0.5 ms | 24 帧 |
| 衰减 | `level *= 0.001^(1 / (decay × rate))`，在 `decay` 内衰减 −60 dB | `decay: 0.1s`：第 30 帧为 0.500，第 4,790 帧为 −60.0 dB |
| 门限释放 | 从音符关闭时的电平开始，在 `release` 内线性下降 | 10 ms 释放在 480 帧后无声 |
| 止音淡出 | 线性，4 ms | 192 帧后低于 0.001（−60 dBFS）；相邻采样间的最大跳变小于 0.01 |
| 抢占淡出 | 线性，8 ms | 相邻采样间的最大跳变小于 0.02；被移到淡出槽的是最轻的声部（力度 10） |
| 声部结束 | `level <= 1e-5` 时声部停止 | 相对满幅 −100 dB |
| 力度 | `velocity / 127`，线性 | 电平为 0.1 和 0.4 的两个层在力度 63 和 64 时保持 4 倍（12 dB）的差距 |
| 音高 | `2^((tune + cents/100) / 12)` × 文件采样率 / 输出采样率 | 升高一个八度，播放比率加倍 |
| 时间 | 击打落在工程时钟上 | 120 BPM 时，底鼓落在第 0 帧，军鼓落在第 24,000 帧 |

反向在解码采样时只做一次，所以音频回调中不需要为它做判断。

## 渲染与导出 {#rendering-and-export}

- 离线渲染（`render_cache::render_builtin_project`）把 `builtin.drumkit` 轨道交给 `oc_synth::drumkit::try_render_track_with_warnings`。
- 流式播放使用 `BlockSource::DrumKit`，它只解码一次，渲染各块时不分配内存。定位会重置所有声部和所有轮换计数器。
- `RenderCache` 记录每个 `layer.*.sample` 文件的状态，所以替换磁盘上的采样会把该轨道标记为已更改并重新渲染。
- 报告给渲染器的尾音长度是最长的采样加上打击垫的释放时间；没有采样的打击垫则取合成音色的尾音，最多 30 秒。
- `ocp fmt` 保留 `@kit` 块，`ocp export --format json` 会输出该预设及其 `pad.*` 和 `layer.*` 参数。

## 鼓组面板 {#the-drum-kit-panel}

底部面板中的**鼓组**（`K`）显示选中轨道的鼓组。如果选中的轨道没有鼓组，就显示选中的鼓组预设，再不然显示工程中的第一个鼓组。标题栏显示鼓组名称、打击垫数量和声部数。

- 网格至少显示 16 个打击垫，按音符排序。鼓组可以声明更少的打击垫（空位无声，可以拖入采样），也可以更多（网格随之扩展）。
- 每个打击垫显示名称、第一个采样的文件名、音符、层数和轮换采样数，以及止音组。
- 单击打击垫会通过鼓组乐器以力度 110 播放它、让它闪烁并选中它。
- 把 WAV 或 FLAC 文件拖到打击垫上，会替换它第一层的采样。拖到空位上会在下一个空闲音符处新建一个打击垫。
- 网格旁边的检查器可以编辑**增益 dB**、**声像**、**调音 st**、**音分**、**衰减 s**、**释放 s**、**止音组**、**模式**（**单次触发**或**门限**）和**反向**，并列出各层及其力度范围。
- 来自实时 MIDI 输入的音符开启会让对应的打击垫闪烁。

每次拖入采样和每次在检查器中编辑，都使用与应用其他部分相同的预设命令，所以都可以撤销。

## 不支持的功能 {#not-supported}

- 时间拉伸或跟随速度。打击垫始终以录制时的速度播放，只按 `tune` 和 `cents` 移调。
- 多于一个维度的层，例如多支麦克风（轮换只是单个触发计数器）。
- 逐垫的滤波器、均衡、压缩或插入效果器；请使用轨道的效果链。
- 从磁盘流式读取采样或在后台解码；鼓组必须能完整放进内存。
- 用 MIDI CC 或触后改变打击垫参数，以及每个打击垫单独输出。
- 导入其他软件的鼓组格式。
- 在面板中修剪波形或设置循环点，以及工程撤销之外的任何采样历史。

## 验证 {#verification}

```sh
cargo test -p oc-synth drumkit
cargo test -p oc-engine drumkit
cd crates/oc-gui/frontend && npx vitest run src/__tests__/drumkit-utils.test.ts \
  src/__tests__/components/DrumKitPanel.test.tsx
```

测试覆盖止音时间、单次触发与门限模式、力度层、确定性轮换、声部抢占、缺失采样、反向播放、衰减规律、合成后备音色、DSL 往返和 JSON 导出，以及一次完整渲染中击打是否落在预期的帧上。
