# I/O 内容审计：模型接入 · OneBot 输入输出 · 数据库输入输出

**日期**：2026-10-10
**范围**：所有跨进程/跨信任边界的 I/O —— ①模型（LLM provider）接入；②OneBot 输入；③OneBot 输出；④数据库输入输出；⑤其他（控制套接字、外部 HTTP、媒体/OCR、文件系统）。
**方法**：先把设计文档里的 I/O 要求抽成可判定的清单（oracle），再逐条回到代码核验；只报有证据的偏差，禁止凑数。
**证据**：每条给出「设计原文 + 行号」「代码 `文件:行`」「可观测后果」「置信度」。凡 high 结论均已回到代码逐行复核。

> **与既有审计的关系**：`docs/working/audit-design-vs-implementation.md`（2026-10-10，39 条）已覆盖部分条目。
> 本文**不重复其结论**，只在两处补上更硬的证据链：其一，找出它未覆盖的 I/O 面（主循环阻塞、事务跨网络、出站落库与实发不一致、表无清理）；
> 其二，为它已报的条目补上**新的设计依据**（`docs/working/human-like-replies.md` 的多气泡契约、`rust/MEDIA.md` 的持锁红线），
> 使这些条目从「看起来不对」升级为「违反明文契约」。凡属重复，均在表中标注「已见前审计」。

---

## 一、I/O 全景清单

| 面 | 方向 | 入口 | 硬约束（设计） |
| --- | --- | --- | --- |
| 模型 | 出站 | `transport/provider_transport.rs::request_inner` | 滚动 1h 预算、`floor(retries)+1` 次尝试、退避 1/2/4/8/16/30s、响应 ≤1,000,000 UTF-16、禁跳转/禁环境代理 |
| 模型 | 入站 | `transport/provider.rs::extract_text` / `parse_object` | 只取正文；截断/空正文/非法 JSON 各有独立错误码且不重试 |
| OneBot | 入站 | `transport/mod.rs::receive` → `main.rs::Runtime::notice` | 单 mpsc 保序、early ≤200；`post_type=message` 才准入 |
| OneBot | 入站（展开） | `engine/policy.rs::resolve_forwards` → `get_forward_msg` | 只读已有消息；标注「外部信息」 |
| OneBot | 出站 | `transport/mod.rs::send_targeted` / `send_media` / `send_forward` | 段由代码构造、`auto_escape`、chat/face 严格校验 |
| 数据库 | 写 | `store/operations.rs`、各模块 `CREATE TABLE` | `BEGIN IMMEDIATE`、JSON 列编码、触发器清理 |
| 数据库 | 读 | `history` / `counts` / `retrieval` / `group_hours` | 保留期 30 天、每 chat 条数上限、`group_hours` 不受裁剪影响 |
| 其他 | 出站 | `topic/mod.rs`（RSS/GitHub）、`media/mod.rs::read_image` | 请求 10s / 响应 ≤256 KiB；素材单文件 2 MiB、超时 15s、总量 100 MiB |
| 其他 | 双向 | `control.rs`（Unix socket NDJSON） | 单行 ≤1 MiB、响应 redact 密钥 |

**偏差计数**：high 6 · medium-high 4 · medium 5 · low/观察 5（另有「无文档依据但可疑」4 条、已确认一致 20 项、对既有结论的更正 2 条、被推翻的并行审计结论 2 条）。

---

## 二、按根因归类

### 根因 ① 出站校验只作用于一个字段，真正发出去的是另一个字段

这是本轮最集中的一处：`ARTICULATE` 的**内容闸门**全部读 `response["text"]`，而**实际发送**在多气泡开启时用的是 `response["bubbles"]`。

#### IO-01 `bubbles` 绕过 `maxOutputChars` 与全部内容闸门 ✅ high

- **设计**：`docs/working/prompt-and-learning-design.md:537` ——「传输层存在硬上限：`maxOutputChars` 与 OneBot 的字段长度限制，超了根本发不出去」；
  `docs/working/human-like-replies.md:175` ——「任一片段仍受 `maxOutputChars` 与总长度约束」。
- **代码**：
  - 闸门只作用于 `raw = response["text"]`：`engine/mod.rs:1903`、`1907-1917`（`content_allowed`、`<think>` 标签拒绝、空正文判定）；
  - 截断只发生在 `decorate(&response, …, max_output_chars)`，而它只读 `response["text"]`：`persona/expression.rs:253-257`；
  - 真正发送的序列是 `response["bubbles"]`，未经任何长度或内容处理：`engine/mod.rs:1999-2013`、`2052-2055`。
- **后果**：模型把超长或违规内容放进 `bubbles`、`text` 填一句无关的合规话，即可完整绕过 `maxOutputChars`、情绪内容闸门与 `<think>` 泄漏检查，直接发给群。
  `bubbles` 只在 `a.multi_bubble=true` 时被读取，但该开关是远端可配置项。
- **备注**：`audit-design-vs-implementation.md` 的 C2 已报「绕过 `maxOutputChars`」，本文补上它与内容闸门同源的证据。

