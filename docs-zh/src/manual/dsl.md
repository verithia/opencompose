# .ocp 文件 {#the-ocp-file}

OpenCompose 工程是一个扩展名为 `.ocp` 的 UTF-8 文本文件。语法定义在 `crates/oc-dsl/src/grammar.pest` 中。本章讲你要写什么；文件如何解析，请看语法文件。

## 文件结构 {#file-shape}

```ocp
@ocp "0.6.0"                 // optional version line

@project "Song title"        // required, properties follow
  tempo: 120
  time_signature: 4/4
  key: C major

// then @preset, @pattern, @track, @bus, @section, @master in any order
```

注释以 `//` 开头，一直到行尾。缩进只影响观感，文件结构由各个 `@` 关键字决定。

各个块可以按任意顺序书写。`@pattern` 或 `@section` 可以写在 `@track` 之后，`@bus` 也可以写在路由到它的轨道之前。不过每个块必须连续：工程层级的下一个 `@` 关键字会结束前一个块，所以轨道的内容必须留在它自己的 `@track` 块里。如果属于某条轨道的一行出现在另一个块之后，解析器会拒绝它，并在报错中指出这一行。

内联鼓乐段（`@pattern name [range] xN` 后接击打行）属于轨道内容，要写在 `@track` 里面。工程层级的 `@pattern` 是具名定义，用 `clip:` 放到轨道上。

名称可以是裸标识符，也可以是带引号的字符串。轨道、总线和预设的名称就是它们的路由标识。

版本行可以省略，新工程写 `0.6.0`。为 `0.2.0` 写的文件无需修改即可加载。之后每个版本引入一项语法：`0.3.0` 引入 `@vocal_tune`，`0.4.0` 引入它的自动字段 `snap`、`reference`、`cap` 和 `window`，`0.5.0` 引入音符时间偏移，`0.6.0` 引入 `@vocal_harmony`。

## 时间与位置 {#time-and-positions}

位置写作 `bar:beat:subdivision`。小节和拍从 1 开始计数，细分 `0` 表示正拍。范围写作 `[start - end]`，终点不包含在内，所以 `[1:1 - 5:1]` 覆盖第 1 到第 4 小节。

```
[1:1]        bar 1, beat 1
[9:4]        bar 9, beat 4 (not the end of bar 9)
[10:1]       the end of bar 9
[1:1:0]      explicit subdivision 0 (on the beat)
```

时值有 `w`、`h`、`q`、`8th`、`16th`、`32nd` 和 `64th`。可以加附点（`q.`、`q..`），写成连音（`qt`、`q5:4`、`q7:8`），也可以用连音线连接（`q~h`、`C4 q~h~q`）。休止符使用同样的时值：`rest q`、`rest 2w`。

乐句或乐段的范围必须覆盖其全部内容，休止符也算在内。4/4 拍下四个小节的乐句写作 `[1:1 - 5:1]`；写成 `[1:1 - 4:4]` 会少一拍。

## 工程属性 {#project-properties}

| 属性 | 示例 | 含义 |
|---|---|---|
| `tempo:` | `tempo: 120` | BPM |
| `time_signature:` | `time_signature: 4/4` | 拍号 |
| `picture_clock:` | `picture_clock: {rate: "30000/1001", origin: "01:00:00;00"}` | 可选的画面帧率，以及工程零点处的时间码标签 |
| `key:` | `key: Bb minor` | 调号（`major`/`minor` 或 `maj`/`min`） |
| `swing:` | `swing: 55%` | 工程摇摆；50 % 为平直，不写也是平直 |
| `swing_subdivision:` | `8th` 或 `16th` | 摇摆作用的网格；不写为 `8th` |
| `tempo_map:` | `tempo_map: 9:1 = 90` | 在指定位置改变速度（也可在时间线的速度轨道中编辑） |
| `time_sig_map:` | `time_sig_map: 17:1 = 3/4` | 在小节起点改变拍号（也可在时间线的速度轨道中编辑） |
| `key_map:` | `key_map: 17:1 = Db major` | 转调 |
| `mpe:` | `mpe: {lower: 2-7, bend_range: 48}` | MPE 分区布局 |
| `export:` | `export: {container: wav, wav: {depth: i24, dither: true}}` | 导出容器、各容器的选项以及导出采样率；旧的扁平写法 `export: {format: i24, dither: true}` 仍可加载（见[导出格式](../export-format.md)） |
| `solo_mode:` | `solo_mode: upstream` | 独奏的传播方式：`both`（默认）保留整个图中的相邻节点，`upstream` 只保留向被独奏通道供声的源 |
| `live_resample:` | `live_resample: 3` | 流式预览的重采样质量档位：`0` 最快，`7` 最好 |
| `automation_normalization:` | `automation_normalization: 2` | 存储的合成器自动化表版本：支持 1 和 2；不写即为版本 1，不支持的版本在渲染前就会被拒绝 |

映射类属性在同一块中每行写一个条目。`export:` 还接受 `resample: 0…7`（离线渲染使用的档位）以及一个容器专属块：`wav: {depth, dither}`、`flac: {depth, level}` 或 `mp3: {bitrate, vbr_quality}`。

## 段落 {#sections}

`@section` 为编排中的一段时间命名。工程一旦有了段落，渲染的时间线就由段落决定：段落按书写顺序播放，`repeat:` 让某段再播放一遍。内容归属于包含其起点的段落。段落之间的空隙会被去掉，不在任何段落中的内容不会被渲染。

```ocp
@section verse [9:1 - 17:1]
  repeat: 2
  label: "Verse"
  flags: pizzicato
```

`repeat:` 至少为 1。段落必须按顺序排列，且不能重叠。

`flags:` 为段落内的音符起点选择一种区域演奏法：`pizzicato`/`pizz`、`arco`、`muted`/`con_sord`、`sul_ponticello`/`sul_pont` 或 `sul_tasto`。音符自身的 `articulation=` 优先。该标志与逐音符的演奏法标记使用同一套演奏法配置，因此段落对应的键位切换、CC 或参数会在段落内每个音符开始前发送。见[段落与渲染后的编排](../arrangement.md)。

## 标记 {#markers}

`@marker` 为时间线上的一个点命名。名称可以是字符串或裸标识符，位置使用常规的 `bar:beat:subdivision` 写法。

```ocp
@marker "Verse 2" at 9:1

@marker outro at 41:1:2
```

标记只是注释，从不改变渲染结果。标记按书写顺序随工程保存，可以放在最后一段音频之后。没有标记的工程不会写出任何标记行。见[标记与循环区域](markers.md)。

