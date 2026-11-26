# 命令行

`ocp` 是 OpenCompose 的命令行工具，用来解析、检查、格式化、导出、渲染和转换 `.ocp` 工程，也可以查看引擎所用的插件注册表和设备注册表。

```sh
cargo build --release -p oc-cli
./target/release/ocp --help
```

每个子命令失败时都以非零状态退出，并在 stderr 上输出原因。有两个全局参数可用于任何子命令：`--debug` 写入一份带调试细节的诊断日志，`--perf` 在这份日志里加入渲染和各阶段的耗时。`ocp` 会在 stderr 上打印日志路径。

## 子命令

| 命令 | 用途 |
|---|---|
| `parse` | 解析一个文件，输出摘要或 AST。 |
| `validate` | 解析并做语义校验，同时解码音频片段的媒体文件。 |
| `fmt` | 输出或写回规范格式。 |
| `export` | 把工程导出为 JSON 或标准 MIDI 文件。 |
| `render` | 离线混音并输出音频文件，可同时输出分轨。 |
| `bounce` | 在一段拍位范围内渲染单条轨道经过效果器后的信号。 |
| `clip align` | 把多个录音片段对齐到参考片段，并写入偏移和伸缩。 |
| `vocal analyze`、`vocal apply` | 分析一个音频片段的音高，或烘焙它已存的音符决策。 |
| `level` | 把轨道上 leveler 的增益决策烘焙进增益自动化。 |
| `randomize` | 为工程中的 OpenSynth 音色补丁写出带种子的随机变体。 |
| `play` | 通过音频设备播放工程，可接入实时 MIDI。 |
| `midi` | MIDI 导入、导出、端口列表和监视。 |
| `midi2ocp` | 把一个 SMF 转换为 `.ocp`（数据管线用的转换器）。 |
| `batch-midi2ocp` | 把一个目录里的 SMF 批量转换为 `.ocp`。 |
| `batch-data` | 把一个目录里的 SMF 转换为（`.ocp`，WAV）训练数据对。 |
| `generate-midi` | 生成合成的 MIDI 文件。 |
| `devices list` | 列出内置设备及其参数结构。 |
| `validate-builtin-effects` | 构建混音器信号图，但不渲染音频。 |
| `scan` | 扫描已安装的插件包；`--rescan` 会重试被隔离的插件包，`--bundle PATH` 重试单个插件包。 |
| `resolve-plugin` | 用渲染器的注册表解析插件 id、名称或路径。 |
| `plugin-params` | 列出 VST3 或 VST2 插件的全部参数。 |
| `plugin-preset import` | 把 `.ffp` 预设导入一个 VST3 插件实例。 |
| `plugin install` | 解析、下载、校验并安装一个目录条目。 |
| `plugin migrate-vst2` | 把不透明的 VST2 FXB 音色库转换为 VST3 预设。 |
| `plugin conformance` | 对每个已安装的插件包运行 VST3 一致性测试。 |
| `compile-project` | 把完整的工程 AST JSON 编译为规范的 OCP。 |
| `plugin-index` | 构建、校验远程插件索引，或为其生成签名密钥。 |
| `sample` | 搜索采样目录并下载一个采样。 |

## 查看与检查

```sh
ocp parse examples/opensynth_showcase.ocp
ocp parse --json examples/demo_ballad.ocp > /tmp/ballad.json
ocp validate examples/demo_full_featured.ocp
ocp fmt examples/demo_ballad.ocp
ocp fmt -w song.ocp
```

`parse` 逐条轨道输出摘要。加上 `--json` 时输出 JSON 格式的 AST。

`validate` 在解析之外还会检查名称和引用、数值范围、总线环路，并解码音频片段；渲染器无法使用的效果器参数值也会被拒绝。通过时输出 `Valid: "<name>" — N tracks`。

`fmt` 把规范格式写到 stdout。加上 `-w` 时直接改写文件。

## 导出

```sh
ocp export examples/demo_ballad.ocp --format json -o /tmp/ballad.json
ocp export examples/demo_ballad.ocp --format midi -o /tmp/ballad.mid
```

`--format` 接受 `json` 和 `midi`。

