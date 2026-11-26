# 导出：容器、位深与抖动 {#export-containers-sample-depth-and-dither}

混音器从每个音源到主输出全程使用 32 位浮点采样。导出时，你要选择一种容器，并决定这份精度有多少写进文件。工程头、命令行和编码器共用同一个 `EncodeSpec`，所以无论从哪个入口导出，同样的选择都会写出同样的字节。

## 容器 {#containers}

| `--format` | `export.container` | 说明 |
|---|---|---|
| `wav` | `wav` | RIFF/WAVE，未压缩。 |
| `flac` | `flac` | 原生 FLAC，无损。 |
| `mp3` | `mp3` | MPEG Layer III，有损。 |

未知的容器会被拒绝，并列出本次构建支持的容器：

```
unknown export container 'ogg': this build writes wav, flac, mp3
```

`--output` 跟随容器。不给路径时，文件名为 `output.wav`、`output.flac` 或 `output.mp3`。路径没有扩展名时，会补上容器的扩展名。扩展名与容器不符的路径会被拒绝，因此文件的扩展名总是与内容一致。

## WAV 位深 {#wav-depth}

| 参数 | 文件 | 内容 |
|---|---|---|
| `--sample-format i16`（别名 `--bit-depth i16`） | 有符号 16 位 PCM | 默认值。 |
| `--sample-format i24`（别名 `--bit-depth i24`） | 有符号 24 位 PCM | 同一份混音多保留 8 位，运行开销可以忽略。 |
| `--sample-format f32`（别名 `--bit-depth f32`） | IEEE 32 位浮点 | 与内部混音缓冲逐位一致。 |

`--bit-depth` 和 `--sample-format` 是同一个选项的两个名字，挑和容器搭配起来读着顺的那个即可。

默认值是 `i16`。渲染校验基线（render gate）基于 16 位文件的字节定义（48 kHz，`--no-loudness-normalisation`，两个测试工程各 4,248,432 字节）。如果默认改成 `f32`，文件体积会翻倍，所有已存的校验哈希都要变，而听感上没有任何收益。所以更高的位深需要你主动选择。

文件还要进入下一个数字环节时（另一个 DAW、母带处理链或分析流程），选 `f32`，因为全程没有量化。交付成品时选 `i24`：听起来与 `f32` 相同，体积只有它的四分之三。需要兼容性，或播放器只能读 16 位 PCM 时，用 `i16`。

所有位深的 WAV 文件头都由 `hound` 写入。16 位导出的字节（包括截断方式）与 16 位写入器一直以来的输出完全相同，渲染校验基线检查的正是这一点。

## FLAC 位深与压缩 {#flac-depth-and-compression}

FLAC 对整数无损，所以在相同位深下，它存储的整数网格与 WAV 相同。`--sample-format`（或 `--bit-depth`）选择 16 位或 24 位。`f32` 会被拒绝，因为 FLAC 没有浮点表示。

`--flac-level 0..=8` 设置压缩级别，默认为 5。级别 0 搜索最少、文件最大；级别 8 搜索最久、文件最小。级别只影响文件大小，解码出的采样永远不变。

在 48 kHz 的 20 轨参考工程上：

| 编码 | 大小 |
|---|---|
| 16 位 WAV | 4.25 MB |
| 16 位 FLAC，级别 0 | 2.18 MB |
| 16 位 FLAC，级别 5-8 | 2.07 MB |
| 24 位 WAV | 6.37 MB |
| 24 位 FLAC，级别 5 | 4.47 MB |

大部分压缩收益出现在启用 LPC 的级别 4。更高的级别只是用编码时间换几 KB。

FLAC 从不抖动。无损容器会把抖动当作永久噪声存下来。`--dither` 与 `--format flac` 同用会被拒绝，导出对话框也会禁用抖动复选框并说明原因。浮点到整数的量化照常进行，使用与不抖动的 WAV 相同的网格（`i16` 截断，`i24` 四舍五入），所以相同位深下，FLAC 渲染和 WAV 渲染解码出的整数逐位一致。