## 乐段与片段 {#patterns-and-clips}

顶层的 `@pattern` 是一个具名、可复用的内容块。`clip:` 行把它放到轨道的某个时间线位置上，同一个乐段可以放置任意多次。

```ocp
@pattern keys_verse | instrument: piano/acoustic_grand, preset: keys, color: "#4fc3f7"
  events:
    C4 q mp
    E4 q mp
    G4 h mf

@track keys | instrument: piano/acoustic_grand, preset: keys
  clip: keys_verse [1:1 - 9:1]
```

`|` 之后的内联属性有：`instrument:`、`preset:`、`transpose:`（半音）、`swing:`（乐段自己的摇摆，覆盖工程设置）、`bars:`（乐段长度，以小节计）和 `color:`。

`bars:` 限定钢琴卷帘和鼓轨道放置新内容的范围。如果你画到了最后一个小节之后，新内容会落在乐段内最后一个可编辑的步上。不写 `bars:` 时，乐段在已写内容所占最后一个小节的末尾结束。缩短乐段时，你可以保留或裁掉新终点之后的音符。保留的音符仍留在文档里，钢琴卷帘会把它们标为超出范围。

### 移调 {#transposition}

`transpose:` 在乐器演奏之前，把乐段中有音高的 `events:` 按整数半音移动。设为 `-2` 时，写下的 C4 听起来是 Bb3。该字段接受 `-127` 到 `127`。超出范围的值会被拒绝，报错信息会给出允许的范围；它不会被当作 0 处理。

所有播放该乐段的路径都会应用这个移调：离线渲染、流式播放、MIDI 导出、分析和区间渲染。因此渲染出的音频和导出的 MIDI 与面板中的数值、乐段预览保持一致。

移调改变的是音符编号，所以它叠加在乐器对每个音符所做的任何处理之上。在内置的 OpenSynth 上，预设的 `tuning.transpose` 仍会再叠加一次：`-2` 的乐段用在 `+12` 的预设上，听起来比写下的音高高十个半音。乐段自己的 `preset:` 和 `instrument:` 决定由什么来演奏它（乐段未指定时使用轨道的预设），但不会取代移调。

`transpose:` 不移动 `hits:`。击打按名称指向一个采样或 `@kit` 鼓垫，移调会让它触发另一件鼓。鼓乐段有没有这个值听起来都一样，渲染时会提示该值对击打无效。

### 乐段主体与内联内容 {#pattern-bodies-and-inline-content}

乐段主体只能包含 `events:`、`hits:` 或两者。`automation:`、`mix:`、`effects:` 和 `engine:` 不是乐段字段。带有其中任何一项的乐段会解析失败并给出诊断，字段不会被悄悄丢弃。乐段是放在轨道上的内容，效果链、混音器状态、自动化和引擎选择都属于那条轨道。

你也可以把内容直接写在轨道里：

- `@phrase name [range]` 加音符列表，在轨道内联放置音符。
- `@pattern name [range] xN` 加击打列表，在轨道内联放置鼓乐段。

```ocp
@track drums | instrument: drumkit, preset: basic
  @pattern beat [1:1 - 3:1] x2
    kick  1:0 ff
    snare 2:0 f
```

### 滑移与循环 {#slip-and-loop}

片段有两个可选属性，分别决定片段播放乐段的哪一部分，以及是否重复。两者都不会移动片段本身：

```ocp
@track keys | instrument: piano/acoustic_grand, preset: keys
  clip: keys_verse [1:1 - 5:1] offset: 1q loop: 2q
```

`offset:` 隐藏乐段开头相应数量的四分音符。片段的起点、终点和长度保持不变，于是乐句相对节拍移动，而块的位置不动。被隐藏的开头是静音的：在其中开始的音符不会发声，那里的踏板标记也会被丢弃。如果片段已经显示了整个乐段，就不能再滑移。

`loop:` 从偏移处开始，把相应数量的四分音符内容在整个片段长度内重复。循环长度必须小于片段，并且必须在乐段内容之内结束。最后一次重复在片段终点停止，越过终点的音符在那里被截断。时间线会在每次重复处画一条虚线接缝。

两个属性都没用的片段不会写出这两个字段。

### 片段组 {#clip-groups}

`@group` 把多个片段绑在一起：拖动一次移动整组，单击一次选中整组。组按片段标识符列出成员，片段在自己那一行用 `id:` 获得标识符：

```ocp
@track keys | instrument: piano/acoustic_grand
  clip: keys_verse [1:1 - 9:1] id: c1
  clip: keys_verse [9:1 - 17:1] id: c2

@group g1 {
  members: c1, c2
}
```

一个组至少包含两个片段。每个成员必须指向带有该标识符的片段，标识符在整个工程中唯一。只有当某个组引用一个片段时，该片段的标识符才存在。删除或拆分片段会把它从所有组中移除；成员少于两个的组会连同它持有的标识符一起被删除。没有组的工程不含任何组行。

组只影响编辑，从不改变渲染结果。

## 音符 {#notes}

音符行由可选的 `@N` 和一个或多个以逗号分隔的事件组成：

```ocp
C4 q mp
F#5 8th f articulation=staccato v=110
Bb3 w mf expression={dynamics: mp->mf}
@grace Db5 16th f
@0 Bb3 h mf                          // absolute beat position 0
@0 C4 q mf, E4 q mf, G4 q mf         // three events at beat 0
@2 [C4 E4 G4] q mf, Bb3 q mf         // chord token as one member
```

逗号列表中的每个成员都在该行的位置发声，并保留各自的时值、力度和修饰。`@0.0 G4 q mf, G5 h mp articulation=legato` 是同时开始的一短一长两个音。不带 `@` 的行使用当前的顺序位置。每行只把位置推进一次，推进量等于其中最长成员的时值。

逗号列表必须写在同一物理行内，且不能重复同一音高。行首逗号、行尾逗号、缺少时值的成员和跨行的逗号都会被拒绝，各有对应的报错信息。

`[C4 E4 G4] q mf` 是逗号行 `C4 q mf, E4 q mf, G4 q mf` 的简写：方括号内的音高共用一组时值、力度和修饰。序列化器写出的是逗号形式，所以和弦写法保存后会变成 `@0 C4 q mf, E4 q mf, G4 q mf`。保存时，除音高外所有属性都相同的同时音符会合并成一个逗号列表；时值、力度记号、力度值或演奏法不同的音符保留在各自的行上。