#### IO-02 落库的自身消息与真正发出的内容不一致 ✅ medium-high

- **设计**：`docs/ARCHITECTURE.md:114` ——「发送后落库 `deliveries(message_id)` + `messages(self=1)`」（落库对象是**发出的那条**）。
- **代码**：`content = text(&decorated,"text")`（`engine/mod.rs:1996`）→ 发送 `seq = bubbles`（`2009-2013`）→ 但落库的是 `content`（`engine/mod.rs:2106`）。
- **后果**：多气泡时 `messages` 只留一行、且正文是**未被拆分的原始 text**，与群里实际看到的 `bubbles` 不符。
  下一轮的历史/`own` 重复检测（`engine/mod.rs:1935-1940`）读的是这段错正文，模型会以为自己说过别的话。

#### IO-03 气泡失败不中断后续发送 ✅ high

- **设计**：`docs/working/human-like-replies.md:178` ——「**任一片段失败即停止后续发送**，并沿用现有『不确定则不重发』的策略」。
- **代码**：循环体 `sent = Some(result);` 之后**不 break**，只有 `available()` 为假才 break：`engine/mod.rs:2017-2066`（`2065` 为赋值，`2032-2040` 为唯一的 break 分支）。
- **后果**：首条失败后仍会继续发第 2、3 条；且 `sent` 只保留**最后一条**结果，首条失败被末条成功掩盖，整轮记为 `sent`（`2070-2120`）。这与「已见前审计 C1」同源，本文确认其在当前 `main` 仍然成立。

#### IO-04 气泡无条数上限、无档位门控、无场景门控 ✅ high（已见前审计 C3，补充依据）

- **设计**：`docs/working/human-like-replies.md:172` 「1–3 条」；`:176` 「只有 `short`/`medium` 档才允许拆分」；`:179` 「拆分只在低风险场景启用（求助/技术回答保持单条）」。
- **代码**：`engine/mod.rs:1999-2008` 只判断 `a.multi_bubble`，对条数、`length_target`、`hint`、是否为求助**全无门控**。
  讽刺的是同一段代码在 `1826-1827` 为 face-only 正确地用上了 `length_target != "long"`，说明档位变量当时就在手边。
- **后果**：`long` 档与求助回答也会被切碎，且条数由模型决定（可远超 3 条），直接违背「便于阅读」的设计意图。

#### IO-05 首条消息没有打字延迟，且延迟公式与文档不符 ✅ high（已见前审计 C4，补充数值依据）

- **设计**：`docs/working/human-like-replies.md:185` —— 发送前 `min(2.5s, 0.4s + 字符数 × 0.02s)` + 抖动；`:186` —— 气泡之间**再**加 0.3–0.8s。
- **代码**：`engine/mod.rs:2018-2029`，延迟写在 `if i > 0` 内 → **首条（也是绝大多数回复的唯一一条）零延迟**；
  且数值是 `700ms + 字数 × 15ms + 抖动(≤300×(1+|mood|))`，既非「发送前」公式，也没有 `min(2.5s, …)` 上限。
- **后果**：「改动 8」对主力路径完全没实现；抖动还与文档的固定 0.3–0.8s 语义不同（随心情无上限放大）。

#### IO-06 气泡之间不重查静默时段 ✅ medium-high（已见前审计 C5，补充依据）

- **设计**：`docs/working/human-like-replies.md:188` ——「延迟必须计入现有的活跃窗口/静默时段判定，且**静默时段开始时不得把已排队的消息发出**」。
- **代码**：`engine/mod.rs:2032` 只重查 `self.available(self.now())`（作息 × 节律）；`policy::quiet(...)` 只在 `1941`（发送前）判过一次。
- **后果**：静默时段在气泡间隔中开始后，已排队的第 2、3 条仍会发出。

### 根因 ② 入站路径上的网络 I/O 无预算约束，且阻塞主循环

#### IO-07 `get_forward_msg` 展开次数无上限，且在主事件循环上串行 await ✅ medium-high

- **设计**：`rust/ONEBOT.md:12` ——「上层应持续消费队列」；`:13` ——「无界队列不会给 WebSocket 读循环施加背压，**但慢消费者会增加内存占用**」。
  `rust/ENGINE.md:15` ——「core 锁串行化 chat 状态和每段同步决策；锁顺序为 core → store，**网络 await 不持锁**」。
- **代码**：
  - 展开循环对**每个不同的 `forwardId`** 各发一次 `get_forward_msg`，只对图片数量设限，对**次数无上限**：`engine/policy.rs:514-555`（`remaining_images` 只限图片）；
  - 该展开在 `Runtime::notice` 里被 `.await`，而 `notice` 本身是主循环 `select!` 的一个分支体：`main.rs:336-347` → `main.rs:595`。
- **后果**：单条消息里若出现 N 个不同 `forwardId`，就是 N 次串行请求，每次最多 `onebot.requestTimeoutSeconds`（默认 12s）→ 主循环最长阻塞 `12N` 秒。
  期间 1s 的 `tick`、5s 的 `report`、配置 watcher 全部不被 poll（`MissedTickBehavior::Skip`），无界的 `notices` 队列持续堆积。
