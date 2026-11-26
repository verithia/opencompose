# 故障排查

下面是用户最常报告的故障。每一节先描述你看到的现象，再给出解决办法。

## 窗口空白

开发构建从 Vite 加载界面。请在 `crates/oc-gui/frontend` 中用 `npm run tauri dev` 启动应用，这样 Tauri 会启动 Vite，并在终端里打印启动错误。独立的发布构建内嵌前端。用 Cargo 手动做发布构建之前，先在同一目录运行 `npm run build`。

## 插件消失了，工程提示 "plugin not found"

扫描器把结果缓存在 `~/.opencompose/plugin-scan-receipts.json`。这个缓存可能在两方面出问题。

1. 扫描记录过期或被覆盖。同时运行的多个会话会互相覆盖这个文件。如果记录文件是在无法运行扫描辅助进程的主机上写的，里面可能每个插件都是 `failed`。删除这个文件：

   ```sh
   rm ~/.opencompose/plugin-scan-receipts.json
   ```

   然后在侧边栏的 **插件** 标签页中点击 **重新扫描**。如果要并行运行多个会话，用 `OCP_PLUGIN_SCAN_RECEIPTS=/tmp/receipts-<task>.json` 给每个会话指定单独的文件。

2. 超时导致插件包被隔离。扫描超时会记录 `timed_out`，插件随之被隐藏 24 小时。卡住的插件不会再次启动，因为它可能在等待一个对话框。`crashed` 和 `failed` 记录会一直隐藏插件，直到你重新扫描它或它的文件发生变化。要手动重新扫描被隔离的插件：

   ```sh
   OCP_PLUGIN_RESCAN_QUARANTINED=1 ocp resolve-plugin "<plugin id or name>"
   ```

   如果插件包仍然扫描失败，请重新安装它，或者确认宿主程序和插件是为同一架构构建的。

## 插件能加载，渲染出来却没有声音

有些插件在后台线程加载采样内容，加载完成时不发出任何信号。在加载完成前开始的音符，整个时值内都不会发声。

- 给插件留出实际时间的预热：`ocp render song.ocp -o out.wav --pre-roll 8`。
- `--reruns N --require-identical` 证明渲染可以复现，但不证明结果正确：两次渲染可能都开始得太早，结果却逐字节相同。请确认预期的采样层确实能听到。
- AudioUnit 乐器和在 Rosetta 下运行的 Intel VST2 乐器会收到一个不带 MIDI 的音频块，其输出被丢弃，这样它们会开始加载保存的状态。宿主在预热期间响应它们的加载回调，并在乐谱开始之前重置乐器。
- 对其他格式，`--pre-roll` 只是等待指定的时间，并不渲染静音，所以不会推进自由运行振荡器的相位。

## 渲染因效果器参数失败

```text
invalid typed effect parameter: high_gain=+2dB@8kHz (high_gain expects a gain in dB, such as high_gain=0dB)
unsupported effect parameter: high_shelf
```

经过检查的 DSP 路径会拒绝设备无法识别类型的值。错误信息会给出它读到的值，以及设备接受的一种写法。`ocp validate` 和 `ocp render` 执行同样的检查，因此能通过校验的工程也能建立它的效果链。用 `ocp devices list --json` 或 [内置设备](devices.md) 查找设备，使用其中列出的参数名。

数值需要带单位（`%`、`dB`、`Hz`、`ms`）。`mix=18%` 是 18 %，而 `mix=0.18` 是 0.18 %。像 `+2dB@8kHz` 这样的复合值来自已停用的简写（`high_shelf=…`），不对应任何现有控件。

旧文件可能使用已停用的写法，例如 `reverb: type=plate, size=large`，或 `eq: high_shelf=-2dB@10kHz`、`low_cut=30Hz`、`notch=…` 和 `presence=…`。渲染路径仍通过兼容层接受这些写法，并为每次展开打印一条警告（见 [DSL compatibility](../../dsl-compat.html)（英文））。它们不是设备参数，有空时请参照 `ocp devices list --json` 改写。兼容层无法表示的值会被报告并跳过，绝不会悄悄生效。要找出出错的效果器，用 `ocp parse --json` 解析文件。

## 音频片段：找不到文件、长度不对或超出预算