编码器是纯 Rust 的 `flacenc` crate（Apache-2.0），不需要 C 工具链，也不调用外部进程。它写出固定块大小的流，即使最后一帧较短，STREAMINFO 块也保留标称块大小。否则像 symphonia 这样严格的读取器会把流当作可变块大小，读到第一帧就停止。

## MP3 码率、采样率与模式 {#mp3-bitrate-rate-and-mode}

Layer III 接收 16 位 PCM，采样率只能是七种之一。这些采样率分属三个 MPEG 版本，每个版本有自己的 CBR 码率表：

| MPEG 版本 | 采样率 | CBR 码率 |
|---|---|---|
| MPEG-1 | 44100、48000 Hz | 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320 kbps |
| MPEG-2 | 16000、22050、24000 Hz | 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160 kbps |
| MPEG-2.5 | 11025、12000 Hz | 同 MPEG-2 |

导出对话框通过 `export_capabilities` 从编码器读取这七种采样率，所以容器写不了的采样率会在渲染开始前被拒绝。

默认是 192 kbps CBR。`--mp3-bitrate <kbps>` 选择表中的其他码率。`--mp3-vbr <0..9>` 改用可变码率编码。两个参数互斥，因为 VBR 编码自己决定平均码率。质量 0 最好，9 体积最小。VBR 各档的平均码率以参考编码器为目标，所以 `--mp3-vbr 2` 在音乐上大约是 190-200 kbps。

没有 Layer III 形式的采样率会被拒绝，渲染器不会为了凑上而重采样：

```
96000 Hz is not an MP3 sample rate: MPEG Layer III writes 44100, 48000, 16000,
22050, 24000, 11025 or 12000 Hz and this build never resamples to reach one
```

不在该版本码率表中的码率会被拒绝，并列出码率表，绝不会自动吸附到相邻值：

```
MP3 bitrate 200 kbps is not in the MPEG-1 Layer III table; MPEG-1 writes 32-320
kbps: 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256 or 320
```

MP3 没有位深，也没有抖动环节。编码器只接收 16 位 PCM，所以命令行在 `--format mp3` 时拒绝 `--sample-format` 和 `--dither`，对话框也会隐藏这两个控件。`--report` 同样会被拒绝，因为它会把写出的文件当作 WAV 重新读取。

对话框会像记住其他容器一样记住 MP3 导出设置。[偏好设置包](../preferences-bundle.html)（英文）把它存为 `export.format: "mp3"`，以及 `export.mp3_bitrate`（CBR 码率表中的值，单位 kbps，`32`-`320`）和 `export.mp3_vbr`（质量档 `0`-`9`，CBR 时为 `null`）。在另一台机器上导入这个包，会恢复码率和模式。

### 编码器与音质 {#encoder-and-quality}

音质在两个渲染校验测试工程（各 22.13 秒，48 kHz 立体声）上与源混音对比测得，因此这些数字描述的是 OpenCompose 自身的输出：

| 编码 | 残差 RMS | 相对源的 LSD | 15 kHz 以上能量 |
|---|---|---|---|
| CBR 192，测试工程 A | -52.11 dBFS | 1.72 dB | -0.35 dB |
| CBR 192，测试工程 B | -42.50 dBFS | 1.66 dB | -0.35 dB |
| VBR q2，测试工程 A | -52.11 dBFS | 1.72 dB | -0.35 dB |
| VBR q2，测试工程 B | -42.49 dBFS | 1.67 dB | -0.35 dB |
| CBR 320，测试工程 B | -50.18 dBFS | 0.74 dB | -0.31 dB |
| CBR 96，测试工程 B | -36.33 dBFS | 3.30 dB | -7.74 dB |

码率越高，残差越低。全频段的三分之一倍频程差异都在 0.12 dB RMS 以内，所以除最高一个倍频程外，没有哪个频段被抬高或削减。15 kHz 以上，编码器损失约 0.35 dB 能量；常见的 MP3 低通在这里会损失 2.5-3 dB。在 CBR 192 和 VBR q2 下，编码器与一个外部参考编码器的对数谱距离差在 1 dB 以内，这是 MP3 作为默认功能提供的前提。