- **定级理由**：常规 QQ 客户端一条消息只带一个 `forward` 段，所以现实触发面主要是「桥变慢」而非「一条消息打满」；
  但**次数无上限**这一条没有任何设计依据，属明确的缺口。`ONEBOT.md:13` 承认了慢消费者会增加内存，却没有要求调用方不得在消费路径上做网络 I/O；
  故本条按 medium-high 记，而非 high。

#### IO-08 媒体下载发生在 `store` 的 IMMEDIATE 事务与 `core` 锁内 ✅ medium-high

- **设计**：`rust/MEDIA.md:125`（对 OCR）——「**下载/识别绝不持有数据库互斥锁**」；`rust/ENGINE.md:15` ——「网络 await 不持锁」；
  `rust/MEDIA.md:28` ——「下载是同步且限时的，**事务串行处理素材写入**」。
- **代码**：`engine/mod.rs:424` 进入 `ingest_with_source` 即持有 `core` 锁；`engine/mod.rs:537-542` 以 `&*self.db()?`（store 锁）调用 `collector.ingest`；
  `media/mod.rs:100` → `124-125` 开 `store.immediate()` → `144` 执行阻塞 `read_image`（ureq，超时 `timeout_seconds` 默认 15s）→ `232/252` 才 `commit`。
- **后果**：一次入站图片下载期间，`core` 锁与 store 锁同时被持有，且写事务（IMMEDIATE）一直开着 —— 引擎的 tick/其他 chat 的 cycle、控制套接字的读库全部被挡住。
  这与项目自己为 OCR 明文规定的「绝不持有数据库互斥锁」形成**同一仓库内的双标**（OCR 实现是对的：`media/ocr.rs:387` 先下载、`397` 才取锁）。
- **注**：`rust/MEDIA.md:28` 允许「下载同步」，但未允许「跨事务/跨 core 锁同步」；`ENGINE.md:15` 的措辞针对 await，此处是等价形态的阻塞调用。

#### IO-09 `enable_media()` 在**每条入站消息**上重跑一遍建表 DDL ✅ medium

- **设计**：`rust/MEDIA.md:32` ——「`Store::enable_media()` **幂等**创建五张新表，已有表不变」；`:82` ——旧库「通过 `PRAGMA table_info` 探测后 ALTER」。
- **代码**：`media/mod.rs:100` 在 `ingest` 入口无条件调用；`media/mod.rs:320-343` 内是 `immediate()` + `execute_batch(include_str!("media.sql"))`（5 × `CREATE TABLE IF NOT EXISTS`）+ `PRAGMA table_info` + 4 次列存在性判断。
- **后果**：「幂等」被实现成「每次都做」，且每次都要拿一次写事务。在热路径（每条人类消息）上引入 5 条 DDL 与一次 PRAGMA，纯属白做，
  并与 IO-08 叠加放大锁竞争。
- **同仓库的正确写法就在手边**：`engine/mod.rs:299-309` 对 affect 与 humanize 的表都是**在 `Engine::new` 里按开关建一次**
  （`affect::enable` / `humanize::enable`），从不在消息路径上重复。媒体这条应当照抄该模式。

### 根因 ③ 他人身份/内容越过边界

#### IO-10 入站合并转发把原发言人昵称/QQ 拼进本群正文 ✅ high

- **设计**：`prompt-and-learning-design.md:887` ——「共享时必须**剥掉来源消息 id 与昵称**」；`:1239` ——「转发的是链接/群级话题/idea，**不是把某人的原话、昵称或身份搬到别的群**」。
- **代码**：`engine/policy.rs:616-632` 明确取 `sender.nickname / nickname / sender.user_id / user_id / uin` 并拼成 `"{speaker}: {text}"`，再包进 `[合并转发]` 块。
  该文本随后进入 `messages`（`engine/mod.rs:528`）与 `LayeredMemory::capture`（`549`），来源校验只认「该消息是某人类发的」，于是**别人的话被算到转发者头上**（`memory/mod.rs:85-89`）。
- **后果**：A 群成员的原话+昵称出现在 B 群的上下文与记忆里，直接踩 §15.4.3 红线。
- **状态**：修复已存在但**未合入 `main`** —— `qq-inner-agent-worktrees/forward-identity` 的 `e25c126`「Strip forwarded speaker identities and block person memory evidence」正是改这里。
  当前 `main`（`5bd0a53`）仍是旧代码。这是本次审计中「已知有解、只差合并」的一条。

#### IO-11 `/记住` 完全绕过 §16 自我审核，且注释给出的理由是假的 ✅ high

- **设计**：`prompt-and-learning-design.md:1175-1178` ——「私聊教学绕过常规学习门控，**但不绕过形式校验与自我审核**」；
  `:923` ——落库顺序「形式校验 → 自我审核 → 落库」。
