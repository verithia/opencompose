# OpenCompose 手册

OpenCompose 是一款以文本文件保存歌曲的数字音频工作站。一个 `.ocp` 文档用可读的文本行写下轨道、乐段、音符、混音器设置和效果器。同一个文件可以手工编辑，可以交给模型生成，可以用命令行渲染，也可以在桌面应用里播放。

## 你会用到的部分

DSL 即 `.ocp` 工程格式，当前版本为 0.6.0。它可以直接阅读，能在版本控制里比较差异，也不依赖某一个引擎。详见 [.ocp 文件](dsl.md)。

`ocp` 命令行用来解析、校验、格式化、导出和渲染工程，还能分析音频、转换 MIDI。详见 [命令行](cli.md)。

图形界面是 Tauri 桌面应用，包含时间线、钢琴卷帘、混音器、设备架、音色补丁编辑器、插件浏览器、DSL 侧栏和 AI 助手。详见 [主窗口](gui.md)。

运行中的图形界面还提供 MCP 和 HTTP 智能体接口，见 [让智能体作曲](agents.md)。

## 代码结构

![OpenCompose 代码结构：各个入口、带有唯一智能体循环的桌面后端、引擎 crate 和格式 crate](../../manual/code-structure.svg)

桌面界面和智能体客户端调用同一个后端。AI 助手面板和无界面的 `POST /ai/turn` 运行的是后端里同一个智能体循环。面板负责显示循环产生的事件，并把你对它提问的回答传回去。图底部的格式 crate 采用宽松许可证，编辑器和其他工具可以直接依赖它们。

## 章节

1. [快速上手](getting-started.md)：构建命令行和图形界面，打开示例工程，试试最初的五件事。
2. [.ocp 文件](dsl.md)：语言本身。歌曲、段落、乐段、片段、音符、打击、自动化、预设、轨道、总线、主输出、效果器和音频片段。
3. [命令行](cli.md)：每个 `ocp` 子命令及示例。
4. [主窗口](gui.md)：走带、时间线、钢琴卷帘、混音器、设备架、音色补丁编辑器、插件浏览器、DSL 编辑器、AI 助手、诊断和快捷键。
5. [内置设备](devices.md)：混音器设备参考。每个设备做什么、有哪些控件、什么时候用。
6. [OpenSynth](synth.md)：内置合成器。振荡器、扭曲、齐奏、滤波器、包络、LFO、调制矩阵、效果架和预设导入。
7. [让智能体作曲](agents.md)：智能体接口能做什么、事务保证什么，以及 AI 助手的限制。
8. [插件](plugins.md)：支持托管哪些插件格式、工程如何指定插件，以及免费插件目录里有什么。
9. [故障排查](troubleshooting.md)：用户常遇到的故障，以及如何收集诊断包。

## 约定

- 路径相对于仓库根目录。
- **Mod** 在 macOS 上指 Cmd，在 Windows 和 Linux 上指 Ctrl。
- 轨道、总线和预设的名称区分大小写。名称就是路由的标识。
- 小节和拍的位置从 1 开始计数。区间的终点不包含在内。
- 各章描述产品当前的行为。设计理由、测量数据和未决问题写在 [文档目录](../README.md) 列出的设计文档里，每章都会链接到它依赖的那些文档。

## 设计文档

- [Audio clips](../../audio-clip.html)（英文）和 [Time-stretch](../../audio-clip-stretch.html)（英文）
- [混音器路由](../mixer-routing.md)
- [Modulation](../../modulation.html)（英文）和 [Filter character](../../filter-character.html)（英文）
- [鼓组](../drum-kit.md)、[摇摆](../timing.md#摇摆) 和 [编排](../arrangement.md)
- [Devices](../../eq-editor.html)（英文）、[Vocal chain](../../vocal-chain.html)（英文）、[Limiter](../../limiter.html)（英文）和 [Multiband](../../multiband.html)（英文）
- [MIDI 导入与导出](../midi-import-export.md) 和 [实时 MIDI](../midi-live.md)
- [AudioUnit hosting](../../au-hosting.html)（英文）：哪些 AU 组件可以离线渲染，哪些只能扫描
- [Agent feedback loop](../../agent-loop.html)（英文）
- [Compatibility and originality](../../compatibility.html)（英文）：兼容范围和原创性规则