流中带有 `Info`/`Xing` 标签帧及扩展块，记录编码器延迟（528 个采样）和末尾填充。能识别该标签的播放器解码出的长度与混音完全一致：1062097 个输入帧编码为 923 个 1152 采样的音频帧，再裁回 1062097。忽略该标签的播放器会在两端各多出最多一帧的静音。

## 采样率 {#sample-rate}

`ocp render --sample-rate` 接受 8000-192000 Hz，即采样器能运行的范围。超出范围会被拒绝：

```
sample rate 0 Hz is outside the supported 8000-192000 Hz range; enter a rate the
sampler can run, such as 44100 or 48000
```

工程可以声明自己的导出采样率：

```ocp
export: {container: wav, wav: {depth: i24, dither: true}, sample_rate: 48000}
```

`ocp render` 优先使用你传入的 `--sample-rate`，其次是 `export.sample_rate`，最后是 44100。导出对话框会把渲染所用的采样率写进同一个字段，这样工程再次渲染时会使用混音时的采样率。

## 抖动 {#dither}

抖动只有开和关两种状态，并且只作用于整数输出。

- `--dither` 打开抖动。`--no-dither` 关闭抖动，即使工程里打开了它。两个参数互斥。
- 在 `i16` 上是 1 LSB 的 TPDF 抖动，在四舍五入到最近整数之前加入。这个量足以让量化误差与信号去相关。再多就成了听得见的噪声，再少则去相关不彻底。
- 在 `i24` 上是同样的 1 LSB TPDF，绝对电平低 48 dB。
- 在 `f32` 上会被拒绝。文件已经保存了内部采样，抖动只会增加噪声。报错为 `dither is meaningless for f32: 32-bit float already carries the internal mix
  precision; use i16 or i24, or turn dither off`。
- 在 FLAC 上会被拒绝，因为无损容器会把噪声存下来。
- 在 MP3 上会被拒绝，因为 Layer III 接收 16 位 PCM，没有抖动环节。

取整方式取决于位深，与抖动是两回事：

- `i16` 不抖动时截断，因此已有的导出和渲染校验基线保持逐字节一致。抖动时四舍五入到最近值。
- `i24` 无论是否抖动，都四舍五入到最近值。

### 确定性与种子 {#determinism-and-seeds}

抖动来自一个基于计数器的生成器。每个采样的抖动值只取决于帧序号、声道和种子（`WavOptions::dither_seed`，默认为 0）。同一个工程渲染两次，字节完全相同，所以带抖动的导出也能测试：重新渲染后不需要带容差比较。改变种子只会改变每个采样的最低有效位。

一组文件中的每个文件都有自己的种子，由文件名派生。混音文件沿用导出设置中的种子，每个分轨从自己的基础文件名派生种子。如果分轨共用一个种子，它们会带着相同的噪声序列，相加后的抖动功率是单个文件的 N 倍。种子各不相同时噪声互不相关，相加后约为 √N 倍。派生过程是确定的，所以相同的工程和分轨名会得到相同的字节。

## 削波 {#clipping}

编码器之前没有任何环节限制混音电平，只有整数写入器在满刻度处钳位。导出会测量量化前的混音并报告结果：

- `ocp render --report` 同时打印写出文件的电平和量化前混音的电平，所以削波的 `i16` 文件不会被误当作干净的 0.00 dBFS 母带：
  `Mix before quantisation: peak +3.20 dBFS, true peak +3.55 dBTP (clipped at full scale in the written file)`。
- 只要导出发生削波，无论是否使用 `--report`，都会打印警告：
  ``warning: <file> reaches +3.20 dBFS before quantisation and was clipped at full scale; lower the level or add a `loudness:` target``。
- 混音超过满刻度 6 dB 以上时，在打开任何文件之前就拒绝导出：
  ``the mix reaches +8.00 dBFS, more than 6 dB above full scale; export refused (lower the master fader or add a `loudness:` target)``。
  峰值达到满刻度两倍是增益结构的错误，写出来只会得到方波。

设置了 `loudness:` 目标时，主输出限制器把混音保持在 −1 dBFS 或以下，所以响度归一化的导出既不会削波，也不会警告。

## 分轨文件名 {#stem-names}