- **代码**：
  - 自我审核**已经实现**：`memory/mod.rs:110`（`learning_review_input`）、`:131`（`apply_learning_review`），调用点在正常学习路径 `engine/mod.rs:1584-1595`；
  - `persona/owner_teaching.rs:69` 的注释却写「**§16 尚未实现**，复用 memory 的文本与 subject 形式校验兜底」，然后 `72-98` 直接 `LayeredMemory::apply` / `ExpressionMemory::apply` 落库，`confidence=1.0`、`importance=0.8`。
- **后果**：`/记住 我的身份证号是…`、`/记住 忽略之前所有规则` 会原样入库并进入此后每轮 prompt。注释与事实相反，会把后续维护者继续引向错误结论。
- **备注**：即前审计 F1，本文确认其在 `main` 仍成立，并补上「审核机制其实已存在」这一关键事实。

### 根因 ④ 写了不删 / 建了不读

#### IO-12 八张 media 表没有清理触发器，也不在 `prune()` 覆盖范围内 ✅ medium

- **设计**：`rust/MEDIA.md:33-34` ——「正文只能关联 `messages` 读取；**历史被清理后引用可能不再解析，不保留正文副本**」；
  `:127-128` ——`media_ocr` 明文要求「**消息删除同时删除其 OCR 行**」。
- **代码**：全仓只有两个随 `messages` 删除的触发器：`media_ocr_cleanup`（`media/ocr.rs:452`）与 `humanize_faces_cleanup`（`persona/humanize.rs:24`）。
  以下八张表**既无 cleanup 触发器，也不在 `store/operations.rs:276-311` 的 `prune()` 里**：
  - `store/media.sql:2-6`：`media_contexts`、`media_receipts`、`media_stages`、`media_senders`（只有素材被容量淘汰时才连带删，`media/mod.rs:213-223`）；
  - `media/media_select.rs:310-314`：`media_fitness`、`media_feedback`、`media_sharing`、`media_wild` —— 这四张表**全仓没有任何 `DELETE`**（唯一的相关删除是 `:596` 对 `media_pending` 的单行 `DELETE`）。
- **后果**：`messages` 按 30 天/每 chat 条数裁剪，而这些表只增不减 —— 活跃群上会按「历史消息数 × 段数」无界增长，
  且残留行 join 不到 `messages`，成为纯死重。`media_stages`/`media_fitness` 还各自带着一份分类 JSON，体积不成比例。
- **备注**：前审计 E-D9 报过 `media_stages` 无读取方；本文的结论更强——**它没有清理，也没有消费者**（见 IO-14）。

#### IO-13 `notes` 是死表；`group_hours` 永不裁剪 ✅ low

- **设计**：`store/schema.sql:5` 建了 `notes`；`store/operations.rs:93-99` 提供 `note()`。
- **代码**：`note()` 在生产代码中**零调用**（仅 `store/tests.rs:165,265`）。
- **后果**：死表 + 死方法；`group_hours`（`store.rs:62-80`）刻意不随消息裁剪（`:52` 注释说明是有意为之），但也无任何保留期，按 chat×小时无界累积。

#### IO-14 `media_stages` 的「复核」语义落空 ✅ low（已见前审计 E-D9）

- **设计**：`prompt-and-learning-design.md`（§11.5）要求「每个判定都要能被复核」；`rust/MEDIA.md:58-59` ——「保存分类及结构证据，不保存正文」。
- **代码**：唯一读取点在 `media/mod.rs:118`，用途是**给自己重算**最近 24 条的分类，而不是供人或仪表盘复核；控制协议无 media 方法。

### 根因 ⑤ 契约在、调用方不在 / 一把开关混用

#### IO-15 `send_forward` 无条数上限且 0 调用方；`forwardEnabled` 单开关混合读写 ✅ medium

- **设计**：`prompt-and-learning-design.md:1275` 高风险档要求「**单次转发条数受限**」；`:1274` 低风险档（转手别人的卡片）走正常阈值；
  `:1277` 原则是「让 agent 做『转手』，不让 agent 做『编辑』」。
- **代码**：`transport/mod.rs:261-310` 的 `send_forward` 校验了节点键白名单与 `reference_id`，但**没有任何条数上限**；
  且全仓无调用方（`grep` 仅定义处）。读（`get_forward_msg`，`:312-320`）与写（`send_forward`）**共用同一个 `forward_enabled`**（`:257-259`）。
- **后果**：`forward` 段这条出站路径运行时不可达；契约层面，一旦接线，开关一开就同时放行了「读别人的转发」与「agent 自拼转发」两种风险档，且后者没有条数闸门。
- **更正**：前审计称「relay 全仓 0 调用方」**已过期** —— 低风险 relay 链路现在**已接线**（`engine/mod.rs:1418-1465` 调 `links::collect` / `shortlist` / `reviewed`，结果并入 `externalTopics`），
  它用的是**文本注入**而非 QQ `forward` 段。因此本条只针对 `send_forward` 这条段级写路径，不等于 §21.7 整体未实现。

#### IO-16 `db.observe` 对回填同样生效，污染 `priorExpectation` ✅ medium（已见前审计 D3）

