你是 qq-inner-agent 的 Rust 移植工程师。当前工作区是**隔离的 git worktree**,基线是 P5 之后的
`main`。改动不影响主分支。

## 前置

已有 `config.rs`、`settings.rs`、`text.rs`、`policy.rs`、`sending.rs`、`prompts.rs`、
`activity.rs`、`provider.rs`、`onebot.rs`、`store.rs`。**先全部读一遍再动手** ——
本阶段是把它们接起来,不要重复实现已有函数。

`policy.rs` 目前**没有** `normalize`(事件 → 内部消息);若 P5 尚未合入,请在动引擎前先把它补上
(规格见 `phase5-normalize-activity-orientation.md` 第 1 节),或者明确告知我缺失。

## 必读

- `docs/working/rust-port/ARCHITECTURE.md`(不可变更契约)
- `docs/working/rust-port/SURVEY.md` —— **第 2 节是 48 步 cycle 调用链**,**第 8.2 节是引擎要点**,
  第 9 节是性能热点
- `src/engine.mjs`(259 行,**逐行读完**)与 `src/main.mjs`(97 行)

## 任务:P6 —— 决策引擎与主循环

### 1. `engine.rs`

复刻 `Engine`,**严格执行 SURVEY 第 2 节的顺序,不要重排**:

- `state(chat)`:`maxActiveChats` 上限;创建 `{ version, lastHuman, lastId, hint, pending, pauseDone, lastThink, busy, due }`
- `ingest(event)`:入群通知分支(自己加入白名单群 → `orientation.joined()` + `version++`,重复通知不得重复递增)
  → `available()` 检查 → `normalize` → 去重写库 → `orientation.observe` → 学习捕获 → `observe`
  → `version++`、`lastHuman`、`lastId`、`hint` 保持批内 `self`、`pending=true`、`due = now + debounceSeconds`
  → **`ingest` 绝不发模型请求**
- `tick()`:活跃检查 → 清理冷聊天 → readiness/连接/在线检查 → 遍历 chats,
  受 `maxConcurrentChats` 限制,跳过 busy/过期/未到 `due`/未到 `minThinkIntervalSeconds`
  → `trigger = pending ? "message" : (!pauseDone && now-lastHuman >= pauseSeconds ? "pause" : null)`
  → 非点名且(`!proactive || quiet`)时清 pending 并跳过 → `busy = true` 并起任务
- `cycle()`:按 SURVEY 第 2 节的 48 步实现。几个**必须保住**的点:
  - `orientation.beforeSpeak` 失败 → `due = now + 5` 后返回
  - **`obsolete()` 的五个检查点**:aborted / version / activity 块 / learning epoch / orientation epoch
  - 已有 `store.assessment(chat, id)` 时直接 finish(防重复处理)
  - 四阶段 `provider.json`:formation → (可选学习) → evaluation → forecast → articulation
  - **先落库再发送**:`store.delivery` 必须在 `transport.send` 之前
  - 送达结果不确定时**只记录、绝不自动重发**
  - `finish()` 统一清 `pending`、`hint='open'`、必要时 `pauseDone`、写 `markHandled`
- `restore()`:保留记忆,但**不重放旧回复、不因旧历史主动发起**

### 2. 并发与取消(与 JS 的对应关系要写清楚)

JS 是单线程事件循环 + `AbortController`;Rust 是 tokio 多任务。请:

- 用 `tokio::select!` 或 `CancellationToken` 复刻"取消后不留部分状态"的时机;
- **明确说明**每个 `obsolete()` 检查点在 Rust 里由什么触发;
- 同一个 chat 必须**单飞**(`busy` 语义);跨 chat 并发受 `maxConcurrentChats` 限制;
- 配置重载时:停止引擎 → 保留 `chats` 中仍被允许的状态(重置 `busy`/`lastThink`)→ 整体重建
  `Engine`/`ActivityRhythm`/`GroupOrientation`,**共用同一个 store**。

### 3. `main.rs`

- `status()` 每 5 秒原子写 `data/status.json`,字段与语义**逐字对齐**现有实现
  (见 SURVEY 第 2 节与 ARCHITECTURE 第 6.1 节的回归红线);
- `revision` 监视器每 1 秒比较,存在 `.settings-write` 时暂停重载;
- 日志行格式与现有 JSON 行一致;信号处理干净停机。

## 硬性约束

1. **不得引入新依赖。**
2. **不得修改 `rust/` 以外的任何文件。**
3. 中文注释,重点标注:`obsolete()` 五个检查点、`version` 递增时机、`busy` 的互斥语义、
   先落库后发送的理由、取消与 `spawn_blocking`(provider 是阻塞的)之间的边界。

## 验收标准

1. `cargo build --release`;`cargo clippy --all-targets -- -D warnings`;`cargo test` 全过。
2. **必须做 golden 对比**:用内存 store + mock provider + mock transport 跑一段脚本化对话
   (含:被点名、开放轮次、突发合并、冷却、静默时段、配额用尽、生成期间来新消息导致作废、
   送达不确定、dry-run),把**决策序列**与 JS 版在同样输入下的输出逐条比对。
   Node 不可用时跳过而非失败。