- JSON 是工程可直接渲染的状态，所以渐强/渐弱记号会以它展开后的增益片段出现。需要源 AST 时请用 `parse --json`。不加 `-o` 时 JSON 输出到 stdout。
- MIDI 必须指定 `--output`。它与 `ocp midi export` 使用同一个写入器：格式 1，480 PPQ，整个工程。

没有 MusicXML 选项。命令在解析参数时就会拒绝 `--format musicxml`，不会先开始任何工作。

`compile-project` 方向相反。它读取一份完整、规范的工程 JSON（文件路径，或用 `-` 表示 stdin），校验后输出 OCP。如果把这份 OCP 再解析一遍得到的工程状态与输入不完全一致，命令就会失败。

```sh
ocp parse --json song.ocp > song.json
ocp compile-project song.json > song2.ocp
```

## 渲染

`render`（包括 `--stems`）和 `bounce` 接受 DSL 文本，也接受 OCPC 工程快照，二者靠文件头区分。输入必须是普通文件，编码后不超过 128 MiB，解码后不超过 256 MiB。格式错误或超出大小的快照会在渲染开始前被拒绝。相对媒体路径仍然相对于输入文件所在的目录。这里能读取快照，并不改变 GUI 的文本保存格式，也不改变其他子命令的输入格式。

```sh
ocp render examples/opensynth_showcase.ocp -o /tmp/showcase.wav --sample-rate 48000
ocp render song.ocp -o /tmp/song.wav --soundfont soundfonts/MS_Basic.sf2
ocp render song.ocp -o /tmp/mix.wav --stems /tmp/stems
ocp render song.ocp -o /tmp/song.wav --reruns 3 --require-identical
```

| 选项 | 含义 |
|---|---|
| `-o, --output PATH` | 输出文件。默认为 `output.` 加容器的扩展名，例如 `output.wav`。 |
| `--format CONTAINER` | 要写入的容器格式；默认取工程的导出设置，没有则为 WAV。 |
| `--sample-rate HZ` | 采样率，8000 到 192000。默认取工程的导出采样率，没有则为 44100。 |
| `--soundfont PATH` | 为没有工程自带乐器的轨道提供后备 SoundFont；也可从 `OCP_SOUNDFONT` 读取。 |
| `--per-track-mix` | 让每条轨道各自经过完整的混音信号图渲染。 |
| `--output-gain-db DB` | 逐轨渲染时使用的固定输出余量，不能为正。 |
| `--minimum-duration-seconds S` | 导出的最短长度（0 到 600 秒）；不会截掉任何事件。 |
| `--reruns N` | 在 N 个相互隔离的插件实例中渲染并比较输出。 |
| `--require-identical` | 只要有一次重复渲染的字节不同就失败。 |
| `--pre-roll S` | 每个插件实例按真实时间静默预热的时长（0 到 600 秒，默认 4）。 |
| `--stems DIR` | 额外为每条轨道和每条总线写出一个母带前文件，并写出 `stems.json`。 |
| `--stem-bus NAME` | 配合 `--stems`，只保留指定总线的分轨；可重复以指定多条总线。整个音频图仍然运行。 |
| `--no-track-stems` | 配合 `--stems`，不写出轨道分轨。 |
| `--stem-pre-master` | 配合 `--stems`，另外写出主输出效果器和推子之前的信号。 |
| `--no-stem-analysis` | 配合 `--stems`，跳过 `stems.json` 中的峰值、RMS 和 LUFS 分析，这些字段为 `null`。音频和工程的响度标准化不受影响。 |
| `--no-loudness-normalisation` | 忽略 master 上的 `loudness:`。 |
| `--sample-format`、`--bit-depth` | WAV 或 FLAC 的采样位深。 |
| `--flac-level`、`--mp3-bitrate`、`--mp3-vbr` | FLAC 压缩级别和 MP3 码率设置。 |
| `--dither`、`--no-dither` | 打开 WAV 抖动；或在工程启用抖动时仍将其关闭。 |
| `--resample-quality STEP` | 重采样质量档位，0（最快）到 7（最好）。 |
| `--report` | 输出渲染文件的峰值、真峰值、RMS、峰值因数和综合响度。 |

`--stems` 只做一次导出，不能与 `--reruns`、`--require-identical`、`--per-track-mix` 或 `--output-gain-db` 同时使用。