- **设计**：`rust/ENGINE.md:40` ——「`restore` 只恢复最近人类消息位置，保留记忆但**不重放、不因旧历史主动发言**」；`ARCHITECTURE.md:60` ——感知与值班解耦。
- **代码**：`engine/mod.rs:551` 的 `db.observe(&value, now)` 位于 `let Some(s) = state else { … }`（`:552-554`）**之前**，对 `backfill=true` 同样执行；
  而相邻的 state/version/`last_human`/pending 更新都被 `state=None` 挡住（`:520-527`）。
  `store/operations.rs:239-244` 的条件是 `ts<=now AND expires>=now`，历史消息可以满足。
- **后果**：回填的历史消息会被登记为「我在等的那个回应」，进入 `priorExpectation`，使模型以为对方回应过自己。

#### IO-17 超大响应的错误码与文档不符 ✅ low

- **设计**：`rust/PROVIDER.md:11` ——「响应大小按 JS UTF-16 长度限制为 1,000,000，并设置字节读取上限」。
- **代码**：`transport/provider_transport.rs:258-276` 先 `.take(4_000_001)` 读字节，再判 `encode_utf16().count() > 1_000_000`。
- **后果**：UTF-16 超限但字节 <4MB 时正确返回 `response_too_large`；**字节 >4MB 时被静默截断**，`serde_json::from_str` 失败 → 返回 `invalid_provider_response`。
  同一个「响应过大」在 JS 与 Rust 下会是两个不同的错误码，与 `PROVIDER.md` 的「逐字一致」目标不符。

#### IO-18 `blocked_until` 在热重载后丢失 ✅ low（观察，无明文要求）

- **代码**：`main.rs:444-451` 每次 `reload` 都 `Runtime::new` → 新 `Provider::new`（`provider_transport.rs:27-43`，`blocked_until = 0`）。
  小时预算因为落在 `Store.calls` 表里（`store/operations.rs:150-164`）可以跨代，但 60s/300s 封锁不能。
- **后果**：热重载会清掉 `http_<N>_check_provider_config` 的 300 秒封锁；若配置本身没改、只是别的键变了，下一次 cycle 会立刻重试并再次失败。
  `PROVIDER.md` 只说封锁在「克隆之间」共享（`:3`），没规定跨代语义，故仅记为观察项。

#### IO-19 README 承诺的三个本地命令在内核里不存在 ✅ medium

- **设计/文档**：`README.md:286`「`./agent contacts` | List available group and friend IDs locally」；
  `README.md:291`「`./agent check --api` | Authenticate and make one small model test request」；
  `README.md:294`「`./agent add-memory group:123 "…"` | Add an owner-authored note for exactly that chat」。
- **代码**：`agent:24` 把 `check|contacts|add-memory` 全部转发给内核二进制；而 `rust/src/main.rs:33-49` 的 `enum Command` 只有
  `Selftest | Start | DbSchema | Config | ConfigDefaults | Check` —— **没有 `contacts`、没有 `add-memory`**，`Check` 也没有 `--api` 参数。
- **后果**：照 README 执行会得到 clap 的 `unrecognized subcommand` / `unexpected argument`。这是文档承诺的 I/O 端点**完全不存在**，
  不是行为差异；`add-memory` 在 Node 侧（`src/cli.mjs`）也只回显一句提示，等于「加主人笔记」这条路径没有可用入口。
- **修复**：`README.md:286,291,294`。要么补齐子命令，要么把这三行从文档撤下。

#### IO-20 affect 表与设计文档的维度命名/主键不一致 ✅ low

- **设计**：`prompt-and-learning-design.md:481` 把二维心情的横轴明确命名为 **`valence`**（心情值：愉悦 ↔ 不悦），`:564` 又写「状态级：`valence`/`rationality` 以有界步长累积」；
  `:371-375` 规定 `message_ratings` 为 **`PRIMARY KEY(message_id)`**。
- **代码**：`store/affect_schema.sql:3` 的 `affect_state.dimension` 只允许 `('mood','rationality','affinity')` —— 设计里的 `valence` 在库中叫 **`mood`**；
  `:2` 的 `message_ratings` 用的是 **`PRIMARY KEY(chat,message_id)`**。
- **后果**：功能上等价（`mood` 就是设计的 `valence` 轴，复合主键与 `messages` 的 `(chat,id)` 主键一致，其实更稳），但**任何照设计文档写的查询/仪表盘字段都找不到
  `valence` 这个维度**；`message_ratings` 的键也与文档不符。属命名/契约漂移，不是数据错误。
- **附带**：`persona/affect.rs:138` 的 `bounded_step` 用 `signal<0 → 0.20 : 0.10` 后统一 `clamp(-0.15, 0.15)`，
  于是设计 `:417` 的「负面最多 −0.20」永远被 `:416` 的「单次 ≤±0.15」压到 −0.15 —— **这两条设计自己互相矛盾**，代码选了后者。建议直接改文档。

---

## 三、无文档依据但可疑（不计入偏差）

以下都不违反明文契约，但结构上值得记一笔。