- `audio clip 'intro': file not found: /project/media/intro.flac` 表示 `file:` 路径没有解析到文件。该路径相对于 `.ocp` 文件，DSL 不接受绝对路径。`ocp validate` 会解码每个引用的片段，所以请在渲染前先运行它。
- 片段比媒体短时，播放媒体从开头到片段结尾的部分。片段比媒体长时，媒体结束后是静音。`stretch:` 超出 `0.25`–`4.0` 会被拒绝。
- 使用 `tempo_follow: true` 的片段不能分割，因为两半会从同一媒体推算出不同的比例。请先设置明确的 `stretch:`。
- 伸缩后的媒体超出内存中的派生媒体预算（默认 256 MiB，用 `OCP_DERIVED_CACHE_MEMORY_BYTES` 设置）时，会溢出到磁盘上的派生媒体缓存。如果磁盘缓存也放不下，加载就会失败：

  ```text
  audio clips need 2600.0 MiB of spooled media, above the available disk
  capacity for the 256.0 MiB derived-media budget
  ```

  请缩短片段，或用 `OCP_DERIVED_CACHE_BYTES` 提高磁盘上限（默认 2 GiB）。
- 重命名轨道不会改写指向它的 `@audio_clip ... | track:` 引用，校验随后会报告这个失效的引用。请把片段的 `track:` 改成新名称。

## 混音器与路由

- `Bus routing cycle`：`routes:` 必须构成有向无环图。删除形成环路的那条路由。
- 校验器和图形界面会拒绝高于 +12 dB 的发送。高于单位增益的发送会更猛地推动总线；要调整电平，请使用总线返回轨道的推子。
- 被独奏的总线会保留它的输入，输出也继续运行。如果某条轨道在独奏时没了声音，说明它没有路由到独奏所保留的那部分路由图。独奏本来就是这样工作的。

## 各插件格式的特殊情况

AU 插件在 macOS 上进程内渲染。OpenCompose 托管乐器（`aumu`）、效果器（`aufx`）和音乐效果器（`aumf`）。工程可以用显示名称（`Apple: DLSMusicDevice`）、四字符代码（`aumu:dls:appl`）或 `.component` 路径指定 AU。见 [AudioUnit hosting](../../au-hosting.html)（英文）。发生器、混音器、声像器、输出和格式转换类单元不受托管：指定这类单元的轨道渲染为静音，`ocp render` 会打印原因。AU 只有一个立体声输出，所以请求辅助输出对的发送会报错。宿主的速度和拍号通过 `HostCallbacks` 传给单元；不接受该属性的单元保持固定的上下文，并记录一条警告。AUv2 没有逐音符表情的入口，因此 MPE 表情会被丢弃，按通道的弯音和触后仍然有效。实时渲染线程与离线渲染使用同一个实例构建 AU。AU 预设试听不在已验证的范围内。

VST3 插件并非在所有情况下都能收到原始 MIDI CC，请改用插件自己的参数。MIDI 2.0 不会传递。速度和拍号映射点在每个音频块开头传递，所以 VST3 插件看到的速度变化是阶梯式的。

多输出乐器以及完整的状态和预设往返只实现了一部分。离线渲染和实时流式播放都会采集辅助输出。当已加载的声源无法提供所请求的输出对时，只播放主输出对，并打印一条警告。

插件原生编辑器只在 macOS 上经过验证。

VST3 状态会同时写入处理器和控制器，所以编辑器显示的是已加载的预设，而不是初始化音色。带有额外状态流的插件仍可能无法完整往返。

插件效果器参数在建立效果链时应用。播放期间，自动化通道按音频块逐块改变它们。

## VS Code 扩展找不到语言服务器

扩展会启动 `ocp-lsp`。用 `cargo build`（或 `cargo build --release`）构建它，然后把 `ocp-lsp` 放进 `PATH`，或在扩展设置中把 `opencompose.lsp.path` 设为可执行文件的绝对路径。该设置为空时，扩展会先尝试相对于扩展目录的开发路径。明确指定路径最可靠。

## 导出诊断包

按 **Mod+Shift+D**，或在偏好设置的 **帮助与关于** 页面点击 **导出诊断信息…**。选择 `opencompose-diagnostics-<stamp>.tar.gz` 的保存位置后，应用写出压缩包并在文件夹中显示它。把这个文件附在报告里。

压缩包是 gzip 压缩的 tar 文件，包含以下条目：

| 条目 | 内容 |
|---|---|
| `manifest.json` | 生成时间、应用版本、构建配置，以及每个条目的 `included` 或 `missing` 状态和原因 |
| `logs/recent.log` | 应用内日志 |
| `project/project.ocp` | 当前工程的源文本（如果有） |
| `environment.json` | 操作系统、架构、系统版本、应用版本、构建配置、工程路径、日志路径 |
| `plugins/scan-summary.json` | 插件扫描摘要 |
| `errors/last-error.txt` | 最近一次错误（如果有） |

无法收集的部分会在清单中标为 `missing`，并附上原因。
