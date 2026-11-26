# 智能体接口 {#agent-surface}

运行中的 OpenCompose 会在回环接口上提供一个 HTTP 服务器。脚本、编辑器或模型通过它读取和编辑当前打开的工程，不需要屏幕自动化；内置的 AI 助手调用的也是同一套工具目录。从用户角度的操作说明见[让智能体作曲](manual/agents.md)；设计约定见 [Agent feedback loop](../agent-loop.html)（英文）。

## 一个服务器，三种接口 {#one-server-three-surfaces}

| 接口 | 路由 | 说明 |
|---|---|---|
| MCP | `POST /mcp` | 协议版本 `2024-11-05` 的 JSON-RPC 2.0：`initialize`、`tools/list`、`tools/call`。 |
| 直接调用 | `POST /call` | 每个请求调用一个工具：`{"tool": "...", "args": { ... }}`。 |
| AI 助手 | `GET /ai/tools`、`GET /ai/toolsets`、`POST /ai/complete` | 内置 AI 助手可用的工具、它的工具集，以及代它执行的一次服务商调用。 |

`GET /health` 不需要令牌即可应答。`GET /tools` 返回完整的工具目录。

三种接口都读取 `tool_registry::catalog`。每个 `ToolSpec` 把描述和 JSON schema 与一个类型化的 `ToolId` 配对。身份表负责线上名称、所属领域、AI 助手可见性、是否修改工程的标志以及审批策略。后端分发对 `ToolId` 做穷尽匹配，每个服务商收到的都是同一份公布的 schema。内置工具集引用这些身份；用户保存的工具集保留原有的线上名称。

参考类和走带类工具用类型化的请求结构体同时完成请求解码和 schema 生成。为了兼容，它们的外部解码器保留可选的 `null` 值并忽略扩展字段。AI 助手按它公布的对象 schema 校验，拒绝未知字段。没有参数的工具会忽略外部调用者发来的任何载荷，而它在 AI 助手中的 schema 只接受空对象。解码失败时，内部会保留类型化的工具身份、错误码和已知字段，现有的走带响应和参考结果保持原来的线上格式。

```text
tool_registry::catalog  ──►  /tools, /mcp tools/list, /ai/tools
        │                              │
        ▼                              ▼
agent_api::dispatch  ◄──  POST /call, POST /mcp tools/call, panel calls
```

## 工具目录 {#the-catalog}

<!-- tool-registry: mcp=128 panel=100 -->

工具目录中每个 MCP 工具对应一条记录。内置 AI 助手另外增加三个本地工具，它们的处理函数在智能体循环中，不在 `dispatch` 中：

- `edit_subtree` 提议替换一个子树，由你决定是否接受。在没有人可以询问的一轮中，它会在该轮内直接应用替换。
- `plan_update` 记录对话的目标和计划。
- `ask_user` 在需要你做决定时问你一个问题。在无人值守的一轮中，它会立即回答当前没有用户（见 [Agent feedback loop](../agent-loop.html)（英文））。

工具按领域分组：工程与编排、轨道、预设、效果器、设备、总线、走带、渲染、分析、MIDI、插件与插件索引、SoundFont、采样、内容、会话、撤销、事务、技能、音频以及系统能力。

`project_search` 是两种接口都提供的只读名称查找工具。调用方在猜测轨道、预设、乐段、总线、段落、标记、效果器或乐句的名称之前，先用已知的词搜索。返回结果是数量有限的命中列表，每条都带有可寻址的 `path`；段落和标记的 `path` 指向 `sections`。

## AI 助手可以调用什么 {#what-the-panel-may-call}

AI 助手能调用哪些工具由 `ToolId` 元数据决定，与传输方式无关。AI 助手只公布已启用的工具，外加 `edit_subtree`、`plan_update` 和 `ask_user`。拒绝某个工具时，拒绝信息会写明该工具被排除的原因。

AI 助手排除以下工具：

- 读写任意文件路径或列出文件系统路径的工具：`project_open`、`project_save`、`midi_import_preview`、`midi_import_apply`、`midi_export`、`plugins_save_state` 和 `sampler_assets`。
- `project_new`，因为它会替换当前会话。
- 进程级的插件操作和原生窗口：`plugins_scan`、`plugins_load`、`plugins_open_gui`、`plugins_close_gui` 和 `plugins_has_gui`。
- 写入应用内容目录和 SoundFont 文件夹的工具：`content_library_register`、`content_library_remove`、`content_library_rescan`、`soundfont_install` 和 `soundfonts_remove`，以及需要联网刷新的 `plugin_index_refresh`。
- 主机音频配置和设备级静音：`audio_devices`、`audio_config`、`transport_mute` 和 `transport_unmute`。
- 不限长度的渲染：`render_offline` 和 `render_stems`。请改用 `render_range`；加上 `stems: true` 后，它能对一个时间窗口完成同样的工作。
- 通过 `project_edit` 整体替换子树，这类操作必须走 `edit_subtree` 提议。
- `sessions_delete`，它会删除已保存的对话。
- 主机能力查询：`system_capabilities`。