#### S1 三个出站 HTTP 客户端的跳转策略不一致 ✅ medium

- **事实**：`provider_transport.rs:169-177` 显式 `.redirects(0)`（`PROVIDER.md:5` 有要求）；而
  `media/mod.rs:442-446`（图片下载）与 `topic/mod.rs:321-324`（RSS/GitHub 抓取）都**沿用 ureq 默认的最多 5 跳**，没有 `.redirects(0)`。
- **为何可疑**：图片 URL 来自 OneBot 段（`data.url`），不是固定白名单；一次跳转就能把出站 GET 引向任意主机（含 `127.0.0.1:<port>` 这类内网端点），并把响应字节落到 `data/media/`。
  凭据泄漏风险低（ureq 2.x 跳转默认剥离 `authorization`/`cookie`），但这三种客户端对同一风险给出三种不同答案，应当统一。
- **说明**：文档只为 provider 规定了跳转策略，故不作为偏差。

#### S2 停机时限可能与 systemd 宽限期冲突 ✅ low

- **事实**：`main.rs:605-608` 是 `control.stop()` → `rt.shutdown()`；`shutdown()`（`:479-496`）在 `engine.stop()` 后自旋等待 `Arc::strong_count(&store) > 1` 消失（注释自述为等在途的 `spawn_blocking` HTTP）。
  而 `scripts/install_service.py:21` 的 `TimeoutStopSec=25`，`provider.timeoutSeconds` 上限是 300s（`config.rs:556`）、OCR 超时上限同为 300s。
- **为何可疑**：一个跨过 25s 的阻塞请求会让 systemd SIGKILL，`agent.sqlite-wal` / `control.sock` / `"stopped"` 日志的清理可能缺失。文档未承诺停机时限，故不计为偏差。

#### S3 媒体容量淘汰不保证真的腾出空间 ✅ low

- **事实**：`media/mod.rs:181` 的 `ensure!(bytes <= max_total_bytes)` 只对新素材**单条**设限；随后的 victim 循环（`:182-186`）在没有足够可淘汰项时自然结束，仍会写入新行。
- **为何可疑**：文档承诺「全库文件字节总量 100 MiB」（`MEDIA.md:42`），此边界路径下索引会超限。需已有大量素材且 `maxTotalBytes` 被调小才触发。

#### S4 `atomic_json` 不做 fsync ✅ low

- **事实**：`settings.rs:29-52` 写 `.tmp` → `rename`，中间没有 `sync_all`；而同一个仓库对媒体文件明文要求「写临时文件、**sync**、rename」（`MEDIA.md:45`）。
- **为何可疑**：两类落盘的持久性语义不一致；配置侧确实没有 fsync 契约（Node `atomicText` 也没有），故只是观察。

#### S5 控制协议缺少 §19.9/§13.8 设计的方法 ✅ 已见前审计 E-D7

`privacy.list/exclude/include` 与素材后台覆盖方法不存在；`Store::set_media_source_override` 零调用方。前审计已报，此处仅确认仍然成立。

---

## 四、已确认一致（避免重复劳动）

以下均已回到代码核对，**不要**再作为偏差上报：

1. **模型端点的三个易错点**：去所有尾斜杠、已带后缀则原样返回、仅 Anthropic 且未以 `/v1` 结尾才插入 `/v1`（`provider.rs:62-88`）。
2. **预算与拒绝顺序**：先判封锁 → 空 key → `call_budget`；三者被拒都不发请求、不计预算（`provider_transport.rs:196-221`）；DB 错误禁止发送（`:213`）。
3. **重试语义**：`floor(retries)+1` 次、退避 1/2/4/8/16/30、`Retry-After` 取较大者并封顶 60s、耗尽封锁 60s、配置错误封锁 300s 且不重试（`provider_transport.rs:91-121, 241-256, 280-298`）。
4. **每次尝试新建 agent、禁跳转、禁环境代理、DB 锁不跨网络**（`provider_transport.rs:168-177, 195-221`）。
5. **模型列表**：不重试、`models_http_<N>` / `invalid_model_list`、DeepSeek hostname 精确匹配、只用 Bearer、前 500 条按 UTF-16 排序（`provider_transport.rs:128-154`）。
6. **取消语义**：`spawn_blocking` 不可中断、预算不退还、旧结果不得写入新引擎（`engine/mod.rs:1069-1083`、`ENGINE.md:35-37`）。
7. **OneBot 收帧**：UTF-16 >1e6 丢弃、echo 匹配响应、`post_type` 为真才是事件、early ≤200 且与状态同锁补发（`transport/mod.rs:324-368, 388-391`）。
8. **OneBot 出站段构造**：顺序固定 `[reply][at][text][face]`、`at` 仅 group、`auto_escape:true`、chat/face 严格校验、media 白名单重建 data（`transport/mod.rs:194-256, 614-635`）。
9. **投递账本**：先写 pending 再发送、成功/确定失败/不确定分别落库、`recover_deliveries` 只标 uncertain 不重发（`engine/mod.rs:1984`、`store/operations.rs:40-47, 173-185`）。
10. **入站链路顺序**：`normalize` → `db.message` 去重 → OCR 入队 → 素材采集 → `orientation.observe` → `LayeredMemory::capture` → `db.observe`，
    与 `ARCHITECTURE.md:50-57` 和设计 `:251`（capture 在 observe 之前）一致（`engine/mod.rs:450-551`）。