`--report` 在文件写完之后测量。配合 `--stems` 时报告的是混音；配合 `--reruns` 时报告的是保留下来的那一次。

只写出几条诊断用分轨、不要轨道分轨和测量：

```sh
ocp render song.ocp -o mix.wav --stems taps \
  --stem-bus drums --stem-bus music --no-track-stems \
  --stem-pre-master --no-stem-analysis
```

指定的总线必须在任何内置设备加载之前就已存在。完整的路由、侧链和主输出处理仍会运行。所选分轨相加未必等于完整混音，而 `pre_master` 已经包含全部内容，所以绝不要把它与其他分轨相加。

分轨导出会先在临时存储中编码并检查每个文件（包括混音），再替换已发布的文件。如果某个分轨被拒绝，原有文件保持不变。如果替换某个文件失败，已做的替换会被撤销；文件夹中的其他文件不受影响。

容器、位深、抖动以及 MP3 采样率和码率表见[导出格式](../export-format.md)。命令行参数会覆盖工程的导出设置。

异步加载内容的插件，如果音符在内容就绪前开始，可能渲染出静音。`--pre-roll` 按真实时间等待。AudioUnit 乐器和在 Rosetta 下运行的 Intel VST2 乐器会先收到一个不带 MIDI 的音频块，其输出被丢弃。拥有插件的进程会响应它的加载回调，乐器在乐谱开始之前被重置。其他格式只是等待，不渲染静音，所以自由运行的振荡器保持相位。`--pre-roll 0` 关闭预热。`--reruns N --require-identical` 能证明输出可复现，但不能证明加载已经完成。参见[故障排查](troubleshooting.md)。

## 单轨渲染

```sh
ocp bounce song.ocp --track lead --from 9:1 --to 17:1 --output /tmp/lead.wav
```

`bounce` 在一段拍位范围内渲染单条轨道经过插入效果器、推子之前的信号，写出 32 位浮点立体声 WAV。这个范围取自完整混音所渲染的同一条时间线，所以效果器状态和延迟处理都与混音一致。发送、总线和 master 效果链不包括在内。`--from` 和 `--to` 使用 `bar:beat[:subdivision]` 写法，`--to` 是开区间，文件恰好包含这段长度。

| 选项 | 含义 |
|---|---|
| `--track NAME` | 要渲染的轨道（必填）。 |
| `--from POS` | 范围的起始拍位，写作 `bar:beat[:subdivision]`。 |
| `--to POS` | 范围的结束位置（不含），写法相同。 |
| `-o, --output PATH` | 输出 WAV，32 位浮点（必填）。 |
| `--sample-rate HZ` | 渲染采样率。默认取工程的导出采样率，没有则为 44100。 |
| `--soundfont PATH` | 为没有工程自带乐器的轨道提供后备 SoundFont；也可从 `OCP_SOUNDFONT` 读取。 |

