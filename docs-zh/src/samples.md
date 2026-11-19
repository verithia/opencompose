# 采样市场 {#sample-marketplaces}

OpenCompose 会搜索别人的采样目录，并一次下载一个文件到你的工程中。每个文件的许可证和署名都保存在音频旁边。OpenCompose 自己不托管任何目录：每条结果都来自已经公开发布它的市场。

底部面板、智能体工具和 `ocp sample` 使用同一个库（`oc-sample`）、同一条许可规则和同一套拒绝代码。面板和智能体工具还共用同一个多市场搜索，带分页、去重和退避；`ocp sample` 则一次只搜索一个市场。

## 本地服务插件 {#local-provider-plugins}

每个市场的搜索都通过一个本地服务插件进行：这是一个独立的程序，应用每次请求都会启动它。只有调试构建会加载这些插件。在发布构建中，每个市场都会报告 `no local sample-search plugin is installed`。已签名的插件索引中也不会列出它们。

要在调试构建中使用，先编译适配程序：

```sh
cargo build -p oc-sample --features local-provider-runner --bin oc-sample-provider
```

然后在 `~/.opencompose/sample-providers/` 中为每个市场添加一个 JSON 清单，文件名就是市场 id。`OPENCOMPOSE_SAMPLE_PLUGIN_DIR` 可以指向别的文件夹。例如 `wikimedia-commons.json`：

```json
{
  "protocol": 1,
  "executable": "/absolute/path/to/oc-sample-provider",
  "args": ["wikimedia-commons"],
  "configuration": { "timeout_seconds": 12 }
}
```

`protocol` 必须是 `1`，`executable` 必须是指向现有文件的绝对路径，否则该市场会报告没有安装插件。适配器使用的市场 id 为 `openverse`、`wikimedia-commons`、`internet-archive`、`freesound` 和 `jamendo`。每个清单可以指定自己的程序和配置。只安装你信任的程序：插件以你的用户权限运行。

应用把一个 JSON 请求写到程序的标准输入。请求包含 `protocol`、`configuration`、`network` 设置（代理和超时）、可选的 `credential` 以及 `operation`，后者是 `{"entrypoint":"search","query":{...}}` 或 `{"entrypoint":"resolve","id":"..."}`。搜索返回一个 `SamplePage`，解析返回一个 `SampleItem`。程序向标准输出写一个 JSON 对象：`status: "ok"` 加 `value`，或者 `status: "error"` 加 `message`。错误还可以带上：