11. **指令边界**：`/记住` 等只在主人本人私聊、群聊无例外、且**去重之后**才执行（`persona/owner_teaching.rs:17-21`、`engine/mod.rs:500-518`）。
12. **OCR 是正例**：先下载（`media/ocr.rs:387`）后取锁（`:397`），符合 `MEDIA.md:125`；`media_ocr` 有随消息删除的触发器（`:452`）。

以下为**控制套接字、外部 HTTP、媒体入站、文件系统、进程**五个面的核对结果，均回到代码确认：

13. **控制套接字分帧与上限**：`MAX_LINE = 1 MiB`（`control.rs:33`）；半包保留、粘包逐行、超长行排空至换行、无换行 EOF 不执行请求；错误封套 `{id, ok:false, error:{code,message}}` 与 `ARCHITECTURE.md:122-129` 一致。
14. **事件推送**：白名单恰为文档的 7 类（`control.rs:35-43`）；慢客户端用 `try_send`，满队列**只丢事件、不阻塞引擎**（`:49-64`）；每条连接独立有界队列 `QUEUE=32`（`:34`）。
15. **socket 生命周期**：bind 前只清理失去监听者的旧文件、`SocketFile` 用 dev/ino 守卫（`control.rs:80-119`）；目录 0700 由 `main.rs:526` 保证。
16. **密钥脱敏**：任何响应与捕获文本都按 `Backend::secrets` 替换为 `[redacted]`（`control.rs:423-431`）；捕获在 `resolve_forwards` **之前**取原始事件（`main.rs:332-335`）、上限 30 条 × 1000 字符、不落盘。
17. **外部 HTTP 与媒体**：话题抓取 10s 超时 + 256 KiB 上限（`topic/mod.rs:322-337`），安全闸门先于形成模型且审核失败即不注入（`engine/mod.rs:1387-1416`）；
    媒体取字节只有三条路径（`data.url` 的 http/https → 本地 `file` / `file://` → 失败），不猜 opaque id/base64（`media/mod.rs:434-453`），单文件 2 MiB / 全库 100 MiB、写 `.tmp`→`sync_all`→rename、失败回滚（`:187-240`），目录 0700（`:281-292`）；
    OCR 无 shell 插值、stderr 丢弃、超时 + 4 MiB 输出上限、临时目录 0700 + RAII 清理（`ocr.rs:132-178`）。
18. **文件系统与进程**：`data/` 0700 / `agent.sqlite` 0600（`main.rs:520-533`）、`agent.log` 0600 且 1 MiB 轮转；`revision` 语义与 `.settings-write` 暂停重载一致；
    三类信号 handler 在打开资源前预装（`main.rs:505-513`）；`agent start` 的 `flock --nonblock -E 75` 与 README 的 exit 75 一致。
19. **数据库事务**：`Store::learn`（`store/learning.rs:100`）与 `reset_learning`（`:160`）都用 `immediate()`；`messages` 用 `INSERT OR IGNORE` 而非 `REPLACE`（`store/operations.rs:48-61`，注释说明 REPLACE 会破坏去重）；`call_budget` 的删除+计数+插入在同一个 IMMEDIATE 事务内（`:150-164`）。
20. **数据库读取口径**：JSON 列的编码/解码是对称的 —— `chat_learning.sources`、`send_assessments.details`、`expectations.forecast/observation`、`memory_layers.sources/keywords`、`expressions.sources`、`media_assets.fitness`、`group_orientation.sources/analysis` 都各自有对应 `decode()`（`store/operations.rs:90,202,254-256`、`memory/mod.rs:513`、`persona/expression.rs:149`、`media/mod.rs:402`、`store/orientation.rs:56`）；
    `prune()` 覆盖了 messages / thoughts / learned_memories / memory_layers / chat_learning / decisions / deliveries / send_assessments / expectations / handled，`expressions` 由 `ExpressionMemory::prune`（`persona/expression.rs:179`）按保留期与每 chat 上限单独处理，`memory_revisions` 由触发器清理。
    `decisions` 表本身无生产读者，但每次 `record_decision` 都会同时发 `decision` 日志事件（`engine/mod.rs:393-396`），所以「判定可观测」由日志承担，表只是留档。

---

## 五、建议修复顺序

