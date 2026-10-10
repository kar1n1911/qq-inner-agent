# 开发文档:系统与实现位置

> 本文件是给**开发者**看的系统地图:按系统拆分,标注每个功能实现在哪个文件,以及**该系统的当前现状**。
> 它是"在哪改 / 现在什么状态"的索引,不是设计讨论。
> **架构(输入→决策→输出三层、数据流、模块接线状态)见 [`ARCHITECTURE.md`](ARCHITECTURE.md)**;
> **待办与未来计划见 [`../TODO.md`](../TODO.md)**。
> 设计与阶段的临时工作文档见
> [`docs/working/prompt-and-learning-design.md`](working/prompt-and-learning-design.md) 与 [`docs/working/rust-port/`](working/rust-port/)。

## 系统总览

| 系统 | 职责 | 核心文件 | 现状 |
| --- | --- | --- | --- |
| [人格系统](#1-人格系统) | persona / 身份自治 / 过往情景 / 人类化 | `config.rs` `persona/*` | ⚠️ 身份自治被 traits 卡住;多气泡契约缺字段 |
| [记忆系统](#2-记忆系统) | 三层记忆 / 召回排序 / 学习 / 词元 | `memory/*` `store/learning.rs` | ⚠️ `short_term` 正常;`traits`/`long_term` 近乎空 |
| [情绪系统](#3-情绪系统) | 心情/认同/好感 + 二维 disposition | `persona/affect.rs` | ✅ 在跑;disposition 落到 `Withdrawn`(动机 ×0.2) |
| [记忆召回](#4-记忆召回) | recall 下钻 | `persona/recall.rs` | ✅ |
| [决策系统](#5-决策系统) | cycle / 三层决策 / 策略 / 发送 / 活跃 / 观察 | `engine/*` | ✅;`@` 会被可用性闸门吞掉 |
| [素材系统](#6-素材系统) | 素材语料 + 图片 OCR | `media/*` | ⚪ 语料未启用;✅ OCR 在用 |
| [话题来源](#7-话题来源-21) | 外部内容抓取 + 群间转发 | `topic/*` | ❌ relay 死代码;⚠️ topicSource 无来源 |
| [传输系统](#8-传输系统) | OneBot v11 + LLM provider | `transport/*` | ✅ |
| [存储与配置](#9-存储与配置) | SQLite / 配置协议 / 文件协议 | `store*` `config.rs` `settings.rs` | ✅ |
| [控制与运行时](#10-控制与运行时) | 控制套接字 / 主循环 / 主人教学 | `control.rs` `main.rs` | ✅ |
| [Node 侧](#11-node-侧) | 仪表盘 / CLI / 静态前端 | `src/*.mjs` `web/*` | ✅ |

图例:✅ 正常 · ⚠️ 部分/受阻 · ⚪ 有意关闭 · ❌ 失效(详见 `TODO.md`)。

全部源码位于 `rust/src/`。

---

## 1. 人格系统

agent "是谁、怎么说话"这一整条链。

| 功能 | 实现位置 |
| --- | --- |
| 基座人格 `agent.persona`(种子文本) | `config.rs` — `Agent.persona`(`RuntimeText`) |
| 行为/回复风格/兴趣/变体 `agent.personality` | `config.rs` — `Agent.personality`;`persona/expression.rs` — `personality_context()` 组装成 payload |
| 提示词里的 persona 注入 | `prompts.rs`(生成物,勿手改);`engine/mod.rs` — formation/articulation payload 的 `persona` 字段 |
| **身份自治**(§22:昵称/群名片/头像/签名自动外显 + 人格成长) | `persona/mod.rs` — `enough`/`propose`/`automate`/`grow`/`persona`/`backup`/`restore` |
| **模型自拟昵称**(贴吧式) | `persona/mod.rs` — `NAME_PROMPT`/`member_names`/`model_nickname`;引擎 tick 里调模型生成 |
| **过往情景**(§23:不可变虚构自身过往) | `persona/backstory.rs` — `create`(只 INSERT)/`add_detail`(只追加)/`recall` |
| 人类化运行时片段(只发表情 / 多气泡指令) | `persona/humanize.rs` — `FACE_ONLY_INSTRUCTIONS`/`MULTI_BUBBLE_INSTRUCTIONS` |
| 多气泡 + 打字延迟发送 | `engine/mod.rs` — articulation 后的发送循环(解析 `bubbles`、条间延迟随心情抖动) |

**现状**:身份自治开关全开但**从未尝试**——`enough()` 要求 `traits ≥ minTraits(3)`,而 `traits` 层只有 1 条;
多气泡开关为真但**从未发出过**——`ARTICULATION` 的输出契约里没有 `bubbles` 字段(见 `TODO.md` A2 / B4)。

**本系统用到的对外接口**:`set_group_card` / `set_qq_profile` / `set_qq_avatar` / `set_self_longnick`(见[传输系统](#8-传输系统))。

### 提示词

- **源**:`src/prompts.mjs`(仓库根,JS);**生成物**:`rust/src/prompts.rs`。
- 改动提示词:`src/prompts.mjs` → `node rust/tools/gen-prompts.mjs` → `node rust/tools/gen-prompts.mjs --check`。
- 运行时片段(不进入生成物):`persona/humanize.rs` / `persona/recall.rs` / `persona/backstory.rs` 的常量,由 engine 追加到 payload。

---

## 2. 记忆系统

| 功能 | 实现位置 |
| --- | --- |
| 三层记忆(short/long/notebook + traits) | `memory/mod.rs` — `LayeredMemory` 等;表结构在 `store.rs` 的建表 |
| 召回排序(词汇重叠 + 中文双字 + 时效) | `memory/ranking.rs` — 稀疏召回,调用方先限 chat/subject |
| 学习事务(落库前 self-review) | `store/learning.rs`;引擎调用在 `engine/mod.rs` |
| 结构判断(沉默不转学习信号) | `persona/conversation.rs` |
| 词元化 / 相似度 | `memory/text.rs` — `terms()`/`similarity()` |
| 中文检索(unicode 归一) | `memory/memory_unicode.rs` |

**现状**:`short_term` 660+ 条(程序记原话,正常);**`traits` 与 `long_term` 近乎空** —— 模型给出的层大多 `verdict=skip`,
且 `LEARNED_STYLE` 要求"只能用 chatStyle/memories 中确有依据的说法",于是形成死锁(见 `TODO.md` A2)。

---

## 3. 情绪系统

| 功能 | 实现位置 |
| --- | --- |
| 三指标(mood 消息级 / agreement 消息级 / affinity 人物级) | `persona/affect.rs` — `rate`/`read`/`update` |
| 衰减(affinity 7 天、其余 4h) | `persona/affect.rs` — `decay()` |
| 二维 disposition(四象限) | `persona/affect.rs` — `disposition(valence, rationality)` → Angry/Withdrawn/Scrutinizing/Supportive |
| 单向约束 + 有界步长 + 负性非对称 | `persona/affect.rs` — `update()` |
| Angry 熔断(无回应连发 ≤3) | `persona/affect.rs` — `burst_allowed`/`reserve_burst` |
| 接入发送概率(motivation 因子) | `engine/sending.rs` — `sending_probability_with_affect` |

**现状**:在跑(`affect_state` 12 条);但实测 `disposition = Withdrawn` → `motivation() = 0.2`,把发送概率**再乘 0.2**,
是有效概率偏低的主因之一(见 `TODO.md` B3)。

---

## 4. 记忆召回

| 功能 | 实现位置 |
| --- | --- |
| recall 下钻(两级召回,预算含 id/时间戳) | `persona/recall.rs` — `RULE`/`CONTRACT`/`Budget`;`engine/mod.rs` — formation/articulation system 里追加 `RULE` |

**现状**:✅ 启用且判定日志里有 `memory_recall` 事件。
注意 `RULE` 当前被人工放宽过(允许"用记忆大意补全细节"),未提交。

---

## 5. 决策系统

| 功能 | 实现位置 |
| --- | --- |
| 引擎 cycle(候选→评估→表达→预测→发送) | `engine/mod.rs` — `cycle()`/`media_cycle()`/`tick()`/`ingest()` |
| 三层决策(①②拆分 + 零模型初筛) | `engine/decision.rs`;门控 `agent.threeLayerDecision` |
| 逐消息策略(allowed/normalize/quiet/select/repeated/长度档位) | `engine/policy.rs` |
| 回填(断连后补录历史) | `engine/backfill.rs`;门控 `agent.backfill` |
| 积压简读(头+尾+采样) | `engine/backlog.rs`;门控 `agent.observation.backlogDigest` |
| 发送概率(因子乘积 + 预测 veto) | `engine/sending.rs` — `sending_probability`/`sending_probability_with_affect` |
| 活跃概率(高斯曲线) | `engine/activity.rs` — `activity_probability` |
| 入群观察闸门 | `engine/orientation.rs`(纯逻辑);持久化在 `store/orientation.rs` |
| forward 段收发(接收侧拉取合并聊天记录 + 标注"外部信息") | `engine/policy.rs` — `resolve_forwards`/`resolve_forwards_backfill` |

**现状**:✅ 主链路正常;三处待修——`@` 被 `available()` 吞掉、发送概率偏低、长度档位偏短(见 `TODO.md` B1/B2/A3)。

**本系统用到的对外接口**:`get_group_info` / `_get_group_notice` / `get_group_msg_history` / `get_forward_msg` / `get_group_member_list`(见[传输系统](#8-传输系统))。

---

## 6. 素材系统

| 功能 | 实现位置 |
| --- | --- |
| 入站表情包采集(哈希去重、即时落盘) | `media/mod.rs` — `Collector::ingest` |
| 素材选择(群温度 + 场合适配度) | `media/media_select.rs` |
| 来源可公开性(本地处理,不上传/不反向图搜) | `media/media_source.rs` |
| **图片 OCR**(可替换引擎 + 置信度) | `media/ocr.rs` — `Engine` trait / `Tesseract` / `RapidOcr` / `Worker`;桥接脚本 `scripts/ocr-rapidocr.py` |

**现状**:素材语料(`media_*` 表)全为 0 —— 未启用;**OCR 在用**(`media_ocr` 14 条,引擎 `rapidocr`),
低可信时上下文会标"文字识别可信度低"。

**本系统用到的对外接口**:取图走 OneBot 的图片 URL(见[传输系统](#8-传输系统))。

---

## 7. 话题来源(§21)

| 功能 | 实现位置 |
| --- | --- |
| 外部新鲜内容(GitHub/RSS 抓取 + 相关度 + 节流/预算/缓存) | `topic/mod.rs` — `fetch`/`parse`/`relevance`/`collect`;配置 `agent.topicSource` |
| 群间转发(低风险转手 / 高风险门控 + 去重 + 责任线/自审) | `topic/relay.rs` — `classify`/`is_duplicate`/`safety_gate`/`decide`;配置 `agent.relay` |

**现状**:
- `topicSource.enabled = true` 但 `github` / `feeds` **均为空**,`Settings::enabled()` 要求来源非空 → **实际不抓取**;
- **`relay` 是死代码** —— 上述四个决策函数在**全仓 0 处引用**,文件头自述"纯决策模块,**调用方**负责抓取/审核/发送",
  而那个调用方**从未实现**;`agent.relay.enabled = true` 等于空转。
- ⚠️ 本文件与设计文档此前把 §21 标为「✅ 已实现」,**与代码不符**,已按实际状态改写(见 `TODO.md` E1/E2)。

**本系统用到的对外接口**:无(抓取走 HTTP,未接入)。

---

## 8. 传输系统

| 功能 | 实现位置 |
| --- | --- |
| OneBot v11 正向 WS(鉴权/心跳/重连/early 缓冲/echo 关联) | `transport/mod.rs` — `OneBot`/`call`/`send`/`send_media`/`send_forward` |
| LLM provider(两种 API 格式 + 重试退避 + 预算) | `transport/provider.rs`(纯逻辑 + HTTP);`transport/provider_transport.rs`(阻塞传输在 blocking 池) |

**现状**:✅ 正常。`onebot.forwardEnabled` 已开,合并转发可解析。

### 8.1 OneBot 接口(内核 ↔ NapCat)

- **传输**:WebSocket `ws://127.0.0.1:3001/`(正向 WS,NapCat 作服务端);`config.onebot.url/selfId/token`。
- **内核现在用它做什么**:

| action | 现在做什么 | 调用方 |
| --- | --- | --- |
| `get_login_info` | 改头像/昵称前先读回自己的 QQ 号做校验,避免改错账号 | `persona/mod.rs`(§22) |
| `get_status` | 探活:判断桥是否在线、QQ 是否登录(重连与状态上报) | `main.rs`、`transport/mod.rs::check` |
| `get_friend_list` | 拉好友列表 → 仪表盘"可选聊天"里的私聊项 | `control.rs`(`contacts.list`) |
| `get_group_list` | 拉群列表 → 仪表盘"可选聊天"里的群项 | `control.rs`(`contacts.list`) |
| `get_group_info` | 取群名称 / 人数 / 简介 → 观察期的"群资料" | `engine/orientation.rs` |
| `_get_group_notice` | 取群公告 → 观察期的"群公告" | `engine/orientation.rs` |
| `get_group_msg_history` | 取群历史:①观察期分析素材 ②连接后回填漏掉的消息 | `engine/orientation.rs`、`main.rs`(`backfill`) |
| `get_group_member_list` | 取成员列表:①给模型看过往昵称样本以自拟新昵称 ②成员去重避免重名 | `engine/mod.rs`、`persona/mod.rs` |
| `get_group_member_info` | 改群名片前读回自己当前名片,用于备份与回退 | `persona/mod.rs`(§22) |
| `get_stranger_info` | 同上,改头像/昵称前读回自身信息 | `persona/mod.rs`(§22) |
| `get_forward_msg` | 展开一条合并转发,取出各节点文本 → 标注为"外部信息"并入上下文 | `engine/policy.rs`(`resolve_forwards`) |
| `send_group_msg` | 往群里发一条消息(多气泡时逐条发) | `transport/mod.rs::send` |
| `send_private_msg` | 往私聊发一条消息 | `transport/mod.rs::send` |
| `set_group_card` | 写群名片(身份自治外显) | `persona/mod.rs`(§22) |
| `set_qq_profile` | 写昵称等资料(身份自治外显) | `persona/mod.rs`(§22) |
| `set_qq_avatar` | 换头像(身份自治外显) | `persona/mod.rs`(§22) |
| `set_self_longnick` | 写个性签名(身份自治外显) | `persona/mod.rs`(§22) |

> §22 身份自治的四个 `set_*` 目前**不会被触发** —— 前置条件 `enough()` 需要 `traits ≥ 3`,现在只有 1 条(见 `TODO.md` E3)。

### 8.2 模型 API(内核 ↔ LLM)

- **OpenAI 兼容**(含 DeepSeek):`POST {baseUrl}/chat/completions`,请求体 `{model, max_completion_tokens, messages:[{system},{user}]}`。
- **Anthropic**:`POST {baseUrl}/messages`,请求体 `{model, max_tokens, system, messages}`。
- **模型列表**:`GET {baseUrl}/models` —— 现在只被仪表盘的"选模型"用(`control.rs` 的 `models.list`)。
- **现在用它做什么**:唯一入口 `provider_transport.rs::json(system, payload)` —— 把 payload 序列化成 user 内容,
  要求模型**只输出一个 JSON 对象**;校验失败即拒绝、不修补。六个任务共用这条路:
  `ORIENTATION`(入群定风格)、`FORMATION`(出候选)、`EVALUATION`(评分)、`ARTICULATION`(成文)、`FORECAST`(发不发)、`LEARNING_REVIEW`(复核学习项)。

---

## 9. 存储与配置

| 功能 | 实现位置 |
| --- | --- |
| SQLite 主接口(建表/查询/保留期清理/预算) | `store.rs` — `Store` |
| 观察期与活动块持久化(epoch 防旧覆盖) | `store/orientation.rs` |
| 其余操作(学习事务等) | `store/operations.rs` `store/learning.rs` |
| 配置协议(defaults/merge/validate/loadConfig/readiness) | `config.rs` |
| 文件协议(revision 哈希 / atomicJson / 热重载) | `settings.rs` |

**现状**:✅。配置键白名单由 `config-defaults` 子命令导出,仪表盘动态同步。

---

## 10. 控制与运行时

| 功能 | 实现位置 |
| --- | --- |
| 控制套接字服务端 | `control.rs` |
| 主循环(1s tick / status / revision 监视 / 信号) | `main.rs` |
| 完整配置 schema(`config-defaults` 子命令) | `main.rs` — `Command::ConfigDefaults` |
| 主人私聊教学(/记住 /黑话 /忘记 /还原) | `persona/owner_teaching.rs` |
| 模块注册 | `lib.rs` |

**现状**:✅。三个 systemd 服务(agent / dashboard / napcat);NapCat 已改为**在 agent 之后启动**。

### 10.1 控制套接字接口(内核 ↔ 仪表盘/CLI)

- **传输**:Unix socket `data/control.sock`,**NDJSON**(一行一个 JSON 对象,上限 1 MiB/行)。
- **请求**:`{"id":"<uuid>","method":"<name>","params":{…}}` → 一行 JSON 响应。
- **内核现在用它做什么**:

| 方法 | 现在做什么 | 对端端点 |
| --- | --- | --- |
| `state.get` | 读 `status.json` 并补上学习计数(记忆条数 / 表达条数),给仪表盘总览 | `GET /api/state` |
| `contacts.list` | 合并好友列表与群列表,供仪表盘勾选"允许的聊天" | `GET /api/contacts` |
| `models.list` | 拿配置里的 apiKey 向 provider 请求 `/models`,给仪表盘选模型 | `POST /api/models` |
| `learning.list` | 查 `memory_layers`(long_term → traits → short_term 排序)+ `expressions`,附最近 10 版修订 | `GET /api/state` |
| `learning.reset` | 按 chat(可再限定 subject)重置:清 style、删 learned_memories、重建三层与表达 | `POST /api/learning/reset` |
| `logs.tail` | 返回日志尾部若干行,给仪表盘日志面板 | `GET /api/state` |
| `debug.send` | 向**自己账号的私聊**发一条带 UUID 的诊断消息(不让调用方选收件人/内容) | `POST /api/debug/send` |
| `debug.receive.start` | 打开一个有界的事件捕获窗口(要求 QQ 在线) | `POST /api/debug/receive` |
| `debug.receive.status` | 查询该窗口是否还在监听 + 已捕获事件 | `GET /api/debug/receive` |
| `debug.receive.stop` | 关闭捕获窗口 | `POST /api/debug/stop` |
| `test.model` | 用当前配置试跑一次模型,返回是否可用 | `POST /api/test-model` |

- **推送事件**:`onebot`(连接状态)、`decision`(判定与原因)、`send_assessment`(概率与 veto)、`message_sent`(发出)、`chat_learning_updated`(学习落库)、`config_applied`(热重载生效)、`cycle_error`(循环异常)。
- **客户端**:`src/control.mjs`(`net.createConnection`,带重连/超时)。

---

## 11. Node 侧

Node 只承载仪表盘与 CLI(旧 agent 实现已归档到 `js-legacy` 分支):

| 文件 | 职责 |
| --- | --- |
| `src/dashboard.mjs` | Web 控制台 + HTTP API(读控制套接字 + 双读回退) |
| `src/cli.mjs` | 命令行(`core-status` 等) |
| `src/control.mjs` | 控制套接字客户端 |
| `src/config.mjs` `src/settings.mjs` `src/provider.mjs` `src/onebot.mjs` `src/store.mjs` | 运维依赖。`settings.mjs` 的键白名单**动态取自 Rust 的 `config-defaults`**,加新键无需手改 JS |
| `src/prompts.mjs` | 提示词源(生成 `rust/src/prompts.rs`) |

**现状**:✅。`web/index.html` 含四个页面(Overview / Configuration / Activity / **Advanced**);
Advanced 页放全部功能开关与细粒度参数。已知排版问题见 `TODO.md` D2。

### 11.1 仪表盘 HTTP 接口(浏览器 ↔ Node)

- **端口**:HTTP `127.0.0.1:5097`(本地)· HTTPS `0.0.0.0:5098`(局域网/VPN)。
- **鉴权**:访问密钥(`data/dashboard-access.txt`,用 `./agent dashboard-key` 取)+ 会话 CSRF;响应里会 redact 密钥与 token。
- **端点**(Node 现在用它做什么):

| 端点 | 方法 | 现在做什么 |
| --- | --- | --- |
| `/` `/app.js` `/i18n.mjs` `/style.css` `/language.css` `/favicon.svg` | GET | 发前端静态资源(含中英 i18n) |
| `/api/login` | POST | 校验访问密钥,建会话并下发 CSRF |
| `/api/session` | GET | 返回当前会话的 CSRF;同时是"是否已登录"的探针 |
| `/api/logout` | POST | 销毁会话 |
| `/api/config` | GET | 读配置(经 `publicSettings`,按内核键白名单裁剪,永不外泄密钥) |
| `/api/config` | PUT | 写配置(键白名单校验 + revision 冲突检测),内核热重载 |
| `/api/state` | GET | 状态快照:控制套接字可用时走它,否则回退到直读 `status.json` + SQLite |
| `/api/models` | POST | 用给定 apiKey 探 provider 的模型列表 |
| `/api/test-model` | POST | 校验配置后试跑一次模型,返回可用性 |
| `/api/learning/reset` | POST | 重置某个聊天的学习(或指定 subject) |
| `/api/contacts` | GET | 取可选聊天(优先控制套接字,回退到直连 OneBot) |
| `/api/service` | POST | 对 agent/dashboard 服务做 start/stop/restart 并回状态 |
| `/api/debug/send` | POST | 触发一次自账号诊断发送 |
| `/api/debug/receive` | GET/POST | 查询 / 打开事件捕获窗口 |
| `/api/debug/stop` | POST | 关闭事件捕获 |

> 所有响应都会把 apiKey / onebotToken / 访问密钥替换成 `[redacted]`。

---

## 测试在哪

**改哪个模块就跑哪个模块的测试；只在合并前跑一次全量。** 测试验证当前模块的行为、不变量与边界，不再保存迁移期 JS oracle 的逐值快照。

### 编写顺序：先按目标写测试方案，再动手实现

任何功能**在动手实现之前**，先写出测试方案，并且必须分两层：

| 层 | 回答的问题 | 写什么 |
| --- | --- | --- |
| **可用性测试** | 它**能不能用** | 输入/输出正确、边界与失败路径，以及**接入程序总线**（输入层 → 决策层 → 输出层的接线真的通，而不是模块自测通过、线上却没接上） |
| **目标测试** | 它**有没有达到目的** | 直接断言设计文档里那句「**为什么**」在**真实链路**上成立 —— 验证这个功能**存在的理由**，而不是只验证零件能转 |

**判定：只通过可用性测试不算完成；两层都过才算完成。**

目标测试必须**注明设计依据**（哪份文档、哪一行），这样设计变更时测试会跟着变红，而不是悄悄漂移。
2026-10 的全流程审计共发现 39 条「设计 vs 实现」偏差（`docs/working/audit-design-vs-implementation.md`），
**全部属于“可用性测试过、目标测试缺席”的情形** —— 例如“精确回复”的段构造有测试，但“被 @ 时真的会引用”没有；
“观察期”的字段有测试，但“观察完成后用时不再增长”没有。这类缺口正是本流程要堵住的。

- Rust 集成套件（`rust/tests/`）：`affect`、`engine_parity`、`onebot_mock`、`media_select`、`media_collect`、`identity`、`backstory`、`owner_teaching`、`humanize`、`learning_triage`、`backfill_policy`、`backlog_digest`、`recall_budget`。例如修改引擎时运行 `cd rust && cargo test --test engine_parity`。`engine_parity` 保留名称，但只验证行为，包括公共成员元数据及精确回复。
- Rust 内联单测在各 `rust/src/**` 模块中。例如修改配置文件协议时运行 `cd rust && cargo test --lib settings::tests`；revision 验证缺失/空文件等价、任一文件编辑可见、恢复原文恢复 revision，以及读取错误传播，不冻结哈希常量。
- 情绪测试合并为 `affect.rs`，按象限、步长、衰减等输入表验证行为。媒体采集与选择分别覆盖持久化/事务和选择/学习链路，保留两个套件。
- Node 套件（`test/*.test.mjs`）：`control`、`dashboard`、`diagnostics`、`prompts`、`redirect`、`reload`、`settings-patch`。例如运行 `node --test test/settings-patch.test.mjs`。
- 提示词选择**生成一致性检查**：修改 `src/prompts.mjs` 或 `src/orientation.mjs` 后运行 `node rust/tools/gen-prompts.mjs`，再运行 `node rust/tools/gen-prompts.mjs --check`。检查在内存中生成并经 rustfmt 格式化，与 `rust/src/prompts.rs` 逐字比较，不修改工作区，也不依赖 golden；Node 的 `prompts` 套件自动执行此检查（需要 Node 与 rustfmt）。
- `rust/tests/golden/` 及其辅助全部删除。旧 JS 整库 schema/数据打开兼容性覆盖随 oracle 删除：旧实现已归档，不再维护全 schema/逐行等价；当前 Rust 存储单测与媒体旧 schema 迁移行为测试仍保留。

本次精简前后（按测试函数计数，表驱动中的输入行不单独计数）：

| 项目 | 精简前 | 精简后 |
| --- | ---: | ---: |
| Rust 测试 | 305 | 231 |
| Rust 集成套件 | 26 | 13 |
| Node 测试 | 34 | 34 |
| golden 目录逻辑字节数（含辅助） | 5,256,568 | 0 |

Rust 数量包含补回一个原本缺少 `#[tokio::test]` 的精确回复用例；情绪九项行为均保留。Node 用生成一致性检查替换旧 JSON 示例快照，所以总数不变。

合并前在仓库根目录执行一次：

```sh
cargo clippy --manifest-path rust/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path rust/Cargo.toml
QQ_CORE_BIN="$PWD/rust/target/debug/qq-inner-core" npm test
```

Node 热重载测试需要已构建的内核；`QQ_CORE_BIN` 可指向其他构建目录。macOS 默认缺少第二个 loopback 地址，TLS 多来源限流测试会按平台跳过。

**磁盘提示**：Rust 集成测试现有 13 个二进制；`Cargo.toml` 中 `[profile.dev] debug = "line-tables-only"` 限制调试产物体积。在服务器验收后可用 `cargo clean --manifest-path rust/Cargo.toml --profile dev` 回收开发构建空间，保留 release。

## 二进制自更新（人工管理入口）

**设计依据 UPDATE-1（目标）**：把手工部署内置化；调用更新入口后，磁盘可执行文件必须等于新构建产物，旧进程退出后由监督者在同一路径启动新版；调用回滚入口后，磁盘和新启动进程都必须恢复上一版。不能仅以“构建成功”或“发送了重启信号”判定完成。

**设计依据 UPDATE-2（发送安全）**：重启必须先停止接收新任务、等待引擎在途发送完成并记录结果，再断开 OneBot、关闭 SQLite、自行退出。发送前已持久化的 pending 投递在异常退出后只转为 uncertain，绝不重放。

实现：`rust/src/update.rs`（更新事务、持久状态、控制分发），`rust/build.rs`（编译时 git revision），`rust/src/main.rs`（CLI、控制总线接线、排空退出）。不定时拉取，不调用 systemctl，不修改 napcat 依赖关系。

### 操作步骤

1. 首次部署本功能仍需正常手工构建并重启一次。确保服务使用本仓库的可执行文件，`Restart=always`（`scripts/install_service.py` 已配置），服务用户有仓库、二进制目录写权限，PATH 能找到 Git/Cargo，Git 的 upstream 和非交互认证已设置好。
2. 在开发机提交、推送。服务端执行 `./agent update apply`；原生 CLI 等价为 `rust/target/release/qq-inner-core --root "$PWD" update apply`。CLI 只发送一次请求，不自动重试。响应 `accepted: true` 表示任务已接收，**不表示更新成功**。
3. 执行 `./agent update status` 查看结果；`runningRevision` 是当前进程编译时写入的 revision，`installedRevision` 是已安装版本，`previousRevision` / `previousBinary` 是备份，另有 `operation`、`result`、`error`、时间戳、`busy`、`pid`。构建详情在 `.runtime/update/build.log`，状态保存在 `.runtime/update/status.json`。
4. 成功后出现 `restart_pending`，进程排空并退出。`flock` 等待的内核退出后，包装进程也退出、释放锁；systemd 根据 `Restart=always` 重启相同路径。启动后的 `update.status` 为 `completed`，`runningRevision` 应是目标 revision。直接手动运行 `start` 时没有监督者，不会自行再启动。
5. 回滚执行 `./agent update rollback`，再查询 `./agent update status`。回滚不拉取、不构建、不回退 Git 工作树或数据库；用备份执行相同的原子安装和排空退出流程，并把换下来的版本保留为新的上一版。

本地控制套接字仍是配置的数据目录下 `control.sock`（目录权限 0700）。管理方法为 `update.apply`、`update.status`、`update.rollback`，使用现有逐行 JSON 协议，例如 `{"id":"admin-1","method":"update.apply","params":{}}`。同一进程同时只允许一个变更，直到重启完成；重复变更返回 `update_busy`，状态查询不阻塞。可由有该用户套接字权限的 CLI/仪表盘调用，本次提供 CLI，不新增公开 HTTP 接口。

### 前置检查、失败与恢复

- `git status --porcelain --untracked-files=all` 必须为空（包括未跟踪文件）；然后 `git pull --ff-only`。脏工作树、无 upstream、认证失败或分叉都报告 `failed` 和错误，保留原二进制，不重启。
- pull 前后 HEAD 相同，返回 `already_latest`（已是最新），**不构建**。这里表示源码没有新提交；回滚后源码与运行版本可能不同，须对照 `runningRevision`。构建失败后已经拉下来的 HEAD 不自动回退；再次 apply 若没有新提交仍不重建，需要推送新的修复提交，或由管理员处理临时工具链故障后手工构建部署。
- 构建实际执行 `cargo build --release --bin qq-inner-core --target-dir <隔离目录>`，从不向运行中的 `rust/target/release` 构建；失败清除隔离构建目录，保留旧文件和备份，保留构建日志。构建后再次校验工作区与 HEAD，拒绝源码在构建期间发生的变化。
- 安装前执行候选产物的 `build-revision`，必须匹配目标 revision；校验当前磁盘版本仍匹配运行版本。候选复制到目标二进制的同目录临时文件，fsync 后，通过 rename 原子覆盖；运行中进程继续持有旧 inode。备份在 `.runtime/update/previous-*`，在替换前持久化。旧备份留存供人工审计；status 指向可直接回滚的上一版。
- 无备份、备份丢失、版本不匹配、文件权限不足均报告错误。替换前失败不改变在线文件；替换后若落盘状态或清理失败，记录错误并仍请求排空重启，避免磁盘已换新版而继续接受第二次安装。进程意外终止时下次启动会把未完成的事务标记为 `interrupted`；若实际运行 revision 已等于目标，则认定 `completed`。
- 重启期间套接字短暂不可用；非常快的回滚可能在确认响应抵达客户端前开始退出。此时应重连查询 status，**不要自动重放更新/回滚请求**。本功能没有启动健康检查后的自动回滚；若新程序无法启动，管理员可停止服务后根据持久化 `previousBinary` 复制到可执行文件同目录临时路径，再 rename 覆盖并启动服务。
- 更新自身不依赖 systemd 的 stop 超时：服务主动等待发送排空后才退出。发送失败/连接断开会按既有逻辑记录 uncertain，启动恢复不重发；部署不提供 OneBot 无法保证的网络层 exactly-once 承诺。

### 两层测试及设计映射

- **可用性**：`rust/src/update/tests.rs` 用临时 Git 仓库作为假远端、脚本作为 Cargo；覆盖无新提交不构建、脏工作区、非快进、构建失败清理、产物版本不匹配、缺失备份、并发互斥、状态与备份、原子替换和控制套接字路由。`rust/src/main.rs` 的运行时测试另外验证生产 `run()` 确实接入管理方法，并验证退出排空。
- **目标**：`control_entry_restarts_new_artifact_and_rollback_restarts_old_artifact` 对应 UPDATE-1：通过真实 `Server → Admin → Updater` 套接字调用升级，逐字节比较磁盘文件与假构建预期产物，等待旧子进程正常退出，再由测试监督者从相同 ExecStart 路径启动；断言新 PID、运行 revision、备份内容；再调用回滚，断言磁盘内容和启动进程都恢复旧版。两个可执行脚本作为可区分的假产物，复用已编译的测试服务程序；测试不拉 GitHub、不嵌套运行 Cargo、不操作真实 systemd。
- **发送安全**：运行时排空测试对应 UPDATE-2；现有引擎与 OneBot 测试验证发送结果落库、uncertain 和无自动重放。测试代码标注上述设计行号，按本文件“两层都过才算完成”的规则验收。