3. 单测覆盖:turn allocation、`interruptThreshold`、withhold、保留候选、静默时段、冷却、
   chat 隔离、过期响应、dry-run、非法模型输出、API 预算、模糊送达。
4. 不访问真实 `data/`、不发真实网络请求。

## 交付

1. `git add -A && git commit`,英文 commit message。
2. 用**一段话**回报:实现了什么、测试怎么跑、与 JS 的已知差异或不确定点。

---

## ⚠️ 补充:引擎必须包含的"更像真人"行为

`docs/working/human-like-replies.md` 里设计了四项行为改进。**提示词部分已经在 JS 侧实现,并经生成器
同步到了 `prompts.rs`**(长度分档 `lengthTarget` 与 `policy::pick_length_target` 也已就位)。
但下面这些**是引擎行为**,不是提示词,必须在 P6 落地 —— 否则移植后这些设计就丢了:

1. **长度分档要真正生效**:组装 articulation payload 时调用 `policy::pick_length_target(hint, random)`
   并把结果作为 `lengthTarget` 传入(**被直接点名时不得给出 `tiny`**)。
2. **表情/face 频率按群学习**,而不是全局固定:
   `p(chat) = clamp(该群人类表情使用率 × 0.8, 0, 0.35)`,冷启动保守用 0.08。
   数据来源可复用现有的表达/特质学习层;需要一个带衰减的滑窗统计。
3. **允许"只发表情"的一条消息**:模型返回空文本 + 一个 face 时,发送**只有 face 段**的数组消息。
   硬性限制:仅 `open`/`other` 且动机中低时允许;**被直接点名、对方求助或表达难过时禁止**;
   每 chat 连续不超过 1 次;计入 `maxMessagesPerHour` 配额。**注意这需要放宽现在
   "正文非空否则 `invalid_articulation`" 的校验**,并在提示词里补上对应指令
   (目前提示词**刻意不含"空文本"措辞**,就是因为引擎还不支持)。
4. **分条发送(多气泡)与打字延迟**:ARTICULATE 返回多条时按间隔依次发送;
   发送前加与长度成比例的短延迟(含抖动)。若本阶段不便实现,请在回报里**明确说明未做**,
   不要静默省略。

以上任一未实现,都必须在回报里写清楚,我会据此安排后续阶段。


---

## ⛔ 本阶段**不要**实现的设计(重要)

`docs/working/prompt-and-learning-design.md` 里记录了几项**已设计但尚未实施**的改动。
本项目的铁律是**与现行 JS 行为逐字对齐**,所以在本阶段:

- **不要**实现提示词分层重构(把身份/背景与任务契约、行为准则拆开);
- **不要**实现学习分诊(`learn` / `partial` / `skip` 三档与自动升格);
- **不要**实现 affect 指标(心情值 / 好感度 / 认同度,二维心情与四象限 disposition);
- **不要**实现记忆召回下钻(`recall` 字段、回查原始历史与语境)。

**一律以 `src/*.mjs` 的现行实现为准**;遇到"设计文档说要改、但 JS 还没改"的地方,
按 JS 现在的行为移植,并在回报里提一句你注意到了这处设计。

这些改动会在 JS 侧先行落地后,再单独派发移植任务。


---

## 📌 补充:三层决策架构与零模型初筛

见 `docs/working/prompt-and-learning-design.md` **第九节**。"要不要说话"是三件事,输入完全不同:

| 层 | 决策 | 主要输入 |
| --- | --- | --- |
| **① 回复必要性** | 这条消息要不要回 | 消息、是否被点名、`expectation`、冷却、配额、静默时段、activity 块 |
| **② 发起话题必要性** | 我要不要主动开话头 | **群的状态**、上次发言距今、想法池、节奏块、主动冷却、**该群作息** |
| **③ 说什么** | 怎么说才像这个人 | 人设 + personality + traits + 该群表达 + 长度分档 + 语气 |

要求:

1. **① 与 ② 必须是两个独立判定点**,不要合并成一个"要不要说话";
2. **零模型初筛**:①② 先用确定性判据筛掉(静默时段、配额、冷却、无新消息、想法池为空、不在该群作息内),
   **只有通过初筛才调用模型** —— 省额度,且让决策可审计;
3. ② 的完整触发条件与"先排除后触发"见 `phase6b-media-select.md` 第 5 节,
   其中 `groupActivity` 与"该群作息"**必须与本引擎自身的 activity 节奏分开**。

## 📌 补充:虚构的界线(需要同时改提示词)

见 `docs/working/prompt-and-learning-design.md` **第十四节**。`boundary` 里"不得编造亲身经历"要改为**责任线**:

- **放开**:无害的经历虚构、情绪/状态表达;
- **仍然禁止**:涉及对方决策或利益的说法、被直接问时冒充人类、涉及第三方的具体事实。

落地:改 `src/prompts.mjs` → 跑 `rust/tools/gen-prompts.mjs` → 更新 `tests/prompts_parity.rs` 里那条相关断言。
**三处必须一起改,否则 parity 测试会失败**(这是有意设计的提醒)。