1. **IO-01 / IO-02 / IO-03**（同一段代码）：把 `bubbles` 纳入 `decorate` 与内容闸门、落库实发序列、失败即停止。这是当前唯一「模型可控、可绕过安全闸门」的通道。
2. **IO-10 / IO-11**：两条红线。IO-10 只差合并 `forward-identity` 的 `e25c126`；IO-11 需要把 `/记住` 接回 `apply_learning_review` 并删掉 `owner_teaching.rs:69` 的假注释。
3. **IO-08 / IO-09**：把媒体下载移出事务与 `core` 锁（照 OCR 的写法），并给 `enable_media` 加记忆化。二者叠加是入站热路径上最重的锁持有。
4. **IO-04 / IO-05 / IO-06**：多气泡的条数/档位/场景门控与延迟公式，按 `human-like-replies.md` 逐条对齐。
5. **IO-07**：给 `resolve_forwards` 加每次事件的展开次数上限，并把入站处理移出主循环的 await 路径（或以有界并发 + 独立任务消费 `notices`）。
6. **IO-12 / IO-13 / IO-14**：补 cleanup 触发器或让 `prune()` 覆盖这八张表；删除 `notes` 死表；给 `media_stages` 定一个消费者或停写。
7. **IO-15 / IO-16 / IO-17 / IO-18**：契约缺口与低危观察项，随相关模块维护时一并处理。
8. **IO-19 / S1**：README 承诺的 `contacts`、`add-memory`、`check --api` 要么实现要么从文档撤下；三个出站 HTTP 客户端统一跳转策略（建议都取 `.redirects(0)`）。

---

## 六、方法学备注

- 本轮的 oracle 来自 12 份文档的**逐句抽取**（362 条可判定要求 + 数值常量表），而非印象式阅读；每条偏差都能指回一句明文。
- 两处需要明确区分：`docs/ARCHITECTURE.md` 与 `docs/DEVELOPMENT.md` 自述为**现状快照**而非规范（`ARCHITECTURE.md:3`、`DEVELOPMENT.md:5-6`），
  因此它们与代码不符时是「文档过期」而不是「实现违规」——例如 `ARCHITECTURE.md:111` 仍写多气泡契约、而 `DEVELOPMENT.md:48` 已承认它从未生效。
- **未覆盖**：`docs/PLUGIN-PORTS.md` 的中立端口模型（P1–P6）属**设计目标**，当前实现未接线，故未纳入本次偏差统计；
  一次性的性能数字（如每条消息的 DDL 开销）未做实测，只报结构性问题。
- **本工作区无线上 `data/`**（无 `control.sock`、无 `agent.sqlite`、无 `agent.log`），也没有构建好的 release 二进制；
  因此所有结论都是代码/文档事实，**没有一条用线上数据反证过**。凡属此类，上文均已标注为「结构性问题」而非「线上已发生」。

### 对既有结论的更正

1. **`topicSource` 的关闭口径已变**：`audit-design-vs-implementation.md:147` 与 `DEVELOPMENT.md:147` 称「`Settings::enabled()` 要求来源非空」——
   **已不成立**。`topic/mod.rs:47-50` 现在只返回 `self.enabled`，测试 `topic/mod.rs:724-752` 明确断言「有来源但 `enabled=false` 不抓取」。
   即现在的口径是**显式开关**而非「来源非空」，与该处描述不同。
2. **relay 的「全仓 0 调用方」已过期**：低风险链路现在**已接线**（`engine/mod.rs:1418-1465` 调 `links::collect` / `shortlist` / `reviewed`）。
   仍为死代码的只有高风险合并转发路径（`OriginKind::AgentMerged` 仅测试构造、`send_forward` 无生产消费者）——本条已并入 IO-15。

### 被推翻的并行审计结论（记录教训）

本轮把一部分面（控制套接字 / 外部 HTTP / 媒体 / 文件系统 / 进程）并行下发给子代理。其报告 **2 条 high 与 1 条 medium-high**，逐条回代码复核后：

- **`debug.receive.store`（high）—— 假的**：`grep -rn "debug.receive" rust/src/control.rs` 只有 `start`/`status`/`stop` 三个分支，全仓不存在该方法。
  它把内部的 Rust 函数 `Backend::observe`（`control.rs:377`，由 `main.rs:332` 直接调用，**不经 socket**）误当成了一个 socket 方法，并据此编出了「未文档化的第 12 个方法泄露群聊正文」这一结论。**整条作废。**
- **「捕获窗口口径不一致」（medium-high）—— 不成立**：`README.md:125` 明写「send several messages … **to yourself or another chat**」，
  「另一个群」本就是文档化的被测行为，代码的 `private | group` 与之相符。
- **「README 承诺的 `contacts` / `add-memory` / `check --api` 不存在」（high）—— 成立**，已采纳为 **IO-19**（并自行复核了 `main.rs` 的 `enum Command` 与 `agent:24` 的路由）。

数据库面的子代理长时间未产出，已中止；**该面由我第一手完成**（IO-08、IO-09、IO-12、IO-13、IO-14、IO-16、IO-20 与已确认项 19、20），因此不存在未复核的二手结论。

这正是 `audit-design-vs-implementation.md:127` 那条教训的复现：**子代理会误读代码，凡 high 结论必须回到代码核验，并以可执行证据反证。** 本轮子代理自报的 2 条 high 里 1 条为假（50% 误报率），且它编造的是一个**听起来很合理、很容易被直接写进审计文档**的结论。