| 部分 | 取值 |
|---|---|
| 音高 | `A`–`G`，可加 `#`、`##`、`b`、`bb`，后接八度 `-1` 或数字 `0`–`9`：`C-1`、`C4`、`F#5`、`Bb3` |
| 列表 | 同一行内以 `,` 分隔的音符事件：`G4 q mf, A4 q mf` |
| 和弦（简写） | `[` 以空白分隔的音高 `]`：`[C4 E4 G4]`，共用一组时值/力度/修饰 |
| 时值 | `w h q 8th 16th 32nd 64th`，附点、连音、连音线 |
| 力度记号 | `ppp pp p mp mf f ff fff`（可选） |
| `v=` | 数值力度 0–127，覆盖力度记号 |
| `@N` | 乐段内的绝对起始拍，作用于该行的每个成员 |
| `articulation=` | `legato`、`staccato`、`pizzicato`、`tremolo`、`palm_mute`、`ghost_note`、`sforzando` 等（语法中共 29 个名称） |
| `text=` | 逐音符注释，例如歌词 |
| `bend=` | 弯音，单位为音分 |
| `vibrato={rate: 5Hz, depth: 20ct, delay: 0.2s}` | 颤音 |
| `expression={dynamics: mp->mf}` | 表情曲线；任意控制名，可用 `->` 指定目标值 |
| `ornament=` | `trill`、`mordent`、`fermata`、`arpeggio`（见[装饰音](../ornaments.md)） |

## 延音踏板 {#sustain-pedal}

踏板标记是乐句或乐段中带有自身位置的事件：`@<beat> pedal down` 或 `@<beat> pedal up`。

```ocp
@track piano | instrument: piano/acoustic_grand
  @phrase theme [1:1 - 5:1]
    C4 q mf
    @1.0 pedal down
    @2.0 E4 q mf
    @3.0 pedal up
```

`pedal down` 会延住所有在踏板按下期间到达音符结束（note-off）的音，`pedal up` 释放它们。位置必须写出，因为踏板跨越多个音符，不参与顺序行模型。不带 `@` 的标记会被拒绝，报错信息会指出缺少位置。写在乐句或乐段之外的踏板标记同样会被拒绝。`hits:` 列表不接受踏板标记，因为鼓击打没有可以延住的 note-off。

## 力度与渐强渐弱 {#dynamics-and-hairpins}

音符上的力度记号设定一个电平。渐强渐弱记号（hairpin）让电平在它覆盖的音符上变化。`@<beat> cresc` 和 `@<beat> dim` 与踏板标记一样，是乐句或乐段中带有自身位置的事件。

```ocp
@track strings | instrument: strings/violin, preset: warm_strings
  @phrase swell [1:1 - 5:1]
    @0.0 cresc
    @0 C4 w pp
    @4 C4 w
    @8 C4 w
    @12 C4 w ff
```

记号会渐变到同一乐句中下一个写有力度记号的音符；如果后面没有音符带力度记号，就渐变到乐句末尾。重复写出相同力度记号的音符会结束这段渐变，所以覆盖的音符全部标为 `pp` 的 `cresc` 会被拒绝：电平根本不会变化。没有写出目标时，`cresc` 从 `pp` 到 `f`，`dim` 从 `f` 到 `pp`。

在内置合成器上，乐句中的渐强渐弱会编译成一个 `gain_automation:` 片段，只改变电平。SoundFont 或托管乐器则在记号范围内收到 CC11 表情控制点，由乐器自身的力度响应跟随渐变。SoundFont 还会在范围内的音符发声时交叉淡化其力度层。力度层的幅度经过补偿，所以电平变化只来自 CC11。内置轨道保留增益片段，因为它的合成器没有 CC11 接收端。

要在 SoundFont 轨道上恢复硬切换的力度层选择，在预设的 `params:` 或轨道的 `engine_params:` 中写 `dynamic_layers: off`。轨道上的值覆盖预设上的值。

在内置轨道上，渐变范围内不要有其他增益片段：同一段时间内一条轨道不能有两个增益片段，而且一个渐强渐弱不能在前一个结束之前开始。`ocp fmt` 会保留记号，并把内置轨道展开后的片段以注释形式打印出来。

乐段中的记号相对于乐段内容。每次放置都会在承载它的轨道上展开该记号，每次循环重复展开一次，所以播放两遍的乐段会写出两段渐变。乐段中的记号始终使用增益片段形式，即使在托管轨道上也是如此，因为乐段可以指定自己的预设。被滑移隐藏的记号会被丢弃，渐变在片段终点停止。

渐强渐弱记号在以下情况会被拒绝：写在 `hits:` 列表中、写在乐句或乐段的 `events:` 块之外，或者在所在容器中后面没有音符。

## 滑音 {#glissando}

音符上的 `gliss` 以音高曲线的方式把音高滑向乐句中的下一个音；它不会触发新的采样。

```ocp
@track lead | instrument: synth/lead_saw, preset: slide_lead
  @phrase slide [1:1 - 3:1]
    A4 w mf gliss
    E5 q mf
```

这个修饰有四种写法：

- `gliss` 滑向下一个音的音高。
- `gliss=G4` 指定目标音高。
- `gliss=120ms` 在 120 ms 后到达下一个音的音高并保持。
- `gliss=run` 或 `gliss=run_diatonic` 用一串短音代替曲线，分别按半音阶或工程调性。

跑句写法是为不能滑音的采样类音源准备的。SoundFont 或采样库预设是录音，所以对它使用普通的 `gliss` 会被拒绝，报错信息会给出可以演奏的跑句写法。接受弯音的插件也能滑音；报告不支持弯音的托管音源同样会被拒绝。

滑音跟随工程时钟。在速度映射下，曲线以四分音符计量，因此会随速度伸缩。带有曲线的轨道会把曲线音高上的每个普通音符视为独立的声部，所以同一条轨道仍可以在该音高上放普通音符，各自保持自己的音高和释放。但如果某个普通事件无法分配给声部（键位切换、控制触发或鼓击打），而它又落在曲线的音高上，引擎仍会拒绝，因为仅凭音高无法判断该事件指向哪个声部。

## 装饰音 {#ornaments}

`ornament=` 把一个写下的音符变成演奏者实际会奏出的若干音符或发声时长。记号保留在乐谱中，`ocp fmt` 会保留它。播放和 MIDI 导出把它展开为普通音符。

```ocp
@project "Ornaments"
  tempo: 120
  key: C major

@track lead | instrument: synth/lead_saw
  @phrase line [1:1 - 3:1]
    C4 q mf ornament=trill
    E4 q mf ornament=mordent
    G4 h mf ornament=fermata
```