- `code`：只有 `unknown_id` 和 `no_download` 会被当作拒绝，其他代码都算失败；
- `kind`：[拒绝](#refusals)一节列出的失败类型之一；
- `http_status` 和 `retry_after_secs`。

没有 `kind`，或者 `kind` 是应用不认识的值时，按 `other` 处理。传输类型和参考适配器在 `crates/oc-sample/src/local_plugin.rs` 中。每个市场的 id 和能力由应用自己的注册表记录，而不是由插件提供。

应用总是通过这套进程协议访问市场，包括带适配器测试夹具的构建。`test-providers` 特性增加一个夹具运行时，供录制好的测试显式传入使用；它不会安装市场，也不会绕过清单。

在**偏好设置 > 外部来源**中可以保存或移除 Freesound API 密钥和 Jamendo 客户端 ID。该页面会显示凭据来自环境变量还是操作系统的凭据存储。命令行只读取环境变量。

## 唯一的规则 {#the-one-rule}

如果某个市场没有可编程的搜索接口，或者其条款禁止程序化下载，OpenCompose 就不会抓取它。这个市场会被记为仅限网页，并附上原因，每次搜索都会报告它不可用：

```json
{ "id": "soundbible", "label": "SoundBible", "available": false,
  "reason": "no public search API; the site is browsed page by page and its terms do not allow programmatic download" }
```

不可用的市场永远不会从列表中消失，应用也绝不会为它编造一个空结果。

## 市场 {#markets}

| 市场 | 许可证 | 密钥 | 搜索 | 下载 |
|---|---|---|---|---|
| Wikimedia Commons | 每个文件单独标注（`LicenseShortName`、`LicenseUrl`、`UsageTerms`） | 不需要 | 支持 | 原始文件 |
| Openverse | 每条结果单独标注，来自源站（`license`、`license_url`） | 不需要 | 支持，但只在选中时搜索 | 直接音频地址 |
| Internet Archive | 每个条目单独标注（`metadata.licenseurl`） | 不需要 | 支持 | 原始文件，从条目的文件列表中选取 |
| Freesound | 每个音效单独标注（`Creative Commons 0`、`Attribution`……） | `FREESOUND_API_KEY` 或凭据存储；原始文件需要 OAuth2 令牌 | 支持 | 连接 Freesound 后为原始文件，否则为 128 kbps 预览 |
| Jamendo | 每首曲目单独标注（`license_ccurl`） | `JAMENDO_CLIENT_ID` 或凭据存储 | 支持 | `audiodownload` 中的 MP3，仅当艺术家允许下载时 |
| Looperman | 网站条款 | 不需要 | 不支持 | 不支持 |
| Splice | 网站条款 | 不需要 | 不支持 | 不支持 |
| SampleSwap | 网站条款 | 不需要 | 不支持 | 不支持 |
| SoundBible | 公有领域音效 | 不需要 | 不支持 | 不支持 |
| Mixkit | Mixkit 许可证 | 不需要 | 不支持 | 不支持 |
| Pixabay Audio | Pixabay 内容许可证 | 不需要 | 不支持 | 不支持 |
| Sample Focus | 网站条款 | 不需要 | 不支持 | 不支持 |
| Bedroom Producers Blog | 每个采样包单独标注 | 不需要 | 不支持 | 不支持 |
| Cymatics | 每个采样包单独标注 | 不需要 | 不支持 | 不支持 |

没有指定市场的搜索会询问除 Openverse 以外的所有可用市场，因为 Openverse 的搜索经常超时。选中它的标签或在请求中写明它，才会搜索它。

最后九个市场仅限网页的原因：

| 市场 | 原因 |
|---|---|
| Looperman、SampleSwap | 只有网站搜索，没有公开文档的搜索 API。 |
| Splice | 没有公开文档、可供第三方搜索的目录 API。 |
| SoundBible | 没有公开的搜索 API，且其条款不允许程序化下载。 |
| Mixkit | 没有搜索 API，下载按钮只在页面的 JavaScript 中运行。 |
| Pixabay Audio | 其公开 API 只记录了图片和视频，没有音频。 |
| Sample Focus | 每个搜索页和采样页都会对普通客户端返回浏览器验证（HTTP 403）。应用不会破解验证，也不会抓取页面。 |
| Bedroom Producers Blog | 页面对普通客户端返回 HTTP 403。它的 WordPress API 能列出免费采样包，但每个包是一个 zip 文件，而不是单个采样。 |
| Cymatics | 免费采样包需要填写邮箱才能获取，也没有公开单个采样文件。 |

每个市场在 `oc_sample::registry` 中声明一份规格：

| 字段 | 含义 |
|---|---|
| `id`、`label` | 工具参数中使用的稳定 id，以及标签上显示的名称。 |
| `id_prefix` | 该市场发出的每个 id 的前缀（`commons:`、`openverse:`、`ia:`、`freesound:`、`jamendo:`），这样下载时仅凭 id 就能判断市场。 |
| `requires_key` | 是否需要密钥；`key_env` 和 `key_account` 说明密钥从哪里来。 |
| `kinds` | 该目录能提供的类型。 |
| `license` | 一句话说明该市场发布的许可证。 |
| `search`、`download` | 当前构建是否实现了搜索和下载。 |
| `download_referer` | 下载主机要求的来源页面头部（如果有）。 |
| `page_only` | 某个市场未接入的原因，作为数据保存。 |
| `default_search` | 没有指定市场的搜索是否包含它。 |

### Wikimedia Commons {#wikimedia-commons}

使用 MediaWiki 搜索 API：不需要密钥，每个文件都有许可证。搜索会发送 `filetype:audio`、查询文本、类型对应的词，以及作为带引号短语的每个标签。同一搜索返回的 MIDI 文件和视频会被丢弃，只保留引擎能解码的 `wav`、`flac`、`mp3`、`ogg`、`oga`、`aif` 和 `aiff`。索引中没有时长字段，所以时长和声道限制在返回的页面上检查。

Commons 是档案库，不是采样包。搜索 `snare drum` 能找到真实的军鼓录音，元数据也可靠，但录音质量取决于上传者。

### Openverse {#openverse}

Openverse 汇集了 Freesound、Jamendo、Wikimedia 等来源，不需要密钥。每条结果都注明来源，并带有该来源的许可证、时长（API 中以毫秒计）、文件大小、采样率、声道数、直接音频地址和 `alt_files`。许可证因结果而异，所以同一页上可能混有 CC0、CC BY 和非商业许可的文件。搜索只发送文本，其他筛选条件都在返回的页面上检查。

### Internet Archive {#internet-archive}

`advancedsearch.php` 搜索索引，返回每个条目的标识符、标题、创作者和 `licenseurl`。查询文本会被转义，所以 `:`、`(` 之类的字符按普通文字搜索，不会被当成查询语法。搜索不发送标签。下载时读取 `metadata/<id>` 获取文件列表并选出一个音频文件，依次优先原始的 `wav`，然后是 `flac`、`aiff`、`ogg` 和 `mp3`。这样一次搜索只需一个请求，而不是每条结果一个请求。

搜索结果没有时长和声道数，所以只要设置了时长或声道筛选，Internet Archive 的结果就会全部被筛掉。没有许可证地址的条目仍会列出，下载前会先请你确认其许可证。

### Freesound {#freesound}

Freesound 是专为采样建立的目录，每个音效都有时长、声道、采样率、标签和许可证。它需要 API 密钥，先读取 `FREESOUND_API_KEY`，再读取操作系统的凭据存储（服务 `org.opencompose.samples`，账户 `freesound`）。服务器会按时长和声道筛选；应用会在返回的页面上再检查一次，同时检查类型和许可证。

Freesound 只向 OAuth2 客户端提供原始文件。**偏好设置 > 外部来源**中的 **Freesound 原始音质**用来连接你的账户：**连接 Freesound** 会要求填写你的 Freesound 应用的客户端 ID 和客户端密钥，在随机的 `127.0.0.1` 端口上监听，并以 `redirect_uri=http://127.0.0.1:<port>/callback` 打开 `https://freesound.org/apiv2/oauth2/authorize/`。它最多等待浏览器两分钟，然后在 `https://freesound.org/apiv2/oauth2/access_token/` 换取令牌。令牌和客户端凭据以 `freesound-token`、`freesound-client-id` 和 `freesound-client-secret` 保存在同一个服务下，所以之后刷新令牌不需要再打开浏览器。令牌请求使用**偏好设置 > 网络**中设置的代理。

有令牌时，下载会带上 `Authorization: Bearer …` 请求 `/apiv2/sounds/<id>/download/`，收据中记为 `quality: "original"`。遇到 HTTP 401 会刷新一次令牌并重试。如果原始文件因其他原因被拒，例如应用没有下载权限或文件已被删除，就改为下载搜索结果中的 128 kbps 预览，收据中记为 `quality: "preview_mp3"`。两种情况都会保留许可证和署名。没有凭据存储的 GUI 会话可以改用 `OPENCOMPOSE_FREESOUND_TOKEN`、`OPENCOMPOSE_FREESOUND_CLIENT_ID` 和 `OPENCOMPOSE_FREESOUND_CLIENT_SECRET`。

### Jamendo {#jamendo}

Jamendo 提供知识共享许可的音乐，只需客户端 ID，没有 OAuth 步骤。密钥来自 `JAMENDO_CLIENT_ID` 或凭据存储中的 `jamendo` 账户。每首曲目都带有 `license_ccurl`、时长、艺术家和一路流式 MP3。流式音频只用于试听。应用下载的是 `audiodownload` 中的文件，而且只在 `audiodownload_allowed` 为 true 时才下载。艺术家不允许下载的曲目可以试听，但不能加入工程，面板上会显示**仅试听**。

## 搜索 {#searching}

搜索接受以下字段：

| 字段 | 含义 |
|---|---|
| `text` | 要匹配的词。 |
| `kind` | `drum_one_shot`、`loop`、`vocal`、`fx`、`instrument_phrase` 或 `unknown`。工具和 `ocp` 也接受 `drum`、`instrument` 和 `any`。 |
| `min_seconds`、`max_seconds` | 时长的上下限，包含边界。 |
| `channels` | 声道数，通常为 1 或 2。 |
| `tags` | 结果需要相关的额外词语。 |
| `commercial_ok` | 为 `true` 时只保留许可证允许商业使用的结果，非商业许可和无法归类的许可都会被丢弃。默认为 `false`。 |
| `providers` | 要询问的市场。省略时使用默认搜索中所有可用的市场。 |
| `offset`、`limit` | 分页。`limit` 按市场计算，限制在 1 到 50 之间，默认为 20。 |
| `cursor` | 上一次结果中的 `cursor`，用于获取下一页。 |

文本和标签在发给任何市场之前都会先整理：连续空白合并为一个空格，空标签和重复标签被丢弃，文本截到 200 个字符，最多保留 8 个标签，每个最长 64 个字符。文本和标签始终是数据，绝不是查询语法：Internet Archive 会转义文本中的查询字符，Wikimedia Commons 把每个标签作为带引号的短语发送并转义其中的引号，Freesound 会给不是单个普通单词的标签加引号。

每个市场都会在返回的页面上检查类型、时长、声道数和许可证，这样服务器做过的筛选会得到确认，服务器不支持的筛选也照样生效。时长或声道数未知的结果不满足相应的限制。市场的下一个偏移量按检查前返回的数量计算，所以被筛掉的结果不会被再次获取。

各市场并行询问。某个市场宕机时，其他市场最多只会被它自己的超时拖住。

### 单条结果 {#one-result}

| 字段 | 说明 |
|---|---|
| `provider`、`provider_name` | 结果来自哪个市场。 |
| `id` | 在该市场内唯一且稳定，所以下载时不需要其他信息。 |
| `name`、`kind`、`tags` | 名称、看起来属于的类型，以及标签。 |
| `duration_seconds`、`channels`、`sample_rate` | 来源没有报告时为 `null`，绝不猜测。 |
| `license` | 机器 id、来源中的名称、地址，以及影响工程用途的四项条款。 |
| `attribution` | 工程中要重复的署名。 |
| `source_url` | 供人打开的页面。 |
| `preview_url` | 试听地址（如果来源提供）。 |
| `download_url` | 下载时获取的文件，总是来自市场。 |
| `sha256`、`bytes` | 声明的校验和与大小，在保存文件前核对。 |
| `quality` | `original` 或 `preview_mp3`。 |
| `license_ok` | 许可证是否可以自由使用。否则 `license_notice` 中是下载时显示的那句说明。 |

搜索结果中的三个地址会为显示而缩短：去掉查询字符串，过长的地址截到 160 个字符。下载时会从市场重新解析地址，从不读取这些字段。

### 搜索结果 {#the-search-result}

一次搜索返回 `providers`（每个市场的可用状态）、`pages`（每个被询问的市场一项）、`items`、`count`、`duplicates`、`commercial_ok`、`cursor` 和 `next_offset`。

结果按市场被询问的顺序轮流排列，所以前几行能看到每个市场最匹配的结果。两条结果的落地页地址相同，就说明是同一个上传被列了两次，例如被一个聚合站和它收录的网站同时列出。比较时忽略协议、主机名大小写、末尾斜杠、片段标识以及 `utm_*` 之类的跟踪参数。先被询问的市场保留这条结果，`duplicates` 统计其余的重复项。标题相同的不同上传都会保留。

每个市场各自分页，所以结果中带有一个不透明的 `cursor`。用同样的查询把它作为 `cursor` 发回，就能得到所有仍有结果的市场的下一页。已经返回最后一页的市场会被略过，没有市场还有更多结果时 `cursor` 为 `null`。格式错误、来自其他格式版本或超过 2048 个字符的游标会在询问任何市场之前被拒绝，代码为 `invalid_cursor`。`offset` 和 `next_offset` 仍可供旧的调用方使用：所有市场都从同一个偏移量开始。发送了游标的搜索返回 `next_offset: null`。

### 分页条目 {#page-entries}

`pages` 中的每一项都有以下字段：

| 字段 | 含义 |
|---|---|
| `provider`、`provider_name` | 市场。 |
| `offset` | 询问该市场时使用的偏移量；没有询问时为 `null`。 |
| `count`、`total`、`next_offset` | 该市场返回的数量、它报告的总数，以及它自己的下一个偏移量。 |
| `error` | 给人看的一句话，或 `null`。 |
| `error_kind` | `needs_key`、`provider_unavailable`、`skipped`、`rate_limited`、`not_found`、`unavailable`、`timeout`、`network`、`parse` 或 `other`；成功时为 `null`。被拒绝的密钥（HTTP 401 或 403）和缺失的密钥一样，记为 `needs_key`。 |
| `skipped`、`skip_reason` | 没有询问该市场时 `skipped` 为 `true`，`skip_reason` 为 `rate_limited` 或 `circuit_open`。 |
| `retry_after_secs` | 距离再次询问该市场的秒数（如果已知）。 |
| `http_status` | 失败请求的状态码（如果有）。 |

### 退避 {#back-off}

连续两次超时、无法连接或返回 HTTP 5xx 的市场会被跳过十分钟（`circuit_open`）。返回 HTTP 429 的市场会按它的 `Retry-After` 跳过，至少一秒、最多十分钟；没有 `Retry-After` 时跳过 60 秒（`rate_limited`）。应用只读取以秒表示的 `Retry-After`。密钥错误、记录不存在或返回内容无法读取，都说明不了市场是否健康，所以这样的市场仍可搜索。退避状态由运行中应用的所有搜索共享。

## 拒绝 {#refusals}

市场或存储拒绝时会返回一个 `SampleError`，其中有稳定的 `code` 和一句你可以据此行动的说明：

| 代码 | 原因 |
|---|---|
| `key_required` | 该市场需要 API 密钥，但没有设置。 |
| `provider_unknown` | 当前构建不认识这个市场 id，或者 id 没有已知前缀且没有指定市场。 |
| `provider_unavailable` | 该市场仅限网页、没有安装插件，或者没有直接下载。 |
| `invalid_kind` | 类型不是可接受的词之一。 |
| `invalid_cursor` | 游标格式错误、来自其他版本或过长。 |
| `unknown_id` | 该 id 不属于这个市场，或者来源已经没有这个文件。 |
| `licence_notice` | 许可证不能自由使用（非商业、禁止演绎、GFDL、社区许可或未知）。说明中会写明条款。发送 `accept_licence: true` 即可仍然下载。 |
| `no_download` | 来源没有为这条结果提供文件。 |
| `invalid_download` | 下载没有返回成功状态，或超出了大小限制。 |
| `size_mismatch` | 文件大小与声明不符。 |
| `checksum_mismatch` | 文件与声明的 SHA-256 不符。 |
| `invalid_audio` | 文件不是应用能识别的音频容器，或者文件头不完整。 |
| `project_unsaved` | 工程还没有文件，因此没有媒体文件夹。 |

不属于拒绝的失败会带一个 `kind`，调用方无需阅读说明就能决定下一步：

| 类型 | 原因 |
|---|---|
| `rate_limited` | HTTP 429。`retry_after_secs` 重复来源的 `Retry-After`，最多一天。 |
| `unauthorized` | HTTP 401 或 403：密钥或令牌缺失、错误或已过期。 |
| `not_found` | HTTP 404 或 410。 |
| `unavailable` | HTTP 5xx。`Retry-After` 的处理与 `rate_limited` 相同。 |
| `timeout` | 请求未能按时完成。 |
| `network` | 连接失败或中断。 |
| `parse` | 返回的内容不是该市场文档中说明的 JSON。 |
| `other` | 其他情况。 |

无论在面板、智能体工具还是 `ocp` 中，每个搜索请求的期限都是 15 秒，每次下载的期限都是 180 秒。

搜索不会因为某个市场不可用而拒绝。它会在 `providers` 中以 `available: false` 和 `reason` 列出该市场，并在 `pages` 中列出失败的市场及其错误。下载会用上表的代码拒绝，并把拒绝作为数据返回，而不是作为传输错误：

```json
{ "status": "refused", "code": "provider_unavailable", "message": "Mixkit is not available in this build: no search API; the catalogue is browsed in the page and the download flow is JavaScript-only." }
```

## 许可证提示 {#the-licence-notice}

许可证随文件一起保存。OpenCompose 不认为可以自由使用的许可证不会被直接拒绝：下载时询问一次，由你决定，文件在工程中保留其许可证和署名。是否在作品中使用免费采样由你决定，应用的职责是把条款告诉你。

| 许可证 | 下载时 | 工程中保留的内容 |
|---|---|---|
| CC0 1.0、公有领域 | 直接下载 | 来源，记录在附属文件中。 |
| CC BY、CC BY-SA | 直接下载 | 作者、许可证名称和地址；SA 许可另记 `share_alike: true`。 |
| CC BY-NC、CC BY-NC-SA | 询问 | 非商业条款和署名。 |
| CC BY-ND | 询问 | 禁止演绎条款。 |
| GFDL | 询问 | 必须附带许可证全文的要求。 |
| 社区许可（已有收据中） | 询问 | 条款说明和署名。 |
| Splice 预览（已有收据中） | 询问 | 预览条款、采样包名称和采样包页面。 |
| SampleSwap 上传（已有收据中） | 询问 | 公有领域声明及该网站的附加说明，以及文件夹页面。 |
| 其他 | 询问 | 来源报告的许可证名称。 |

`sample_pull` 接受 `accept_licence`（命令行中为 `--accept-licence`）。不带它时，需要提示的许可证会返回 `{"status":"refused","code":"licence_notice"}`，附上你要阅读的说明。面板会显示这段说明，并提供**仍然下载**和**取消**。你确认后，收据中会记录 `licence_accepted: true`。

署名保存在三个地方，所以复制文件夹后依然存在：音频旁边的 `.json` 附属文件、存储中 `index.json` 里的收据，以及每个工具结果的 `attribution` 字段。

## 本地存储 {#the-local-store}

采样保存在工程的媒体文件夹中：

```text
<project>/media/samples/
  openverse-snare-hit-01-72a91f6b.mp3
  openverse-snare-hit-01-72a91f6b.json
  index.json
```

- 文件名为 `<market>-<slug>-<id hash>.<ext>`：同一个 id 的文件名不变，便于阅读，也不含工程路径不能容纳的字符。扩展名取决于应用在文件中检测到的音频容器。如果文件名已被占用，下载会在名字后加上文件校验和的一部分和一个序号，而不会覆盖现有文件。
- 附属文件使用 `opencompose.sample.v1` 格式：收据、许可证和署名。
- `index.json` 使用 `opencompose.samples.v1` 格式，每个 id 一条收据。再次下载同一个 id 会替换它的收据，而不会新增一行。

下载按以下顺序进行，拒绝或失败都不会改变文件夹：

1. 许可证提示（需要提示的许可证要求 `accept_licence`）；
2. 下载，最多 96 MiB；
3. 核对声明的大小，然后核对声明的 SHA-256；
4. 检查音频容器；
5. 把音频、附属文件和更新后的索引写入一个私有的暂存文件夹，再放到位。替换 `index.json` 是最后一步；任何一步失败，本次下载已放好的文件都会被删除。

## 面板 {#the-panel}

**采样**是底部面板的一种模式，与**片段**、**钢琴卷帘**、**预设**、**混音器**、**鼓组**、**历史**和**检查器**并列。它没有默认快捷键，用命令**打开采样市场**（`view.toggleSamples`）可以打开它。**浏览**浮层中的**采样**标签页显示的也是这个面板。

- 一个搜索框和一个**搜索**按钮，按回车也能搜索。
- 类型标签：**全部**、**单发音**、**循环**、**人声**、**音效**和**乐器**。
- **时长**：**任意时长**、**2 秒以下**、**2–10 秒**或 **10 秒以上**，均包含边界。
- **允许商业使用**只保留许可证允许商业使用的结果。
- 市场标签：当前构建认识的每个市场，带一个表示是否可用的圆点。提示框显示许可证说明，或者市场不可用的原因。单击标签可以包含或排除该市场。在你改动标签之前，除 Openverse 之外的所有可用市场都会被包含。
- **外部来源**会打开偏好设置中的同名页面。如果某个被包含的市场需要密钥而尚未设置，面板还会显示一个密钥输入框；**保存密钥**会把密钥存入操作系统的凭据存储，并重新读取市场列表。
- 每个无法回应的市场都有一行提示，例如它需要 API 密钥、请求过于频繁（附等待时间）、被暂时跳过或无法连接。
- 结果列表：名称、市场、类型、时长、署名和许可证标记；许可证可以自由使用时标记为绿色。右键单击一条结果，可以复制它的署名、来源链接或下载链接。
- **加载更多**用游标获取每个还有更多结果的市场的下一页，已显示的行不会重复添加。

改变类型、时长、商业使用筛选或市场时，结果会被清空，请再按一次**搜索**。

单击一条结果或它的播放按钮即可试听。面板会停止上一段试听，并通过试听流播放文件，绝不经过走带，最长 30 秒。试听下载最多 48 MiB。下载过的试听文件会在本次会话中保存在应用缓存文件夹的 `sample-previews/` 中：最多 32 个文件、256 MiB，先删除最久未用的文件，应用第一次打开它时会清空它。再次试听同一条结果不需要联网；如果下载地址就是试听过的那个，**加入工程**会直接复用该文件。加入时仍会解析该条目，并核对声明的大小和校验和。

**加入工程**会下载文件、写入附属文件和收据，并在选中的轨道上放置一个 `@audio_clip`。如果没有选中轨道，就新建一条名为 `Samples` 的 `audio/clip` 轨道（名称已被占用时依次为 `Samples 2` 等）。片段从播放头处或其之前最近的拍开始，保持采样的实际长度。如果下载期间工程发生了变化（你编辑了它或打开了另一个工程），片段不会被放置，文件仍留在媒体文件夹中。没有可下载文件的结果会显示**仅试听**。需要提示的许可证会先弹出提示。

命令失败时，面板收到 `{code, message}`，其中 `code` 是拒绝代码（`licence_notice`、`key_required`、`invalid_cursor`……）或 `null`。对于它认识的代码，面板显示自己的说明，否则显示 message。

## 智能体工具 {#agent-tools}

`sample_search` 接受 `query`、`kind`、`min_seconds`、`max_seconds`、`channels`、`tags`、`providers`（数组；省略时使用默认搜索）、`provider`（旧的单市场写法）、`offset`、`limit`、`commercial_ok` 和 `cursor`。它返回上文描述的搜索结果，每条结果都带有 `provider`、`provider_name` 和 `license_ok`。

`sample_pull` 接受 `id`、可选的 `provider`（省略时由 id 前缀推断）、可选的 `dest` 和 `accept_licence`。它默认写入 `<project>/media/samples`，并返回 `path`、可移植的 `media/samples/...` 路径、许可证、署名、SHA-256、`quality`，如果是预览还会附上警告。Freesound 下载在有已保存的 OAuth2 令牌时使用该令牌，返回 `quality: "original"`；没有令牌或原始文件被拒时，返回 `quality: "preview_mp3"`。

这两个工具在 AI 面板中都可以使用：搜索只读取目录，下载只把一个文件写进工程已有的文件夹。被拒绝的调用返回 `{"status": "refused", ...}`。网络或磁盘故障返回错误，因为什么都没有改变，重试也许就能成功。

## 命令行 {#command-line}

```sh
ocp sample providers
ocp sample search "snare drum" --kind drum --limit 5 --provider openverse
ocp sample search "vinyl loop" --min-seconds 1 --max-seconds 4 --channels 2
ocp sample search "pad" --commercial-ok
ocp sample pull "commons:File:Snare drum unmuffled.ogg"
ocp sample pull "ia:prelinger-snare" --provider internet-archive
FREESOUND_API_KEY=... ocp sample search "riser" --provider freesound --json
```

`ocp sample search` 只询问一个市场：`--provider` 指定的那个，否则是 Wikimedia Commons。它用 `--offset` 分页，最后一行会打印下一页的偏移量。每条结果显示 id、类型、时长、声道、采样率、许可证和署名，并在许可证需要 `--accept-licence` 时注明。`--tag` 添加一个标签，可以重复使用。

`ocp sample pull` 下载到 `--dest`，默认是当前文件夹下的 `media/samples`。它打印路径、类型、音质、大小和文件名以及署名，如果是预览还会给出警告。`ocp sample providers` 列出每个市场的可用状态和原因。`--json` 会为脚本打印原始的页面、收据或市场列表。

命令行只从 `FREESOUND_API_KEY` 和 `JAMENDO_CLIENT_ID` 读取密钥。它打印的拒绝说明与工具相同，并以非零状态退出。

## 测试 {#tests}

各市场的解析器用 `crates/oc-sample/tests/fixtures/` 中录制的响应、通过模拟的传输层测试，没有测试会访问网络。

存储测试覆盖了关键的拒绝情况：大小不符、校验和不符以及需要提示的许可证都会让文件夹保持为空；成功的下载会写入音频、附属文件和收据，其中的校验和与文件内容一致。注册表测试覆盖有无密钥时的可用状态、仅限网页的原因和 id 前缀。面板测试会渲染面板、执行搜索、显示市场标签、用**加载更多**翻页，并通过模拟的 `invoke` 加入一条结果。
