# `builtin.sampler`（SFZ 子集） {#builtinsampler-sfz-subset}

`builtin.sampler` 是引擎内置的基于文件的采样器。工程预设可以直接使用它，`builtin.open_synth` 中导入的采样振荡器层和多重采样振荡器层也通过它发声。

## 支持的输入 {#supported-input}

- WAV（8、16、24、32 位 PCM，以及 32 位浮点）。
- FLAC。
- 解码器按文件扩展名（`.wav`、`.wave`、`.flac`）选择。其他扩展名的文件如果以 `fLaC` 开头就按 FLAC 读取，否则按 WAV 读取。
- 超过两个声道的文件只保留前两个声道。
- 单个解码后的采样最多 64 Mi 个采样值，SFZ 文件最大 4 MiB。
- 采样的采样率可以与工程不同。播放时用线性插值重采样，所用的比率综合了音符、区域的调音以及文件采样率与输出采样率之比。
- 所有文件都在构建采样器时解码。`Sampler::next_stereo` 既不分配内存，也不解码。

## 支持的 SFZ 子集 {#supported-sfz-subset}

头部：

- `<control>`：只支持 `default_path`，即其后各采样路径的文件夹前缀；
- `<global>` 和 `<group>`：其后各区域继承的默认值，group 的值优先于 global；
- `<region>`：一个采样区域。

| 操作码 | 含义 |
|---|---|
| `sample` | 采样路径，相对于 SFZ 所在文件夹或某个搜索根目录。带空格的路径不必加引号。 |
| `key`、`lokey`、`hikey` | 键位区域，可写 MIDI 编号或音名（`C4` 为 60）。没有 `pitch_keycenter` 时，`key` 同时设定根音。 |
| `lovel`、`hivel` | 力度区域。 |
| `pitch_keycenter` | 根音。既没有它也没有 `key` 时，根音为 60。 |
| `tune` | 调音，单位为音分。 |
| `volume` | 增益，单位为 dB。 |
| `pan` | −100 到 100。 |
| `amp_veltrack` | 力度对音量的影响程度，0 到 100%，默认 100。 |
| `offset` | 开始播放的帧。 |
| `end` | 停止播放的帧。 |
| `loop_start`、`loop_end` | 以帧为单位的循环点。不写时循环整个采样。 |
| `loop_mode` | `no_loop`（默认）、`one_shot`、`loop_continuous`、`loop_sustain` 或 `loop_pingpong`（也接受 `pingpong`）。 |
| `seq_position`、`seq_length` | 轮换位置和循环长度；整个采样器所有区域共用一个序列。 |
| `group`、`off_by` | 止音组。 |
| `ampeg_attack`、`ampeg_decay`、`ampeg_release` | 振幅包络时间，单位为秒。释放默认 0.05 秒。 |
| `ampeg_sustain` | 延音电平，取值为 0 到 1 的比例。大于 1 的值按 1 处理，所以像 `50` 这样的 SFZ 百分比会得到完全延音。 |
| `ampeg_release_shape` | 释放曲线的弯曲程度，−20 到 20；默认 0，即直线淡出。 |

解析器会把所有不支持的头部和操作码，以及所有无法使用的区域（没有 `sample`、找不到文件、区域上下颠倒）收集到 `SamplerPatch::diagnostics` 中，并在构建采样器时记入日志。它绝不会把不支持的操作码当作别的功能来解读。

## 力度、调音与弯音 {#velocity-tuning-and-pitch-bend}

在工程预设中，力度按 `1 − (1 − velocity / 127) × amp_veltrack / 100` 决定音量。导入的振荡器层沿用其来源格式的力度规律。

`pitch_keycenter` 设定区域的根音，`tune` 以音分为单位偏移它。音符、调音和文件采样率合成为一个播放比率，所以以其他采样率录制的采样仍会以写定的音高播放。

弯音范围为 ±2 个半音，作用于整个通道：一次弯音会移动该通道上所有按住的声部，包括叠加的区域。单个音符的微调请写在 `tune` 中，不要在同一通道上再加一次弯音。

采样器还响应延音踏板（CC 64）、全部声音关闭（CC 120）和全部音符关闭（CC 123）。

## 循环、释放与轮换 {#loops-release-and-round-robin}

- `no_loop` 播放到 `end`，收到音符关闭时进入释放。
- `one_shot` 忽略音符关闭，播放到 `end` 或采样末尾。
- `loop_continuous` 在循环点之间循环，直到释放结束。
- `loop_sustain` 在按住琴键期间循环。音符关闭后循环停止，释放开始，采样从当前位置继续往后播放。
- `loop_pingpong` 先正向播放到循环终点，再反向播放到循环起点，然后再正向播放。它和 `loop_continuous` 一样，音符关闭后仍继续循环，直到释放结束。