| 取值 | 效果 |
|---|---|
| `trill` | 以十六分音符网格在写下的音高和上方相邻音级之间交替，最后落在写下的音高上。 |
| `mordent` | 先奏写下的音高，再奏上方一级，然后保持写下的音高。 |
| `fermata` | 把写下的发声时长延长到 1.5 倍。 |
| `arpeggio` | 把同一起点的各成员按音高从低到高依次延后一个三十二分音符。 |

颤音和波音的上方音级取自工程调性；没有调性时，音级为一个半音。它们不能与演奏法、弯音、颤音（vibrato）、表情曲线或滑音用在同一个音符上，因为这些都描述了第二条音高或电平路径。颤音或波音需要单独占用一个音符。音符太短、放不下网格时，要么保持完整，要么把发声时长分给波音的三个部分。`arpeggio` 会琶音化所有与被标记音符同时开始的音符，并保留各自写下的时值。

完整的时值规则和导出行为见[装饰音](../ornaments.md)。

## 鼓击打 {#drum-hits}

击打写作 `drum beat:subdivision dynamics modifiers`。鼓名是自由标识符（`kick`、`snare`、`hihat`、`ride`、`crash` 等），触发什么取决于鼓组或预设。精确力度写作 `v=1` 到 `v=127`，位置与音符的力度值相同，并覆盖写下的力度记号：`kick 1:0 mp v=120` 以 120 发声。

```ocp
hits:
  kick  1:0 ff
  hihat 1:0 mp articulation=closed
  snare 2:0 f
  hihat 2:2 mf articulation=open
```

击打位置是乐段自身那一小节内的 `beat:subdivision`，不含小节号。拍从 1 开始计数，细分以该拍的十六分音符计，取值 `0` 到 `3`。`1:0` 是强拍，`1:2` 是强拍之后的第二个十六分音符，`2:3` 是第 2 拍的最后一个十六分音符。

要把击打放在两个十六分音符之间，使用 `grid_offset_beats`。例如 `kick 1:1 mf grid_offset_beats=-0.125` 把击打放在强拍之后一个三十二分音符处。偏移带正负号，以四分音符计，任一方向最多 0.125。偏移后的位置决定片段和循环的归属，也是摇摆的输入。如果要在摇摆之后再做表情性的微调，使用 `timing_offset_beats`。

一个乐段包含一小节的击打。`clip:` 把具名 `@pattern` 播放一次。多小节的律动要写成轨道内的内联乐段，用 `xN` 重复写下的小节：轨道里的 `@pattern groove [1:1 - 9:1] x8` 会把那一小节播放八次。

击打演奏法有 `closed`、`open`、`half_open`、`cross_stick`、`ghost_note`、`flam`、`roll` 和 `rim_shot`。`rest 1:2` 让该位置静音。`|` 用来分隔书写上的小节，只为方便阅读。

## 自动化 {#automation}

轨道上每个控制对应一条自动化通道：

```ocp
automation: expression {1:1=20%, 4:1=50%, 8:1=30%, 12:1=60%}
automation: volume [ppq=480] {1:1=0dB, 3:1=-6dB}
automation: pitch_bend {1:1=0ct, 2:1=50ct}
```

值可以是百分比、dB、数字、字符串或布尔值。点写在 `bar:beat` 位置上。`[ppq=N]` 把通道切换为 tick 位置，每个四分音符 N 个 tick。

给通道加上时间范围，它就成为一个片段：

```ocp
automation: filter.cutoff [3:1 - 5:1] {4:1=200Hz, 4:4=8000Hz}
automation: filter.resonance [3:1 - 5:1] muted {3:1=20%, 5:1=60%}
```

范围写作 `[start - end]`，终点不包含在内，与其他带位置的块相同。`muted` 是不带值的标志，静音的通道什么也不写。

片段只在其范围内写值。在片段开头之前，参数保持其 `effects:` 条目中声明的值。第一个点从开头起保持，最后一个值保持到（不含的）终点，终点之后不再写值，所以参数停留在片段结束时的值上。没有范围的通道从其第一个点起驱动整条时间线。

同一所有者、同一目标的两条通道不能同时写值。重叠的片段会被拒绝，片段与覆盖整条时间线的通道同时存在也会被拒绝。在边界处相接的片段可以共存，例如 `[1:1 - 3:1]` 和 `[3:1 - 5:1]`。

通道名称选择轨道乐器能理解的控制。`volume` 和 `pan` 总是映射到 MIDI CC。其他 General MIDI 控制（`modulation`、`expression`、`sustain`、`resonance`、`brightness`、`pressure`）取决于乐器的控制配置。`pitch_bend` 以音分计。在内置合成器轨道上，校验会拒绝这套词汇之外的名称，并列出可用的名称。在其他乐器上，配置中没有的名称不产生任何事件。

内置合成器的音色补丁参数只能以 `pluginparam_<hex>` 形式作为控制通道，名称是参数名字节的十六进制：`pluginparam_66696c7465722e6375746f6666` 就是 `filter.cutoff`。在预设中声明 `control_cutoff_param: "filter.cutoff"`，即可为该参数提供可读的通道名 `cutoff`。

音色补丁参数的点按参数自身的单位读取，也就是它在 `params:` 中使用的单位：截止频率用赫兹，包络时间用秒，失谐用音分。0 到 1 之间的裸数字是归一化位置（即音色补丁编辑器写入的值），百分比也表示位置。连续参数在点之间渐变，所以从 `200Hz` 到 `8000Hz` 的通道会扫过滤波器。`filter.model` 这类离散参数在每个点上阶跃。

要自动化效果器参数，写出效果器和参数的名称。轨道、总线和主输出都接受这类通道：

```ocp
automation: compression.threshold {1:1=-24dB, 3:1=-6dB}
automation: fx2.Mix [ppq=480] {1:1=50%, 5:1=80%}
```

当效果链中某类效果器恰好只有一个时，用类型名（`compression`、`eq`、`plugin` 等）指定它；否则用从 1 开始的链位置（`fx2`）。值使用与 `effects:` 条目中该参数相同的单位。内置效果器的参数名取自设备模式；插件参数使用插件自己的名称。

每个点可以带 `curve=`（`linear`、`stairs`、`half_sine` 或 `smooth`），决定到下一个点的那一段；`smooth` 还可以加 `tension=`。默认是 `linear`，`stairs` 保持阶跃。通道可以加 `smoothing=<time>`（默认 0 ms），让值经过一个单极点低通。