## 工具集 {#toolsets}

一个对话可以把 AI 助手的工具缩小到若干个具名工具集。工具集就是一组工具名称。对话提供你所选工具集的并集，所以取消选择某个工具集，会把它的工具同时从公布的列表和请求中移除。

内置工具集划分了 AI 助手的全部工具，所以每个工具都能被用到。`~/.opencompose/ai/toolsets.json` 可以添加你自己的工具集，但不能重新定义内置 id。如果该文件损坏，AI 助手仍会使用内置工具集正常打开，并记录这个问题。选择只能缩小策略范围：被排除的工具即使出现在某个工具集中，也仍然被排除。

## 技能 {#skills}

技能是一份简短的参考笔记，模型需要时才会读取。内置技能编译在应用中，`~/.opencompose/ai/skills/<id>.md` 可以添加你自己的技能，但不能重新定义内置 id。索引中不包含笔记正文。`list_skills` 返回每个技能的 id、标题、类别、版本以及它提到的工具，`read_skill` 返回一篇笔记。

如果某次拒绝正好有内置技能解释，应答会在错误旁附上 `"skill": "<id>"`，调用方可以读那篇笔记，了解该怎么完成任务。错误文本本身保持不变。

## 事务与修订号 {#transactions-and-revisions}

每个修改类工具都接受 `expected_revision`。如果它与当前修订号不符，调用会失败，较新的工作保持不变。

- `transaction_begin` 为工程、DSL 文本、未保存标志、文件路径和撤销标记创建快照，并返回基准修订号。
- 在打开的批处理中，修改不会逐个发布修订号，批处理内的每次调用 `expected_revision` 都接受基准修订号。
- `transaction_commit` 发布一个修订号，并把整个批处理合并为一条撤销记录。
- `transaction_rollback` 逐字节恢复快照，丢弃批处理的撤销和重做记录，并把修订号向前推进，让过期的校验失败。
- 失败的修改不会改变任何东西，批处理保持打开。`dispatch` 把错误作为调用结果返回，之前的编辑仍留在批处理中。客户端直接持有的批处理只会被显式的 `transaction_rollback` 丢弃；提交失败时批处理仍为持有者保持打开。AI 助手或无人值守的一轮在任何结束方式下（包括取消和停止）都会提交它自己的批处理。
- 批处理打开期间，保存、撤销和重做都会被拒绝。

完整的约定，包括智能体对编辑之后才完成的渲染可以作何假设，见 [Agent feedback loop](../agent-loop.html)（英文）。

## 凭据 {#credentials}

服务器绑定 `127.0.0.1` 的 38642 端口，如果被占用，则使用其后二十个端口中第一个空闲的端口。`OPENCOMPOSE_AGENT_PORT` 可以修改基准端口。`~/.opencompose/agent.json` 保存 `{ "port", "token", "pid", "version" }`。以 `X-OpenCompose-Token: <token>` 或 `Authorization: Bearer <token>` 的形式发送令牌。

凭据属于单个进程，绝不会写入工程或日志。

## AI 助手的服务商通道 {#the-panels-provider-path}

`POST /ai/complete` 通过 AI 助手使用的编码器执行一次服务商调用。请求体为 `{provider, messages, tools?, max_tokens?}`，成功时的应答是解码后的线上消息，旁边附 `"ok": true`。

失败时，应答使用该失败类别对应的 HTTP 状态码，请求体为 `{"ok": false, "error": "<message>", "kind", "retryable", "reauth"}`：

| 状态码 | 含义 |
|---|---|
| 400 | 请求格式错误。 |
| 401 | 服务商拒绝了凭据。 |
| 403 | 服务商不允许使用该模型。 |
| 409 | 保存的服务商或端点需要修正。 |
| 429 | 触发速率限制。 |
| 502 | 回复无法使用或被截断。 |
| 503 | 服务商超时或无法连接。 |

`error` 始终是普通字符串，所以在引入失败类别之前编写的调用方仍能正常工作。`retryable` 表示再次发送同一轮是否可能成功，`reauth` 表示操作者是否需要重新登录。该端点从不记录请求或凭据。密钥保存在操作系统的凭据存储中，绝不会写入工程；服务商的设置方法见[让智能体作曲](manual/agents.md)。

## 添加工具 {#adding-a-tool}

1. 在 `tool_registry/identity.rs` 中添加身份及其元数据。
2. 在 `tool_registry::catalog` 中添加它的 `ToolSpec` 和输入 schema。
3. 在 `agent_api::dispatch` 的穷尽匹配中添加该 `ToolId` 的分支。
4. 在身份元数据中设置 AI 助手可见性。被排除的工具需要写明原因。
5. 把该 `ToolId` 加入负责其领域的内置工具集。
6. 如果这个工具改变了用户可以提出的请求，在[让智能体作曲](manual/agents.md)中描述它。页面中出现工具目录里没有的工具名时，`scripts/check-docs.sh` 会报错。