循环可以结束在采样的最后一帧；省略 `loop_end` 时，循环结束在区域末尾。正在运行的循环在那里回绕或掉头，而不会结束声部；已释放的延音循环可以播放到末尾。

除 `one_shot` 外，所有模式都在音符关闭或松开延音踏板时开始释放（`ampeg_release`）。

`ampeg_release_shape` 弯曲释放曲线。为 0 时淡出是一条直线；负值开始时下降更快，正值更慢。释放过程中电平遵循 `1 - expm1(shape × progress) / expm1(shape)`，在 0 处退化为直线。它可以写在区域、组或全局头部中。

轮换使用整个采样器共用的一个计数器，每次音符开启都会让它前进一步。在与某个音符匹配的区域中，循环长度取最大的 `seq_position` 或 `seq_length`。`seq_position` 等于计数器在该循环中所处位置的区域会发声，所有没有 `seq_position` 的匹配区域也一起发声。

## 止音与复音 {#choke-and-polyphony}

`off_by` 用于止音：当某个音符的区域带有 `off_by = N` 时，所有区域为 `group = N` 的声部都会停止。停止是立即的，没有淡出，所以可能产生咔嗒声。同一个音符触发的区域按顺序启动，所以当两个区域共用一个止音组时，一个区域可能止住同一音符较早的层。在区域止住自己所在的组之前，请检查分层映射。鼓组的 `choke` 字段则会淡出，参见[鼓组](drum-kit.md)。

采样器有 64 个声部。全部占满时，新音符会替换最早的声部。

播放回退时（例如定位），采样器会重置：所有声部停止，轮换计数器、弯音和延音恢复为初始值。已解码的采样仍保留在内存中。

## 工程参数 {#project-parameters}

```ocp
@preset pad
  source: builtin
  plugin: "builtin.sampler"
  params: {
    sampler.sfz_file: "pad.sfz",
    sampler.root: "samples",
    sampler.gain: 1.0
  }
```

预设用以下三个参数之一指定来源。同时设置了多个时，按列表顺序以第一个为准：

| 参数 | 来源 | 采样搜索文件夹 |
|---|---|---|
| `sampler.sfz_text` | 内联的 SFZ 文本，保存在已存的音色补丁中。 | 工程文件夹，然后是 `sampler.root`。 |
| `sampler.sfz_file` | 一个 SFZ 文件。 | SFZ 文件所在的文件夹，然后是 `sampler.root`。 |
| `sampler.sample_file` | 覆盖整个键盘的单个采样。 | 无，直接使用该路径。 |

使用 `sampler.sample_file` 时，以下参数也会生效：`sampler.root_note`（默认 60）、`sampler.loop_start`、`sampler.loop_end` 和 `sampler.loop_mode`。无论哪种来源，`sampler.gain`（线性，默认 1.0）和 `sampler.pan`（−1 到 1）都作用于整个音色补丁。

相对路径形式的 `sampler.sample_file`、`sampler.sfz_file` 或 `sampler.root` 都相对于工程文件夹解析。这条规则及其拒绝情况见 [.ocp 文件](manual/dsl.md)中的“预设”部分。不存在的 `sampler.sample_file` 会报错。采样器把最长的释放时间报告为尾音长度，最多 30 秒。

导入的预设把它的 SFZ 文本保存在 `SamplerPatch::sfz` 中，所以没有原始预设文件也能重建映射后的音色补丁。它的采样路径相对于导入预设的内容文件夹及其多重采样文件夹解析。

## 把带标记的 WAVE 文件作为切片播放 {#play-a-marked-wave-file-as-slices}

在 WAVE 文件旁生成一个可编辑的 SFZ，然后把它用作预设的 `sampler.sfz_file`。

```sh
ocp sample slices media/loop.wav --first-key 60 > media/loop.sfz
```

命令按帧顺序为每个提示点分配一个琴键。每个切片结束于下一个提示点或文件末尾。用 `--gated` 让切片在音符关闭时释放，用 `--json` 查看琴键与帧的映射。编辑 SFZ 中每个 `offset` 和 `end` 即可微调切点，无需修改音频。

## 已知的导入缺口 {#known-import-gaps}

- OpenSynth 支持[颗粒与频谱振荡器](../osc-granular-spectral.html)（英文）和[采样层齐奏](../synth-levels.html)（英文）。导入预设需要能访问采样文件：只含嵌入音频、没有文件路径的振荡器不受支持。
- 采样层遵循所属声部的滤波器路由和振幅包络，然后送入合成器效果器架。
- SFZ 滤波器、`pitch_keytrack`、键位切换、释放采样、`<master>` 和 `<effect>` 头部、超出单个共享计数器的轮换，以及力度层之间的交叉淡化都没有实现。
- 除 `ampeg_release_shape` 外，`ampeg_*` 包络的各段是线性的，不是 SFZ 的曲线形状。