插件在渲染块内按采样位置收到每个变化的值，作为参数变更。内置设备在相同位置拆分处理。效果器通道与控制通道一样接受 `[start - end]` 范围和 `muted` 标志。加上范围后通道成为片段，LFO 通道的周期从片段开头开始。见[`docs/automation.md`](../automation.md)。

轨道、总线和主输出还可以使用增益包络：

```ocp
gain_automation: linear [1:1 - 9:1] {1:1=1, 8:1=0.9}
gain_automation: step ppq=480 [10:1 - 12:3] {10:1=0, 11:2=1}
```

依次写 `linear` 或 `step`、可选的 `ppq=`、范围，然后是各个点。值是线性增益，1.0 为单位增益。一个所有者可以有多个包络，但它们的范围必须有序且不能重叠。轨道上的渐强渐弱记号就是这类片段的记谱形式；见[力度与渐强渐弱](#dynamics-and-hairpins)。

## 控制源 {#control-sources}

自动化通道由你手动绘制。控制源则从一条轨道的音频中推导出控制值，让推子跟随音乐：

```ocp
@control_source kick_env | from: track(kick), attack: 5ms, release: 120ms
```

`from:` 指定一条轨道，其插入效果之后、推子之前的音频送入检测器。`sidechain(kick)` 是同一个取信号点。`attack:` 和 `release:` 是单极点时间常数：阶跃输入经过一个时间常数达到 `1 - 1/e`。可选属性如下：

| 键 | 取值 | 默认 |
|---|---|---|
| `lfo:` | `none` 或 `sine\|triangle\|square\|saw@<rate>Hz`，例如 `sine@2Hz` | `none` |
| `lfo_depth:` | `0`… 任意数字 | 1 |
| `formula:` | 关于 `x` 的简短表达式，`none` 表示关闭 | `none` |
| `scale:` | 数字 | 1 |
| `offset:` | 数字 | 0 |
| `curve:` | `linear`、`squared`、`sqrt`、`log` | `linear` |

控制值按以下顺序计算：

1. 起音/释放跟随器生成包络。
2. 计算 `scale * envelope + offset`，并限制在 `0`…`1`。
3. 应用 `curve`。
4. 应用 `formula`。
5. 加上 `lfo * lfo_depth`，结果再次限制在 `0`…`1`。

检测器取各声道绝对值中较大的一个，所以控制值跟随的是采样电平，不测量响度。用 `scale` 把电平过高的源拉回范围内。

公式只是一个简短表达式，没有脚本功能。它读取一个变量 `x`，支持 `+ - * / ^`、括号、一元负号、数字，以及 `sqrt(x)`、`abs(x)` 和 `clamp01(x)`。结果限制在 `0`…`1`，非有限值按 0 处理，所以 `1-x` 和 `x*x` 在每个采样上都是安全的。

轨道用这个值驱动推子：

```ocp
@track bass | instrument: synth/bass
  mix: control(kick_env=-12dB)
```

`control(...)` 中的值是深度，即控制值满时的增益变化。负值压低，正值提升。这个推子动作在推子之后逐采样应用，所以增益可以和底鼓瞬态在同一个采样上变化。推子前发送在它之前取信号。

设备的控制输入也可以使用这个值。声明了控制端口的设备有第二个静默输入：控制信号影响设备的处理，但从不混入它的输出。在拥有该效果链的通道上用 `route:` 行写出这条连接：

```ocp
@track kick | instrument: drums/kick

@track bass | instrument: synth/bass
  effects:
    - compression: threshold=-24dB, ratio=4:1, attack=1ms, release=100ms
  route: control kick_env -> compression
```

目标是一个效果器地址，写法与自动化通道相同：链中该类型恰好只有一个条目时用类型名，否则用从 1 开始的链位置 `fx2`。这里的类型是 `control`。不写类型时路由为 `audio`，它改为送入设备的音频侧链输入（即 `sidechain=` 的路由写法）。

一个控制源可以供给多个通道。音频图在它所监听轨道的效果链之后、每个使用者之前计算它，所以压低从起音的那个采样开始，不会晚一个块。未知的源、链中不存在的目标，以及没有控制输入的设备，都会被拒绝并给出报错信息。

通道不能供给自己的控制输入。控制值在使用者之前计算，这样的路由会形成环路。总线和主输出的效果链也接受同样的 `route:` 行。

## 控制联动 {#control-links}

控制源只供给一个使用者。控制联动把一个控制分发到多个参数，每个参数有自己的映射和平滑：

```ocp
@control_link swell
  from: cc(1, 11)
  -> track: pad param: filter.cutoff formula: exponential smoothing: 20ms
  -> track: lead param: filter.cutoff formula: linear
```

`from:` 接受三种源：

- `cc(<channel>, <controller>)`，通道从 1 开始计数。
- `macro(<n>)`，OpenSynth 的八个宏之一。
- `lane(<track>, <param>)`，一条自动化通道，其归一化值成为控制值。

每个 `->` 行指定 `track:`、`param:` 和 `formula:`，可以再加 `smoothing:`。公式有 `linear`、`inverted`、`exponential`、`logarithmic`、`midi_unit` 和 `midi_centered`。平滑为零时直接跳到新值，所以没有 `smoothing:` 的行表现得像阶跃自动化。

控制值每个块归一化一次。之后每个目标应用自己的公式，并推进自己的单极点平滑器。`midi_unit` 和 `midi_centered` 读取原始的 0…127 值，因此 CC 动作和宏动作可以在相同范围内驱动同一个参数。

## 预设 {#presets}

预设描述一个乐器及其效果器。轨道和乐段按名称引用预设。

当乐段指定了预设时，该乐段的每个片段都用乐段的预设演奏，不使用所在轨道的预设，因此一条轨道可以承载两个使用不同乐器的乐段。没有指定预设的乐段使用轨道的预设。轨道的效果器、发送和推子只作用一次，作用于乐器输出的总和。如果乐段指定了工程中未声明的预设，渲染会按名称拒绝它，不会退回到轨道的乐器。

```ocp
@preset keys
  source: soundfont
  program: 4

@preset lead
  source: builtin
  plugin: "builtin.open_synth"
  params: {synth_version: 2, osc.a.waveform: 1, filter.cutoff: 1400Hz}

@preset wide_lead
  extends: lead
  effects:
    - chorus: rate=0.35, depth=3.5, mix=28%

@preset lv2_gain
  source: lv2
  plugin: "http://example.org/lv2/gain"
  params: {gain: 0.5}
```

在效果链中用 `lv2.` 前缀引用 LV2 效果器插件：`- plugin:"lv2.http://example.org/lv2/gain": gain=0.5`。LV2 控制输入成为预设参数，以端口符号命名。插件包通过 `LV2_PATH` 和平台的 LV2 目录查找，插件的二进制只在工程用到时才加载。宿主支持立体声音频、控制端口、MIDI atom 输入和 `state:interface`，拒绝需要 worker、options、log 或 UI 扩展的插件。

在 macOS 上，AudioUnit 预设使用 `source: au`，并给出显示名称（`Apple: DLSMusicDevice`）、四字符代码（`aumu:dls:appl`）或 `.component` 路径。

| 字段 | 含义 |
|---|---|
| `source:` | `soundfont`、`clap`、`vst3`、`vst2`、`au`、`lv2`、`builtin` 或 `sample` |
| `program:` | `soundfont` 使用的 General MIDI 音色号 |
| `plugin:` | `clap`/`vst3`/`vst2`/`au`/`lv2`/`builtin` 的插件 ID（例如 `"builtin.open_synth"`） |
| `patch:` | 插件内部的预设/音色名称 |
| `file:` | `source: sample` 使用的采样文件 |
| `root_note:` | 采样播放的根音 |
| `params: { ... }` | 插件或内置音色补丁参数 |
| `effects:` | 作用于该预设的效果链 |
| `extends:` | 继承另一个预设，再覆盖部分设置 |

插件状态写在 `params:` 中，没有对应的预设字段。写 `<format>_state_file` 或 `<format>_state_hex`，带上格式名，例如 `vst3_state_file`、`au_state_file` 或 `clap_state_hex`。通用的 `plugin_state_file` 和 `plugin_state_hex` 使用预设自身的格式。状态字节在参数之后应用，所以状态文件会覆盖它同样包含的单个参数。

预设资源相对 `.ocp` 文件所在目录解析。这条规则适用于 `file:`、`wavetable.file`、`sampler.sample_file`、`sampler.sfz_file`、`<format>_state_file`、鼓组的力度层采样，以及 `serum_file` 和它的 `serum_root`。工程打开后的第一次加载同样适用，因为加载请求自带工程目录，不需要从 GUI 读回。

绝对路径按原样使用，不需要目录。没有工程目录时，相对路径会被拒绝；磁盘上不存在的文件也会被拒绝，并给出尝试过的解析路径。OpenSynth 导入器报告缺少目录时的信息是 `Relative synth asset requires a saved project directory`。

## 鼓组 {#drum-kits}

`@kit` 声明一个每个音符对应一个鼓垫的鼓乐器。它会转换为内置预设，所以轨道上写 `preset: basic` 就够了。

```ocp
@kit basic
  max_voices: 32
  pad kick       | note: C2, gain: -2dB, decay: 0.35s, one_shot: true, choke: 1
  pad hat_closed | note: F#2, gain: -3dB, choke: 2
  layer kick     | low: 1, high: 127, sample: "samples/kick.wav"
  layer kick     | low: 1, high: 127, sample: "samples/kick_alt.wav"
```

鼓垫字段有 `note`、`gain`、`pan`、`tune`、`cents`、`decay`、`release`、`reverse`、`one_shot` 和 `choke`。力度层字段有 `low`、`high`、`sample` 和 `gain`。力度窗口重叠的各层组成轮询组。见[鼓组](../drum-kit.md)。

## 轨道 {#tracks}

```ocp
@track lead | instrument: synth/lead_square, preset: lead, channel: 1
  mix: vol=-6dB, pan=0.15, send(hall=12%), polarity=normal
  effects:
    - compression: ratio=3:1, threshold=-12dB
  automation: brightness {5:1=40%, 7:1=70%}
  clip: lead_hook [5:1 - 9:1]
```

`instrument:` 写作 `category/name`（`piano/acoustic_grand`、`drums/acoustic_kit`、`synth/lead_square`、`audio/clip` 等）。裸名称表示鼓组乐器。

`channel:`（0–15）设置由音符生成的事件所用的 MIDI 通道。`port:`（从 0 开始）选择插件声明的某个 MIDI 端口，`pair:`（在该端口内从 0 开始）选择轨道读取的立体声输出对；见[多输出路由](../../multi-output-routing.html)（英文）。

轨道头不接受 `engine:`。引擎由乐器和预设决定，所以轨道头会以解析错误拒绝这个字段，不会存下一个无用的覆盖值。`engine_params: {key: value}` 向乐器传递每条轨道的覆盖设置，SoundFont 播放器从中读取 `dynamic_layers: off`（见[力度与渐强渐弱](#dynamics-and-hairpins)）。

`mix:` 属性：

| 键 | 取值 | 默认 |
|---|---|---|
| `vol=` | dB，`-120`…`+24` | 0 dB |
| `pan=` | `-1.0`…`1.0` | 0 |
| `send(bus=level)` | `%`、数字或 dB，最高 +12 dB | 无 |
| `send(bus=level, pre)` | `pre` 或 `post` | 推子后 |
| `send(bus=level, pan=x)` | `-1.0`…`1.0` | 0 |
| `send(owner.outN->bus=level)` | 插件辅助输出，N ≥ 2 | 不适用 |
| `mute=` / `solo=` | 布尔值 | false |
| `output=` | 总线名称（独占的主路由） | master |
| `polarity=` | `invert`、`normal`、`true`、`false` | normal |
| `solo_safe=` | 布尔值 | false |
| `display_name=` | 带引号的字符串，1–64 个字符 | 轨道名称 |
| `color=` | 带引号的 CSS 颜色 | 调色板 |

`display_name` 和 `color` 只改变轨道的显示，轨道名称仍是路由标识。见[混音器路由](../mixer-routing.md)。

`pan` 是立体声声场的比例。`-0.1` 是偏左 10 %，`-1` 是最左，`0` 是居中，`1` 是最右。它既不是百分比，也不是角度。超出 `-1.0`…`1.0` 的值无法通过校验，不会被截断到范围内。

## 总线与主输出 {#buses-and-master}

```ocp
@bus hall
  mix: vol=-6dB, mute=false, solo=false
  routes: [master]
  effects:
    - reverb: decay=2.4s, mix=100%

@bus drum_verb
  routes: [hall]            // bus to bus

@master
  volume: -1dB
  loudness: -14dB
  effects:
    - limiter: ceiling=-0.8dB
```

总线接受 `mix:`（`vol`、`mute`、`solo`）、`routes:` 和 `effects:`。`routes:` 指向其他总线或 `master`，校验器会拒绝环路。

轨道的发送与其主路由并行。发送到总线不会让轨道离开主输出。

`@master` 接受 `volume:`、`loudness:`（LUFS 目标，−80 到 0）、`effects:` 和 `gain_automation:`。

## 效果器 {#effects}

效果链是一组 `- device: key=value, …` 条目。设备名称就是它的 DSL 关键字；[内置设备](devices.md)列出了所有设备及其控制。

```ocp
effects:
  - eq: low_freq=150Hz, low_gain=-2dB
  - compression: ratio=3:1, threshold=-12dB, attack=5ms, release=80ms
  - reverb: decay=1.6s, mix=18%
  - limiter: ceiling=-0.8dB
```

值可以带单位后缀 `dB`、`%`、`Hz`、`ct`、`ms` 和 `s`。比率写作 `3:1`，布尔值为 `true` 或 `false`。设备未定义的参数名会导致渲染错误，绝不会被悄悄忽略，所以请照抄[内置设备](devices.md)中的名称。已退役的写法 `reverb: type=hall` 和 `eq: high_shelf=…` 仍可通过兼容层加载，并给出警告（见 [DSL 兼容性](../../dsl-compat.html)（英文））。它们不是当前的控制。外部效果器写作 `- plugin:"org.example.fx": mix=50%`。

在任意条目上写 `bypass=true` 会把该设备移出效果链。被旁通的设备不处理任何信号、不报告延迟、不增加尾音，所以渲染结果与删除该条目后的工程逐位相同。`bypass` 是效果器条目的字段，不属于设备自身的控制，因此它适用于所有内置设备和托管插件，任何设备都不能定义同名参数。序列化器把它写在设备参数之后。`bypass=false` 是默认值，从不写出，所以没有旁通任何设备的工程序列化后不含该字段。旧的 `dynamic_eq: bypass=…` 参数按同一字段读取。

## 音频片段 {#audio-clips}

`@audio_clip` 把解码后的媒体文件放到轨道上。轨道的混音、发送和效果器对它的作用与对音符完全相同。

```ocp
@audio_clip intro [1:1 - 5:1] | track: tape
  file: "media/intro.flac"       // relative to the .ocp file
  offset: 250ms                  // trim from the start
  gain: -3dB
  gain_envelope: 0:0dB, 0.5:-6dB, 1:0dB  // relative to gain, position 0..1
  fade_in: 5ms
  fade_out: 1.5s
  crossfade: 10ms
  stretch: 1.25                  // output length = media length x 1.25
  mode: musical                  // off | percussive | musical
  tempo_follow: false            // derive the ratio from the range instead
  pitch: -2                      // semitones, fractional allowed, -24..=+24

@audio_clip take2 [1:1 - 3:1] | track: tape
  file: "media/take-vox-2.wav"
  take_group: "vox take 1720000000000"  // shared by every pass of one session
  take: 2                              // one-based pass number
  muted: true                          // keep it, but play nothing

@track tape | instrument: audio/clip
  mix: vol=-6dB
```

| 字段 | 含义 | 默认 |
|---|---|---|
| `file:` | 相对工程的媒体路径；不接受绝对路径 | 必填 |
| `offset:` | 从（伸缩后的）媒体开头裁掉的秒数 | 0 |
| `gain:` | 片段增益，单位 dB | 0 |
| `gain_envelope:` | 可选的 `position:gain` 点，按片段归一化并相对于 `gain:`；第一个位置为 0，最后一个为 1 | 无 |
| `fade_in:` / `fade_out:` | 线性淡入淡出，单位秒 | 0 |
| `crossfade:` | 片段首尾的等功率淡变，单位秒 | 0 |
| `stretch:` | 时间伸缩系数，`1.0` 时未处理的音频保持不变；允许范围 `0.25`–`4.0` | 1.0 |
| `mode:` | `off` 关闭处理，`percussive` 为 WSOLA，`musical` 为相位声码器 | musical |
| `tempo_follow:` | 忽略 `stretch:`，让媒体适配片段范围；优先于 `stretch:` | false |
| `pitch:` | 以小数半音移调，不改变时长 | 0 |
| `formant:` | 独立的频谱包络移动，以小数半音计 | 0 |
| `preserve_formants:` | 近似补偿移调带来的共振峰偏移 | false |
| `warp:` | 可重复的源到目标锚点，例如 `warp: 0.5s -> 0.65s` | 无 |
| `take_group:` | 同一次循环录音中各遍共用的标识符 | 无 |
| `take:` | `take_group` 内从 1 开始的遍次号 | 无 |
| `muted:` | 保留片段及其媒体，但不输出音频；引擎像对待不存在的片段一样跳过它 | false |

启用伸缩时，`offset` 以输出时间计量。开启 `tempo_follow` 时，速度变化会改变伸缩比。

变形（warp）时间从媒体起点算起，在偏移之前计量。锚点接在隐含的 `0s -> 0s` 之后，在两条轴上都必须递增；最后一个锚点之后使用显式的伸缩比。使用 warp 时必须关闭 `tempo_follow`。

有效的共振峰校正限制在 ±12 半音以内。移调、warp 和非零的共振峰移动都要求处理处于开启状态（`mode:` 不为 `off`）。当 `stretch × pitch` 超出伸缩范围时，`pitch:` 会被拒绝。

比片段长的媒体会被裁掉，比片段短的媒体之后补静音。一次循环录音会为每一遍写一个片段，它们使用相同的 `take_group:`。最新的一遍不静音，其他各遍静音，直到拼接（comp）选择了另一遍。会话独奏不是 DSL 属性，只影响实时试听。见[音频片段](../../audio-clip.html)（英文）、[拼接](../../comping.html)（英文）和[时间伸缩](../../audio-clip-stretch.html)（英文）。

## 人声修音 {#vocal-tuning}

`@vocal_tune` 为一个音频片段保存你逐音符做出的修音决定。工程中只保存这些决定；检测到的音符和基频（f0）来自派生的 `.ocn` 分析文件。

```ocp
@vocal_tune lead_tune | clip: lead
  scale: A major
  amount: 60%
  vibrato: flatten
  notes:
    - at=0.480s tgt=A3 amount=100% vibrato=preserve
    - at=2.013s tgt=+2st
    - at=5.470s tgt=5@4 bypass=true
```

该块还为没有保存目标的音符设定自动策略：

| 键 | 默认 | 含义 |
|---|---|---|
| `snap:` | `auto` | `auto`（设置了参考时用参考，否则用音阶）、`scale`、`reference` 或 `off`。 |
| `reference:` | 无 | 在 `snap: reference` 下，其检测音高作为目标的片段。 |
| `cap:` | `100ct` | 音阶吸附的最大校正量；更大的移动不会被提议。 |
| `window:` | `35ct` | 围绕音符音高的搜索窗口。 |
| `max_move:` | 无 | 单个音符校正量的上限。 |
| `vibrato:` | `preserve` | `preserve` 保留演唱者的颤音，`flatten` 把音符固定在测得的中位音高，`scale` 保留测得深度的一半。单个音符可以用 `vibrato=preserve\|flatten\|scale` 覆盖。 |

`at` 是做出该决定时所依据的检测起点。`tgt` 接受音名（`A3`）、绝对音分（`5700ct`）、半音或音分偏移（`+2st`、`-25ct`），或音阶级数加八度（`5@4`）。`ocp vocal apply` 解析这些决定并烘焙片段。所有 amount 都为 `0%` 时，烘焙结果逐采样一致。见[人声修音](../../vocal-tune.html)（英文）。

## 人声和声 {#vocal-harmony}

`@vocal_harmony` 从修过音的音频片段派生出第二个声部。这个声部是片段按自然音程移调后的副本，加到片段所在的轨道上。主唱保持不变。

```ocp
@vocal_harmony third | clip: lead
  interval: 2         // diatonic steps above the lead note
  level: -6dB         // voice level relative to the lead
  pan: -0.3           // -1 left to 1 right
```

| 键 | 默认 | 含义 |
|---|---|---|
| `clip:` / `track:` | 无 | 源片段，或恰好只有一个片段的轨道。两者必须且只能写一个。 |
| `interval:` | `2` | 在片段音阶内高于主唱音符的音级数；负数表示声部在下方。没有音阶时按半音计数。 |
| `level:` | `-6dB` | 声部电平，单位 dB，相对于主唱在相同音符上的 RMS。 |
| `pan:` | `0` | 声部的立体声位置，`-1` 到 `1`。 |

`interval` 按音阶级数计数。C 上方三度是 E，D 上方三度是 F。音阶依次取自片段自己的 `@vocal_tune` 块、该音符处的调号映射、工程调号。整个工程都没有音阶时，按半音计数，渲染会给出警告。会让某个音符移动超过 ±12 半音的音程会被拒绝，报错信息会指出该音符。见[人声修音](../../vocal-tune.html)（英文）。

## 原始通道事件 {#raw-channel-events}

轨道可以用原始的、MIDI 形式的演奏数据代替音符。位置是绝对的四分音符数，通道从 0 开始计数，每个事件以 `;` 结尾：

```ocp
@track out | preset: meridian_keys, instrument: keys/kontakt
  performance: {
    @0 ch2 program 65;
    @0.125 ch2 note_on 60 90;
    @0.25 ch2 bend -8192;
    @1.25 ch2 note_off 60 37;
  }
```

命令有 `note_on <note> <velocity>`、`note_off <note> <velocity>`、`cc <controller> <value>`、`program <program>`、`bend <value>`、`pressure <value>` 和 `poly_pressure <note> <pressure>`。事件必须按位置非递减排列。带有这个块的轨道不能同时包含乐句、乐段或片段，校验器会拒绝这种组合。

## 完整示例 {#a-complete-example}

```ocp
@ocp "0.4.0"

@project "Manual Example"
  tempo: 120
  time_signature: 4/4
  key: C major
  swing: 55%

@preset keys
  source: soundfont
  program: 4
  effects:
    - eq: low_freq=150Hz, low_gain=-2dB
    - reverb: decay=1.6s, mix=18%

@preset lead
  source: builtin
  plugin: "builtin.open_synth"
  params: {synth_version: 2, osc.a.waveform: 1, osc.a.level: 0.7, filter.cutoff: 1400Hz, filter.resonance: 0.35, amp.attack: 0.005s, amp.decay: 0.3s, amp.sustain: 0.55, amp.release: 0.35s, output.gain: 0.3, control_cutoff_param: "filter.cutoff"}

@preset tight_drums
  source: soundfont
  program: 0
  effects:
    - compression: ratio=3:1, threshold=-8dB, attack=5ms, release=60ms

@pattern keys_verse | instrument: piano/acoustic_grand, preset: keys, color: "#4fc3f7"
  events:
    C4 q mp
    E4 q mp
    G4 h mf expression={dynamics: mp->mf}
    |
    F4 q mf
    A4 q mf
    C5 h f

@pattern lead_hook | instrument: synth/lead_square, preset: lead
  events:
    @0 G4 8th f articulation=accent
    @1 C5 8th f
    @2 Eb5 q ff vibrato={rate: 5Hz, depth: 20ct, delay: 0.2s}
    rest q
    @3 G5 h. ff articulation=legato

@pattern groove | instrument: drums/acoustic_kit, preset: tight_drums, swing: 60%
  hits:
    kick  1:0 ff
    hihat 1:0 mp articulation=closed
    snare 2:0 f
    hihat 2:2 mf articulation=open

@track keys | instrument: piano/acoustic_grand, preset: keys
  mix: vol=-3dB, pan=-0.1, send(hall=20%)
  clip: keys_verse [1:1 - 9:1]

@track lead | instrument: synth/lead_square, preset: lead
  mix: vol=-6dB, pan=0.15, send(hall=12%)
  clip: lead_hook [5:1 - 9:1]
  automation: cutoff {5:1=40%, 7:1=70%, 8:1=50%}

@track drums | instrument: drums/acoustic_kit, preset: tight_drums
  mix: vol=-2dB
  clip: groove [1:1 - 9:1]

@bus hall
  mix: vol=-6dB
  routes: [master]
  effects:
    - reverb: decay=2.4s, mix=100%

@section verse [1:1 - 9:1]
  repeat: 2
  label: "Verse"

@master
  volume: -1dB
  loudness: -14dB
  gain_automation: linear [1:1 - 9:1] {1:1=1, 8:1=0.9}
  effects:
    - eq: low_freq=40Hz, low_gain=-1dB
    - limiter: ceiling=-0.8dB
```

用 `ocp validate` 检查这个文件，用 `ocp render` 试听；两个命令的用法见[快速上手](getting-started.md)。其中两个预设是 SoundFont 音色，所以渲染时加上 `--soundfont soundfonts/MS_Basic.sf2`。
