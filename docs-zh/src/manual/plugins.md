# 插件 {#plugins}

OpenCompose 可以加载五种插件格式的第三方乐器和效果器，并在内置目录中列出免费插件。本章介绍支持哪些格式、应用在哪里查找插件、工程如何引用插件，以及免费插件目录包含什么。

## 支持的格式 {#formats-hosted}

| 格式 | 乐器 | 效果器 | 说明 |
|---|---|---|---|
| VST3 | 支持 | 支持 | 进程内加载，可打开插件自己的编辑器窗口。 |
| VST2 | 支持 | 支持 | 进程内加载。只有一路立体声输出，没有插件编辑器；预设编辑器会显示自动生成的参数面板。参见 [VST2 plugins](../../vst2.html)（英文）。 |
| CLAP | 支持 | 支持 | 进程内加载，可打开插件自己的编辑器窗口。 |
| AU（AudioUnit） | 支持 | 支持 | 仅限 macOS。只有一路立体声输出；预设编辑器会显示自动生成的参数面板。参见 [AudioUnit hosting](../../au-hosting.html)（英文）。 |
| LV2 | 支持 | 支持 | 进程内加载。只有一路立体声输出，没有插件界面；预设编辑器会显示自动生成的参数面板。 |

免费插件目录里也可能列出独立运行的应用程序供下载，但宿主不会加载独立应用。之所以列出它们，是因为目录描述的是下载项，而不只是插件。

## 插件的查找位置 {#where-plugins-are-found}

