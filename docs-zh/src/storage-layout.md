# 磁盘存储 {#on-disk-storage}

一个工程就是一个纯文本 `.ocp` 文件。OpenCompose 为它保存的其他内容都放在工程文件夹之外，位于配置文件夹 `<config>` 下。`<config>` 默认是 `~/.opencompose`；设置 `OPENCOMPOSE_CONFIG_DIR` 可以改用独立的档案，参见[偏好设置](preferences.md#separate-configuration-profiles)。

这里几乎所有内容都可以重建。编辑历史、恢复副本、对话、AI 笔记或缓存，要么会在使用中重新生成，要么用来还原工程，但它们都不是工程本身。要保住工程，就保住它的 `.ocp` 文件。

## 路径键 {#the-path-key}

每个按工程存放的数据都以**路径键**命名：工程规范路径的 SHA-256 的前 8 个字节，写成 16 位小写十六进制数。同一个键用于同一个工程文件的编辑历史、AI 笔记和恢复副本。工作区文件夹的对话对文件夹路径使用同样的规则。尚不存在的文件通过它所在的文件夹解析，所以首次保存前后它的键相同。

从键无法反推出路径。每份数据都另存一个记录可读路径的小文件，这样被移动的项目还能重新找到，存储页也能显示每项数据属于谁。

## 目录结构 {#layout}

```text
<config>/
  locks/                        open-project and open-conversation locks
  history/<path key>/
    project.json                marker: version, canonical path, content SHA-256
    entries.jsonl               one stored entry per line
    index.bin                   fixed-size rows: the live linear history
    state.json                  cursor and the document at the cursor
  recovery/
    <key>/
      <stamp>-<key>.ocp         one whole project document
    <key>.owner                 the path and content SHA-256 it belongs to
  ai/
    projects/
      index.json                key to project path
      <key>.md                  the project's AI note
    sessions/
      <workspace key>/
        workspace.json          the folder this key comes from
        <session id>/
          meta.json             title, times, provider, model, project path
          messages.jsonl        one record per model turn
          view.json             where the current view starts in messages.jsonl
          results/<sha256>.txt  a full tool result that a saved row names
          versions/<sha256>.ocp the project version a human turn was sent with
      audition-media/           clips that a listening request replays
      audition-renders/         full-rate renders kept for listening
      detached/<session id>/    a conversation with no workspace
      locks/session-<id>.lock   the process that has one conversation open
  plugin-folders.json           plugin folders you added
  content-libraries.json        sound folders you added
  runtimes/<runtime id>/        installed helper runtimes
  plugin-index/scopes/          cached signed plugin catalogs
  presets/                      the preset library
  sample-providers/             sample provider plugins
  gui-settings.yaml             GUI settings
  cache/                        caches, only in a selected profile
  logs/                         app and diagnostic logs, only in a selected profile
```

## 打开锁 {#open-locks}

打开某个工程的进程持有 `<config>/locks/project-<path key>.lock`。同一时间只有一个进程能打开某个工程。另一个桌面进程或无界面进程试图打开同一个工程时，会在任何内容改变之前被拒绝：它的文档、历史和文件都保持原样。另一个工程取代当前工程或进程退出时，锁会释放。锁文件不属于工程数据，列表和清理任务都会跳过它。

编辑历史没有单独的锁，因为只有持有工程锁的进程才会打开它的历史。清理任务或手动删除其他工程的历史、笔记或恢复副本时，会先获取该工程的锁；如果锁被另一个进程持有，就跳过这些数据。**另存为**、**移动项目**以及在别的路径打开工程时，都会先获取目标路径的锁，再往那里移动任何东西。

保存的对话有自己的锁 `ai/sessions/locks/session-<id>.lock`，参见[保存的 AI 对话](../ai-sessions.html)（英文）。

## 编辑历史 {#edit-history}

`<config>/history/<path key>/` 保存一个工程的编辑历史。

| 文件 | 内容 |
|---|---|
| `project.json` | 标识 `{version: 1, path, sha256}`：工程的规范路径，以及应用最近一次读写时文件内容的 SHA-256。每次成功写入工程都会更新它，包括智能体发布、退出时的写盘和用**新建工程**创建的工程。 |
| `entries.jsonl` | 每行一条记录，包含压缩后的正向和反向补丁、作者和范围。普通编辑不写可选字段 `cause`；当前版本不认识的 cause 按普通编辑加载。显示文本不保存。这个文件只增不减：被重做分支取代的行会一直留着，直到历史被重写。 |
| `index.bin` | 当前线性历史的定长行，从旧到新。丢弃重做分支时会截断它。 |
| `state.json` | 光标、光标所在记录的 id 以及光标处的文档文本，以一次原子替换写入。如果较短的索引让该记录换了行，打开历史时会按 id 找到它。只有索引中已没有该 id 时才会丢弃这个头部，所以写了一半的文件绝不会把光标和错误的文档配在一起。 |

应用先按路径查找历史。如果该路径没有历史，就把文件内容的 SHA-256 与每份历史的标识比对。工程文件已不存在的历史会跟随内容相同的文件，所以在应用之外移动或复制的文件仍能保留撤销。旧文件还在时复制历史，旧文件已不在时移动历史。位于未挂载卷上的文件仍拥有自己的历史。用**新建工程**创建的工程从全新的历史开始，绝不接收其他文件的历史。

如果历史的重写被中断，应用下次打开该历史时会用 `.old` 文件夹完成重写。被中断的复制留下的暂存文件夹以及其他残留文件夹都会被删除，而不会被拿来使用。

**移动项目**会把历史移到目标路径。**另存为**会复制历史，并保留源历史。两者都会把标识更新为目标路径。

日志每秒写盘一次，智能体工具修改工程后以及应用退出时也会写盘，所以就算在退出前一刻做的编辑也保留撤销记录。尚未结束的智能体批处理要等提交后才写入日志。

## 恢复副本 {#recovery-copies}

`<config>/recovery/<key>/` 保存整份文档的恢复副本。已保存的工程使用路径键；从未保存的工程使用应用会话作为键。副本命名为 `<stamp>-<key>.ocp`，文件夹旁边的 `<key>.owner` 文件记录规范路径和内容的 SHA-256。

每个工程保留最新的 10 份副本。应用也会读取以早期版本的键（文件标识和较早的 64 位路径哈希）保存的副本，所以这些副本仍能恢复。**移动项目**会把副本移到目标路径的键下。参见[自动保存与恢复](autosave.md)。

## AI 笔记与对话 {#ai-notes-and-conversations}

`<config>/ai/projects/` 为每个工程文件保存一份 Markdown 笔记。`index.json` 把每个键映射到可读路径，`<key>.md` 是笔记本身。这里的键是 `.ocp` 文件的路径键，而不是它所在文件夹的。**移动项目**会移动笔记；**另存为**会复制笔记，并保留源笔记。

`<config>/ai/sessions/` 保存对话。对话属于某个工作区文件夹，而不是某一个 `.ocp` 文件，存放在工作区键下：

- `workspace.json` 记录该文件夹以及这个键对应过的所有路径，这样被移动的工作区可以重新关联。
- `<session id>/meta.json` 记录标题、时间、服务商、模型、工作区和工程路径。
- `<session id>/messages.jsonl` 是对话记录，只追加不修改。
- `<session id>/view.json` 指向最近一条替换了对话内容的记录，所以读取时只需解析当前视图。读取时会检查它，如果与对话记录不符就重建。
- `<session id>/results/<sha256>.txt` 保存放不进消息行的大型工具结果。该行用 `result_sha` 代替文本。清理任务会删除本对话中没有任何已保存行引用的结果文件；分支对话会复制它继承的行所引用的文件。读取时会校验哈希、限制读取的字节数，并拒绝内容与文件名哈希不符的文件。
- `<session id>/versions/<sha256>.ocp` 保存附在人类发言上的工程版本，以内容哈希命名。
- `detached/<session id>/` 保存没有工作区的对话，例如未保存工程的对话。
- `audition-media/` 保存之后的请求要重放的短片段，`audition-renders/` 保存为试听保留的全采样率渲染。这两个文件夹共用 256 MiB 的预算。

在 Unix 上，这些文件夹的权限为 `0700`，文件为 `0600`。每个工作区最多保留 32 个对话。**偏好设置 > 存储**中的**清除旧对话历史**会从每个对话倒数第二新的摘要处重写对话记录，然后删除只被丢弃记录用到的文件。AI 面板的 `/clear` 命令对当前对话做同样的事。两者都会报告丢弃了多少条记录、释放了多少空间。参见[保存的 AI 对话](../ai-sessions.html)（英文）。

## 缓存 {#caches}

缓存都可以重建，可以在应用关闭时删除。使用独立档案时，缓存放在 `<config>/cache/` 中，所以两个档案永远不会共用或互相挤掉对方的文件。不使用档案时，缓存放在平台的缓存文件夹中：

| 平台 | 不使用档案时的媒体缓存 |
|---|---|
| macOS | `~/Library/Caches/OpenCompose/media` |
| Windows | `%LOCALAPPDATA%\OpenCompose\cache\media` |
| Linux | `$XDG_CACHE_HOME/opencompose/media`，或 `~/.cache/opencompose/media` |

其他缓存与媒体缓存位于同一个上级文件夹中。

| 文件夹 | 内容 |
|---|---|
| `media/` | 解码后的媒体、重采样缓冲区和派生的片段音频，按源路径、修改时间和大小作为键。 |
| `media/` | 人声分析（`.ocn`）以及修音与和声烘焙结果，预算 1 GiB。 |
| `preset-trims/` | 测得的预设响度补偿。 |
| `audition-scratch/` | 等待交给对话的渲染。 |

存储页把解码后的音频、处理后的音频和人声缓存分行显示，**清理未使用的片段音频**会同时清理这三项。媒体缓存和派生音频缓存各自有磁盘占用上限。

## 应用文件夹 {#application-folders}

| 文件夹 | 内容 |
|---|---|
| `<config>/runtimes/` | 已安装的辅助运行时，例如分轨分离器。 |
| `<config>/plugin-index/scopes/` | 缓存的已签名插件目录，每个签名机构和来源一个范围。`OCP_PLUGIN_INDEX_DIR` 可以移动这个文件夹，但不改变信任的密钥。 |
| `<config>/presets/` | 预设库，包括从音色补丁编辑器保存的音色补丁。 |
| `<config>/sample-providers/` | 已安装的采样服务插件。`OPENCOMPOSE_SAMPLE_PLUGIN_DIR` 可以指向别的文件夹。 |
| `<config>/logs/` | 使用独立档案时，这里保存每次运行的 `opencompose-app.log`，以及带诊断参数启动时的诊断日志。不使用档案时，两者都写入平台的日志文件夹（macOS 上是 `~/Library/Logs/OpenCompose`）。存储页会显示实际使用的文件夹。 |

每个日志文件夹中每个程序最多保留 20 份诊断日志，每次运行都会删除超出部分中最旧的那些。应用日志超过 40 KB 后会从头开始写。

## 文件夹列表 {#folder-lists}

你添加的文件夹保存在两个小文件中，偏好设置的文件夹页会编辑这两个文件。

| 文件 | 内容 |
|---|---|
| `<config>/plugin-folders.json` | 你添加的插件文件夹，最多 32 个。`OCP_PLUGIN_FOLDERS` 可以指向别的文件。 |
| `<config>/content-libraries.json` | 你添加的音色文件夹。每个文件夹是一个内容库，附带上次扫描找到的乐器文件；只有采样的文件夹不列出任何乐器文件。每次修改都会把上一版保存为 `content-libraries.json.bak`。`OPENCOMPOSE_CONTENT_CATALOGUE` 可以指向别的文件。 |

如果应用无法读取其中某个文件，它会报告问题，并且绝不覆盖该文件。从列表中移除文件夹不会删除其中的文件。

## 过期数据 {#stale-data}

应用只删除已不存在的工程和工作区文件夹的数据，别的一概不删。位于未挂载卷上的文件只是暂时无法访问，并不算不存在，所以它的数据会保留。

如果某项数据所属的文件或文件夹在可访问的卷上已不存在，这项数据就是“已不存在”。已不存在的数据如果 30 天没有被动过，就是“即将删除”。应用启动 60 秒后，会在 UI 线程和音频线程之外运行一次清理，删除所有即将删除的数据。当前打开的工程的数据，以及有一轮对话正在进行的对话，永远不会被删除。

**偏好设置 > 存储**列出每个工程和文件夹的数据：编辑历史、AI 笔记、恢复副本和对话，每项都附有大小以及是否已不存在或即将删除。只有已不存在的数据才提供**删除**，并且应用在删除前会再确认一次它确实已不存在。该页面还列出各应用文件夹及其大小，以及片段缓存和人声缓存。
