# P6a 决策引擎核心

你是 qq-inner-agent 的 Rust 移植工程师。工作区是**隔离的 git worktree**。

## 定位:**移植**阶段(与 P5b 不同)

`src/engine.mjs` **已经存在**,所以本阶段是**移植**,不是新功能:

- **JS 是参照实现**,Rust 必须镜像它;
- 验证走 **JS 交叉对照**,不是凭空断言;
- 无法对齐的地方**必须在注释与测试里写明**,不许悄悄改行为。

## 范围(只做这些)

`src/engine.mjs` 的 **Engine 本体**:

- `state(chat)`:`maxActiveChats` 上限、每 chat 的 `{version, lastHuman, lastId, hint, pending, pauseDone, lastThink, busy, due}`
- `ingest(event)`:入群通知 → `available()` → `normalize` → 去重写库 → `orientation.observe` → 学习捕获 → `observe`
  → `version++` / `lastHuman` / `lastId` / 批内 `hint='self'` / `pending=true` / `due = now + debounceSeconds`;
  **`ingest` 绝不发模型请求**
- `tick()`:活跃检查 → 清理冷 chat → readiness/连接/在线检查 → 遍历 chats,受 `maxConcurrentChats` 限制,
  跳过 busy / 过期 / 未到 `due` / 未到 `minThinkIntervalSeconds`;
  `trigger = pending ? "message" : (!pauseDone && now-lastHuman >= pauseSeconds ? "pause" : null)`;
  非点名且(`!proactive || quiet`)时清 pending 并跳过;`busy = true` 后起任务
- `cycle()`:**严格按 `docs/working/rust-port/SURVEY.md` 第 2 节的 48 步顺序,不要重排**
- `restore()`:保留记忆,但**不重放旧回复、不因旧历史主动发起**

## 必须保住的关键点

1. **`obsolete()` 的五个检查点**:aborted / version / activity 块 / learning epoch / orientation epoch。
   在 Rust 里要**逐一说明每个检查点由什么触发**;
2. **先落库再发送**:`store.delivery` 必须在 `transport.send` **之前**;
3. **送达结果不确定时只记录,绝不自动重发**;
4. **同一 chat 单飞**(`busy` 语义);跨 chat 受 `maxConcurrentChats` 限制;
5. `orientation.beforeSpeak` 失败 → `due = now + 5` 后返回;
6. 已有 `store.assessment(chat, id)` 时直接 finish(防重复处理);
7. `finish()` 统一清 `pending`、`hint='open'`、必要时 `pauseDone`、写 `mark_handled`。

## 并发与取消(必须写清楚)

JS 是单线程事件循环 + `AbortController`;Rust 是 tokio 多任务。

- 用 `tokio::select!` 或 `CancellationToken` 复刻"取消后不留部分状态"的时机;
- **provider 是阻塞传输**:取消只能丢弃结果,已发出的请求会跑完、**预算不退还**
  —— 见 `rust/PROVIDER.md`,引擎必须**在取消/热重载后检查任务版本并丢弃旧结果**,
  不得因为取消就假定服务端已停止生成。

## ⛔ 本阶段**不做**

- **不要**写 `main.rs` 的运行时(status.json 写入、revision 监视、日志格式、信号处理)—— 那是 **P6b**;
- **不要**实现"更像真人"的三项行为(长度分档接线、按群表情频率、只发表情、多气泡/打字延迟)
  —— 那是 **P6c**,且属新功能,要**开关化默认关闭**;
- **不要**实现三层决策拆分与零模型初筛 —— 那是 **P6d**;
- **不要**实现素材选择与发送 —— 那是 **P6b-media**;
- **不要**实现 `docs/working/prompt-and-learning-design.md` 里其他未落地的设计
  (提示词分层、学习分诊、affect、召回下钻、跨群共享、自我审核、注意力漂移);
- **不要**改提示词。

**一律以现行 `src/*.mjs` 行为为准。**

## 依赖(都已合并,直接复用)

`config`、`settings`、`text`、`policy`(`normalize`/`select`/`pick_length_target`)、`sending`、
`prompts`、`provider`(含传输)、`onebot`、`store`(含 `learning`)、`activity`、`orientation`、
`memory`、`ranking`、`expression`、`media`、`conversation`。

## 验收标准

1. `cargo build --release`;`cargo clippy --all-targets -- -D warnings`;`cargo test` 全过。
2. **必须做 golden 对比**:用**内存 store + mock provider + mock transport** 跑一段**脚本化对话**
   (含:被点名、开放轮次、突发合并、冷却、静默时段、配额用尽、生成期间来新消息导致作废、
   送达不确定、dry-run),把**决策序列**与 JS 版在同样输入下的输出逐条比对。
   Node 不可用时跳过而非失败。
3. 单测覆盖:turn allocation、`interruptThreshold`、withhold、保留候选、静默时段、冷却、
   chat 隔离、过期响应、dry-run、非法模型输出、API 预算、模糊送达。
4. **不访问真实 `data/`、不发真实网络请求**;时间与随机必须可注入。
5. 测试强度按 `docs/working/prompt-and-learning-design.md` 第十八节:不变量精确,启发式用区间/方向。

## 交付

1. `git add -A && git commit`,英文 commit message。
2. 用一段话回报:实现了什么、测试怎么跑、**`obsolete()` 五个检查点各由什么触发**、
   与 JS 的已知差异或不确定点。