应用会搜索四类文件夹：各格式的标准文件夹、其他音乐软件常用的文件夹、由环境变量或其他软件的设置指定的文件夹，以及你添加的文件夹。**偏好设置 > 文件夹**按这几类列出所有被搜索的文件夹，参见[偏好设置](../preferences.md#folders)。

标准文件夹：

| 平台 | 标准文件夹 |
|---|---|
| macOS | `~/Library/Audio/Plug-Ins/` 和 `/Library/Audio/Plug-Ins/` 下的 `VST3`、`CLAP`、`Components`（AU）、`LV2` 和 `VST` |
| Windows | `%COMMONPROGRAMFILES%` 下的 `VST3`、`CLAP` 和 `LV2`；`%LOCALAPPDATA%\Programs\Common` 下的 `VST3`、`CLAP` 和 `VST2`；`%LOCALAPPDATA%\LV2`；VST2 还有 `%ProgramFiles%` 和 `%ProgramFiles(x86)%` 下的 `Common Files\VST2`、`VstPlugins`、`Steinberg\VstPlugins` 和 `Common Files\Steinberg\VST2` |
| Linux | `~/.vst3`、`~/.clap`、`~/.lv2` 和 `~/.vst`；`/usr/lib` 和 `/usr/local/lib` 下的 `vst3`、`clap`、`lv2`、`vst` 和 `lxvst` |

`OCP_VST2_PATH` 中的文件夹也算作标准 VST2 文件夹。

以下文件夹存在时也会被搜索：

| 平台 | 常用文件夹 |
|---|---|
| macOS | `/Network/Library/Audio/Plug-ins/VST3`；`~/.lv2`、`/usr/local/lib/lv2` 和 `/opt/homebrew/lib/lv2` |
| Windows | `%APPDATA%\LV2`；`%ProgramFiles%\VST3`；`%ProgramFiles%\Cakewalk\Vstplugins`；`%USERPROFILE%\Documents\Plugins\VST`；安装程序记录在 `HKEY_CURRENT_USER\Software\VST` 和 `HKEY_LOCAL_MACHINE\Software\VST` 下 `VSTPluginsPath` 中的 VST2 文件夹 |
| Linux | `/usr/lib64` 和 `/usr/local/lib64` 下的 `vst3`、`clap`、`lv2`、`vst`、`lxvst` 和 `linux_vst`；`/usr/lib/linux_vst` 和 `/usr/local/lib/linux_vst` |

还有来自环境变量和其他软件设置的文件夹：

| 来源 | 文件夹 |
|---|---|
| 环境变量，所有平台 | `CLAP_PATH`、`VST3_PATH`、`VST_PATH`、`LXVST_PATH` 和 `LV2_PATH` 中的每个文件夹 |
| REAPER，所有平台 | 其资源文件夹中的 `UserPlugins/FX`，以及其 `reaper.ini` 中每一项 `vstpath…` 和 `clap_path…` |
| FL Studio，Windows | 它保存在 `HKEY_CURRENT_USER\Software\Image-Line\Shared\Plugin search paths\List` 下的插件搜索路径，以及它的 64 位 VST2 文件夹（`HKEY_CURRENT_USER\Software\Image-Line\Shared\Plugins\FL Studio` 下的 `VstDir64`） |

REAPER 的资源文件夹在 Windows 上是 `%APPDATA%\REAPER`，在 macOS 上是 `~/Library/Application Support/REAPER`，在 Linux 上是 `$XDG_CONFIG_HOME`（或 `~/.config`）下的 `REAPER`。

读取其他软件的设置时有以下限制：

- 超过 1 MiB 的设置文件不读取。
- 所有其他软件的设置加起来最多取 64 个绝对路径，相对路径会被跳过。
- FL Studio 列表中带有 `%FLPath%` 这类占位符的条目会被跳过。这类条目指向该软件自己的文件夹，里面放的是它的原生插件，而不是 VST 或 CLAP 插件。
- 在 Windows 上，来自其他软件设置、位于 `%ProgramFiles(x86)%` 下的文件夹会被跳过，因为其中是 OpenCompose 无法加载的 32 位插件。

被多个来源指定的文件夹只搜索一次，文件夹页按以下顺序把它归入第一个来源：标准位置、环境变量、常用位置、其他软件的设置、由你添加。

插件最多可以位于搜索文件夹下三层，所以 `<VST3>/<vendor>/<plugin>.vst3` 这样的结构也能找到。每个 `.vst3`、`.clap`、`.component` 或 `.lv2` 包算一个插件文件，每个 `.vst`（macOS）、`.dll`（Windows）或 `.so`（Linux）文件也算一个。一个文件可以包含多个插件，插件浏览器会逐一列出。

### 添加文件夹 {#adding-a-folder}

要搜索别的文件夹，可以在偏好设置的文件夹页或首次启动向导的第一页选择**添加插件文件夹…**。已经作为标准文件夹或你自己的文件夹被搜索的文件夹会被拒绝。你的列表保存在 `~/.opencompose/plugin-folders.json` 中，最多 32 个文件夹；`OCP_PLUGIN_FOLDERS` 可以指向别的文件。插件扫描、LV2 加载器和素材库的已安装检查都会读取这个列表。

文件夹页上的**移除**会把你添加的文件夹移出列表，其中的文件仍保留在磁盘上。

### 这台电脑上的音色 {#sounds-on-this-computer}

同一页还列出电脑上的音色：SoundFont（`.sf2`、`.sf3`）、SFZ 和采样器文件，以及音频采样（WAV、AIFF、FLAC、OGG 和 MP3）。它会搜索：

- 乐器文件夹：OpenCompose 自己的 `soundfonts` 文件夹和音色包文件夹；“文稿”下的 `SoundFonts`、`soundfonts`、`MuseScore4/SoundFonts`、`MuseScore3/Soundfonts` 和 `lmms/samples/soundfonts`；“音乐”下的 `SoundFonts`；macOS 上的 `~/Library/Audio/Sounds/Banks` 和 `/Library/Audio/Sounds/Banks`；Linux 上用户数据文件夹中的 `soundfonts`，以及 `/usr/share/soundfonts`、`/usr/share/sounds/sf2`、`/usr/share/sounds/sf3` 和 `/usr/local/share/soundfonts`；Windows 上系统盘根目录下的 `\soundfonts`；
- 其他音乐软件使用的采样文件夹：“文稿”下的 `Samples`、`Image-Line`、`Ableton/User Library`、`Ableton/Factory Packs`、`lmms/samples` 和 `Splice`；“音乐”下的 `Samples`、`Ableton/User Library` 和 `Ableton/Factory Packs`；主文件夹中的 `Splice` 和 `lmms/samples`；
- 你添加的音色文件夹。

每个文件夹最多搜索八层、两万个条目。**添加音色文件夹…**会把文件夹添加为内容库，其中至少要有一个 SoundFont、SFZ 或采样器文件，或者一个音频采样，所以只有 WAV 或 AIFF 采样的文件夹也可以添加。OpenCompose 自带的 SoundFont 音色库不计入。

安装插件后，请在插件浏览器中按**重新扫描**，或在文件夹页按**重新扫描**。

## 在工程中引用插件 {#naming-a-plugin-in-a-project}

预设用 `source` 指定插件格式，用 `plugin` 指定插件：

```ocp
@preset bass
  source: vst2
  plugin: "vst2:surge_xt"

@preset gain
  source: lv2
  plugin: "https://example.org/lv2/gain"
```

插件浏览器会显示每个插件的 id。id 有以下几种形式：

- VST3 和 VST2：`vst3:` 或 `vst2:` 加上小写的插件名，空格换成下划线（`vst3:surge_xt`、`vst2:surge_xt`）；
- CLAP：插件的描述符 id（`com.vendor.product`）；
- AU：类型、子类型和制造商代码（`aumu:dls:appl`），参见 [AudioUnit hosting](../../au-hosting.html)（英文）；
- LV2：插件 URI（`https://…` 或 `urn:…`）。

在轨道的效果器列表中，在 id 前面加上格式前缀（`clap.`、`vst3.`、`vst2.`、`au.` 或 `lv2.`）可以把查找限定在该格式，例如 `- plugin:"vst2.vst2:surge_xt"` 或 `- plugin:"lv2.http://example.org/lv2/gain"`。不加前缀时，id 可以匹配任何格式的插件。完整语法见 [.ocp 文件](dsl.md)。

## 加载和保存 Kontakt 状态 {#load-and-save-kontakt-state}

先安装并激活 Kontakt 和你要用的音色库。在插件浏览器中按**重新扫描**，用**添加预设**添加 Kontakt，再用**打开编辑器**打开它的编辑器，然后在 Kontakt 里加载一个乐器或 multi。在加载 multi 之前，Kontakt 不会发出声音。

在预设编辑器中依次打开**乐器高级设置**和**插件设置**。选择**嵌入**会把插件状态存进工程，选择**保存**会把它写入一个状态文件。基于文件的预设在 `vst3_state_file` 中指定这个文件，相对路径从工程所在的文件夹解析。Kontakt 的 `.nkm` multi 是在插件内部打开的文档，不是宿主状态。

把预设分配给一条轨道，并使用 Kontakt 中设置的 MIDI 通道。保存的状态会还原整个设置。每台打开该工程的电脑都需要安装插件和音色库，并拥有相应授权。已测试的情况见 [External plugin support](../../plugin-support.html)（英文）。

## 免费插件目录 {#the-free-plugin-catalog}

**素材库**面板列出一份已签名远程索引中的免费插件。应用通过 HTTPS 从项目的文档站点下载索引，缓存在磁盘上，并用内置于应用的 Ed25519 密钥校验。应用和 `ocp` 本身不带任何目录，所以需要联网一次才能获取。在下载到索引之前，素材库里什么也不会列出。参见 [Plugin index](../../plugin-index.html)（英文）。

索引中每个插件一条记录，包括名称、厂商、一句话简介、分类标签、许可证、主页以及一个或多个安装项。安装项写明它提供的平台和格式以及下载地址。应用会显示当前平台可用的格式和许可证，并根据安装项下载压缩包、打开安装程序或打开厂商页面。

OpenCompose 不捆绑也不托管插件二进制文件。目录中的每个安装项都链接到厂商自己的下载或发布文件，应用从那里下载。`license` 字段记录插件的许可证，写成 SPDX 标识符，免费的专有软件则写 `Freeware`。它不会改变插件本身的条款。

首次启动向导或素材库的插件列表需要索引时，或者你按下**刷新**时，应用才会获取索引。列表会先用缓存的副本显示，后台不会自动获取索引。如果缓存副本是最近六小时内检查过的，打开列表时直接使用它。**刷新**总会去获取，但最多每五分钟一次。刷新失败时应用会保留缓存副本，副本已有 14 天或更久时会给出提示。索引的格式、托管、签名、缓存和发布命令见 [Plugin index](../../plugin-index.html)（英文）。

## 限制 {#limits}

- 宿主从不修改或重新打包插件。厂商的下载地址变了，要通过更新目录来修正，而不是上传一份副本。
- VST2、AU 和 LV2 显示自动生成的参数面板，而不是插件自己的界面，并且只有一路立体声输出。参见 [VST2 plugins](../../vst2.html)（英文）和 [AudioUnit hosting](../../au-hosting.html)（英文）。
- 扫描时每个插件都在单独的辅助进程中运行，所以扫描中崩溃的插件不会让应用退出。渲染在进程内进行，所以渲染时崩溃的插件会连同应用一起退出。