文件按渲染结果原样写出：没有抖动、没有响度标准化、也不削波，所以电平偏高的轨道会保留它的余量。GUI 的单轨渲染走的是同一条路径，因此不打开应用也能检查渲染结果。具体语义和效果器烘焙规则见[音频片段第 11 节](../../audio-clip.html#11-bounce-and-consolidate)（英文）。

## 拼接录音

```sh
ocp clip align song.ocp lead-take take-a take-b
ocp clip align song.ocp lead-take take-a --output aligned.ocp
ocp clip align song.ocp lead-take take-a --dry-run
ocp clip align song.ocp lead-take take-a --invert
```

`clip align` 把指定的 `@audio_clip` 录音与参考片段对比分析，把开头裁剪量写入 `offset:`，把速度校正写入 `stretch:`，并报告对齐后的相关系数。加上 `--invert` 时，相关系数低于 `-0.25` 就写入 `invert: true`，否则清除该标记。

`--sample-rate` 设置分析用的采样率（默认 48000）。写入的秒数和比例与这个采样率无关。所有录音都分析完之后才会写入，所以任何一条失败都不会改动工程。默认改写输入文件；`--output` 写到别处，`--dry-run` 只报告。

校正是仿射的：一个偏移加一个固定速率，不会对录音做非线性变形。超出 `0.25..=4.0` 的比例会被拒绝，命令不会把它截断到范围内。超过 ±10 % 的速度校正会被接受，但会给出警告。设计、实测数据和现有限制见 [拼接录音](../../comping.html)（英文）。

## 烘焙 leveler

```sh
ocp level song.ocp --track lead --write
ocp level song.ocp --track lead --output baked.ocp
ocp level song.ocp --track lead --json
```

`level` 先干渲染一条轨道，再用设备自身的检测规律让该轨道的 leveler 在渲染出的信号上逐步运行。结果写入轨道的 `gain_automation` 自动化，每个 10 ms 控制边界放一个点；线性增益插值在 0.05 dB 以内就能还原的点会被去掉。随后 leveler 从轨道上移除，工程里保存的是增益决策，不再实时调节电平。

| 选项 | 含义 |
|---|---|
| `--track NAME` | 要烘焙的轨道（必填）。 |
| `-w, --write` | 替换输入工程。 |
| `-o, --output FILE` | 把烘焙后的工程写到 `FILE`。 |
| `--json` | 以 JSON 输出烘焙报告。 |
| `--sample-rate HZ` | 干渲染的采样率（默认 48000）。 |
| `--soundfont PATH` | 为没有工程自带乐器的轨道提供后备 SoundFont。 |

不加 `--write` 或 `--output` 时，命令只报告，不写任何文件。

干渲染会运行去掉 leveler 后的轨道效果链，因此 leveler 之前的效果器都包括在内。leveler 必须是轨道上的最后一个效果器，并且轨道上必须有片段。烘焙会替换该轨道上已有的增益片段。自动化只对应一组设置，所以改了 leveler 的设置就要重新烘焙。设备本身不变。检测规律和实测吻合度见 [人声压缩设备](../../vocal-comp.html)（英文）。

## 播放与 MIDI

```sh
ocp midi list
ocp midi monitor --input "IAC Driver Bus 1"
ocp play song.ocp
ocp play song.ocp --midi-in "IAC Driver Bus 1" --midi-out "IAC Driver Bus 1" --midi-clock --midi-thru
```

`midi list` 列出输入和输出端口及其编号。需要指定端口的地方，填名称或编号都可以。`midi monitor` 输出带时间戳的事件。

`play` 先离线渲染工程，再通过桌面应用设置中保存的音频设备播放结果。实时播放支持内置、OpenSynth 和 SoundFont 轨道；乐器为托管插件的轨道不参与。参见[实时 MIDI](../midi-live.md)。

```sh
ocp midi import song.mid --out song.ocp --json-summary
ocp midi import song.mid --out song.ocp --split-channels --keep-empty
ocp midi export song.ocp --out song.mid --format 1 --ppq 480 --tracks 0-2 --json-report
ocp midi2ocp song.mid -o song.ocp
ocp batch-midi2ocp midi/ -o ocp_out/ --flatten
ocp batch-data midi/ -o training_data/ --soundfont soundfonts/MS_Basic.sf2
ocp generate-midi -o data/midi_raw -c 100 --seed 42
```

`midi import` 是完整的导入器，带映射选项、速度/拍号/调号映射、CC 自动化和摘要。`midi2ocp` 是数据管线使用的较早的批量转换器；不加 `-o` 时输出到 stdout。`batch-data` 需要一个 SoundFont，每写出一个 `.ocp` 就渲染一个对应的 WAV。

## 设备与插件

```sh
ocp devices list
ocp devices list --json
ocp validate-builtin-effects song.ocp --sample-rate 48000
ocp resolve-plugin "org.surge-synth-team.surge-xt"
ocp plugin-params "/Library/Audio/Plug-Ins/VST3/Example.vst3"
ocp plugin-params "/Library/Audio/Plug-Ins/VST/Example.vst"
ocp plugin-params "org.example.eq" --json
ocp plugin-preset import "org.example.eq" preset.ffp
ocp plugin migrate-vst2 old.fxb --source-name Example --plugin Example.vst3 -o new.vstpreset
ocp plugin conformance
ocp plugin conformance --plugin "/Library/Audio/Plug-Ins/VST3/Example.vst3" --format json
```

`devices list` 从引擎和 GUI 共用的参数结构中输出每个设备的 id、类别、参数数量和显示名称。`--json` 的输出是固定的接口约定。

`validate-builtin-effects` 构建音频信号图作为 DSP 预检，但不渲染。它会跳过外部插件效果器，所以工程里可以混用内置设备和插件。旧的效果器简写会被展开并以警告报告，参见 [DSL 兼容性](../../dsl-compat.html)（英文）。

`resolve-plugin` 要求在 VST3、VST2、CLAP、LV2 和内置设备中恰好匹配一个，并以 JSON 输出它的元数据。在 macOS 上，AudioUnit 组件 id（例如 `aufx:lpas:appl`）、插件包路径或 `au:Name` 会在系统的 AudioUnit 注册表中查找。按 id 或名称找到的匹配，路径为空。

`plugin-params` 加载一个 VST3 或 VST2 插件并列出全部参数。VST3 插件的表格显示 id、名称、单位、范围、默认值、步数和标志位；VST2 插件显示编号、名称、标签、默认值和当前值，VST2 的值归一化到 0..1。如果同时装有同名的 VST3，传入 `.vst` 路径即可选中 VST2 版本。

`plugin-preset import` 读取 `.ffp` 预设，把其中的参数值应用到指定的 VST3 插件上。它报告解析、映射和应用的参数数量，并列出未映射、被拒绝以及回读不一致的参数。`--json` 以 JSON 输出报告。

`plugin migrate-vst2` 把 VST2 FXB 音色库转换为 VST3 预设。音色库必须是不透明的，且当前程序为第一个。给出原 VST2 插件的名称（据此推导 VST3 类 id）和已安装的 VST3 插件包。命令选择匹配的类；如果插件包恰好声明了一个替代类，则选择它。新的实例加载转换后的状态后，命令写出一个新的 `.vstpreset`；在 `vst3_state_file` 中指定这个文件。插件包没有匹配的类、状态校验不通过或输出文件已存在时，命令失败。转换在单独的进程中运行，时限为十秒。

`plugin conformance` 为每个 VST3 插件包启动一个子进程，以表格、JSON 或 Markdown 输出逐步骤的结果。测试项和 JSON 结构见 [VST3 一致性测试](../../vst3-conformance.html)（英文）。

## 插件索引

```sh
ocp plugin-index keygen --out /tmp/index-keys
ocp plugin-index build --catalog catalog.json --out dist/ --key /tmp/index-keys/plugin-index.key
ocp plugin-index verify dist/ --public-key /tmp/index-keys/plugin-index.pub
```

`keygen` 生成一对 Ed25519 密钥。`build` 写出 `index.json`、它的签名和压缩后的目录。`verify` 校验签名和目录哈希。

## 采样

```sh
ocp sample search "snare drum" --kind drum --limit 5
ocp sample search "vinyl loop" --min-seconds 1 --max-seconds 4 --channels 2 --json
ocp sample pull "commons:File:Snare drum unmuffled.ogg" --dest media/samples
FREESOUND_API_KEY=... ocp sample search "riser" --provider freesound
```

`search` 列出每个结果的类型、时长、声道数、采样率、许可和署名行。结果的许可被拒绝时，会说明原因。

`pull` 解析 id 并下载文件，然后校验声明的大小和 sha256。它写出音频文件、一个记录许可和署名行的 `.json` 附带文件，以及一份回执。

Wikimedia Commons 是默认来源，不需要密钥。Freesound 需要 API 密钥，提供 128 kbps 的试听版本。GUI 的**采样**面板可以通过 OAuth2 连接 Freesound 并下载原始文件。`ocp sample providers` 列出当前构建支持的来源。存储布局、许可规则和工具名称见[采样](../samples.md)。

`ocp sample slices loop.wav --first-key 60` 打印一个 SFZ 音色补丁，每个 WAVE 提示点对应一个区域；`--gated` 让切片跟随音符长度，`--json` 则改为打印帧范围。参见[采样器](../sampler.md)。

## 区间渲染与分析

窗口渲染、音频分析和 A/B 对比工具（`render_range`、`audio_analyze`、`audio_compare`）不是命令行子命令，而是运行中 GUI 的智能体接口的一部分；参见[让智能体作曲](agents.md)。

没有 `ocp serve` 命令。智能体 HTTP 接口由运行中的 GUI 提供，命令行没有服务器可启动。