分轨文件名保留所有文字系统的字母和数字，转为小写，并使用容器的扩展名。用中文命名的工程会导出 `<工程名>-mix.wav` 和 `<工程名>-<轨道>.wav`（或 `.flac`），中文字符原样保留。`Café Déjà` 导出为 `café-déjà-mix.wav`。总线分轨命名为 `<song>-bus-<bus>`。在路径中不安全的连续字符（`/`、`\`、`:`、引号、控制字符）会合并成一个短横线。如果什么都不剩，整组文件回退为 `song`，单个分轨回退为 `track`。

## 设置保存在哪里 {#where-the-setting-lives}

工程的规范写法按容器嵌套各自的选项：

```ocp
@project "My song"
  tempo: 120
  export: {container: wav, wav: {depth: i24, dither: true}, sample_rate: 48000, resample: 6}
```

FLAC 工程加一个 `flac:` 块：

```ocp
export: {container: flac, wav: {depth: i16, dither: false}, flac: {depth: i24, level: 8}}
```

`flac:` 块默认为 `{depth: i16, level: 5}`。取默认值时序列化器会省略它，所以 WAV 工程的写法保持不变。对应的命令行是 `--format flac --sample-format i24 --flac-level 8`，命令行参数优先于工程中的块。

MP3 工程声明码率或 VBR 质量：

```ocp
export: {container: mp3, wav: {depth: i16, dither: false}, mp3: {bitrate: 256, vbr_quality: 2}}
```

`mp3:` 块默认为 `{bitrate: 192}`，不带 `vbr_quality`，取默认值时会被省略。`bitrate` 是 CBR 码率，单位 kbps。`vbr_quality` 是 0-9 的质量档，出现时取代 CBR 码率。对应的命令行是 `--format mp3 --mp3-bitrate 256` 或 `--format mp3 --mp3-vbr 2`。

`resample:` 可选，选择离线重采样档位（0-7）。`sample_rate:` 可选，记录工程的导出采样率；不写时由调用方决定。

旧的扁平写法仍然可以加载，含义相同：

```ocp
@project "Legacy"
  export: {format: i24, dither: true}
```

没有 `export:` 块的工程导出不抖动的 16 位 WAV，序列化时也不会写出这个块。`ocp fmt` 会把已有的块改写成容器形式。扁平形式仍被接受，所以旧文件可以原样加载。

设置的优先级依次为：命令行参数、工程头、不抖动的 16 位 WAV。分轨导出会在 `stems.json` 中记录实际生效的选择：`container`、`sample_format`、`dithered`、`sample_rate` 和 `channels`，并记录混音和每个分轨的帧数。

分轨互不重叠。送往总线的轨道只出现在该总线的分轨里，不会再单独导出一份。只有 `mix_sum_member` 为 true 的分轨相加等于主输出之前的混音，而且前提是它们全部导出。这些分轨包含送往主输出的最终路由延迟；其他分轨都取自轨道或总线的输出端，位于其送出路由的延迟之前。因此清单中同一份干信号绝不会出现两次，接收方可以用标记的分轨重建混音，不会把编组和其子轨道重复叠加。

## 导出对话框 {#the-export-dialog}

图形界面只有一个导出对话框。有三种方式打开它：工具栏的渲染按钮、文件菜单中的 **导出音频…**，以及 `Cmd/Ctrl+E`。分轨按钮打开的是同一个对话框，范围预设为 **混音 + 分轨**。

在对话框中，你可以选择容器、位深、抖动、码率或 VBR 质量、采样率、重采样档位、响度归一化和分轨文件夹。对话框通过与其他工程修改相同的可撤销编辑路径，把这些选择写进工程的 `export:` 块。它还会在本机记住上一次的选择，但只用来填补工程没有设置的选项。

格式选择器会向后端查询本次构建带有哪些编码器。没有 MP3 编码器的构建会把 MP3 显示为禁用，原因是 `本次构建没有 MP3 编码器`。MP3 相关控件只在选中 MP3 时出现。MP3 的大小估算按所选码率的 CBR 计算，对话框会注明这一点，因为 VBR 文件的大小取决于素材。

渲染在命令线程之外运行，并在检查点报告进度。你可以取消渲染；它会在下一个检查点停止，不发布任何文件。
