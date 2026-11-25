# 快速上手

本教程从仓库源码构建 OpenCompose，并播放随附的示例工程。不需要签名的安装包。

## 准备工作

- Rust 1.95.0（edition 2024）。这是支持的构建基线。
- Node.js LTS 及 npm。图形界面前端使用 SolidJS 和 Vite。
- 所用平台的 Tauri 构建依赖。在 macOS 上请安装 Xcode Command Line Tools。
- SoundFont 音色库（可选）。图形界面安装包自带 MIT 许可的 `FluidR3Mono_GM.sf3`，GM 预设无需下载即可发声。用命令行渲染时如需外部音色库，传入 `--soundfont <path>`。只用原生合成器和插件的工程不需要音色库。

Rust 1.95.0 是本地测试过的基线，CI 会在 macOS、Linux 和 Windows 上检查这个编译器版本。更旧的编译器能否构建本项目没有经过测试。

原生 CI 任务还会编译以下组合：不带 MP3 导出的图形界面和命令行，分别启用插件托管和 MP3 导出的引擎，以及本地采样提供程序的运行器。这些任务只做编译检查，不能说明每个平台、架构或可选功能组合在运行时都正常。

SoundFont 预设不需要任何系统库，引擎自带读取器和播放器。

## 构建并运行图形界面

在仓库根目录下安装前端依赖，然后运行 Tauri 开发命令。Tauri 会先启动 Vite，等它就绪后再打开应用。

```sh
cd crates/oc-gui/frontend
npm ci
npm run tauri dev -- --release
```

另开一个终端，在仓库根目录构建命令行。

```sh
# 命令行
cargo build --release -p oc-cli
./target/release/ocp --help
```

可执行文件名为 `ocp`。直接运行 `cargo build --release` 还会构建语言服务器 `ocp-lsp`，VS Code 扩展会用到它。

## 示例工程

先从 `examples/opensynth_showcase.ocp` 开始。它大约一百行，使用三个 OpenSynth 音色补丁，不需要 SoundFont，也不需要插件。

```sh
./target/release/ocp validate examples/opensynth_showcase.ocp
./target/release/ocp render examples/opensynth_showcase.ocp \
    --output /tmp/showcase.wav --sample-rate 48000
```

第二个示例 `examples/dynamic_eq.ocp` 只有 25 行：一个持续音，主输出上挂一个六段动态 EQ。

```sh
./target/release/ocp render examples/dynamic_eq.ocp \
    --output /tmp/dynamic_eq.wav --sample-rate 48000
```

## 其他常用命令

下面四个命令一开始就会用到：

```sh
ocp validate song.ocp                    # 解析并做语义检查；失败时打印原因
ocp fmt -w song.ocp                      # 规范格式化，原地写回
ocp export song.ocp --format midi -o song.mid
ocp midi2ocp input.mid -o song.ocp       # 把 MIDI 文件转换成工程
```

不加 `-w` 时，`ocp fmt` 把结果写到标准输出。可以先运行 `ocp fmt song.ocp > /tmp/x`，和原文件比较之后再决定是否保留。`ocp parse --json` 打印 AST，`ocp export --format json` 则打印可直接渲染的工程状态，渐强渐弱等记谱标记已经展开。全部子命令见 [命令行](cli.md)。

## 构建文档站点

```sh
scripts/docs.sh build    # 手册输出到 book/，API 参考输出到 book/api/
scripts/docs.sh serve    # 在 http://127.0.0.1:3000 上提供手册
```

## 先试试这五件事

1. 检查文档。`ocp validate examples/opensynth_showcase.ocp` 会解析并校验工程；如果工程里有音频片段，还会解码每个引用的媒体文件。成功时打印工程名和轨道数。
2. 渲染。上面的 `ocp render` 命令会写出一个 48 kHz 的 WAV 文件，任何播放器都能打开。整个混音（包括主输出效果器）都是离线渲染的。
3. 在图形界面中打开。点击工具栏上的 **打开文件**（或按 **Mod+O**），选择 `examples/opensynth_showcase.ocp`。按 **Space** 从光标处播放，点击标尺即可跳转。
4. 改一个数字。按 **D** 打开 DSL 侧栏，修改某个预设里的 `amp.release`，再按 **Mod+Enter** 解析并应用。**Mod+Z** 可以撤销这次修改。
5. 导出分轨。`ocp render examples/opensynth_showcase.ocp -o /tmp/mix.wav --stems /tmp/stems` 会为每条轨道和每条总线各写一个主输出之前的 WAV 文件，外加一份 `stems.json` 清单。

接下来请读 [.ocp 文件](dsl.md)。如果想先在应用里上手，请读 [主窗口](gui.md)。
