# 第二批 bug 修复：任务书（待额度恢复后派发）

> **依据**：`docs/working/audit-design-vs-implementation.md`（审计报告）第五节 + 第六节工作顺序。
> **原则**：先修 bug（设计与实现不符），再做新功能；热更新提前。
> **派发方式**：每条一个 worktree，各自独立提交；由父 agent 逐条验收合并。

---

## 通用要求（每条任务都适用，派发时随任务书一起给 codex）

### 测试必须两层
1. **可用性测试**：输入/输出正确、边界与失败路径，以及**接入程序总线**（输入层 → 决策层 → 输出层的接线真的通）；
2. **目标测试**：直接断言该任务对应的**设计文档原句**在**真实链路**上成立，并**注明设计依据（哪份文档、哪一行）**。
   **只过可用性测试不算完成。**

### 任务边界（**越界即失败**）
- 任务书里会给出「**允许修改**」与「**禁止修改**」两份清单；**只准动允许清单内的文件**；
- 若你判断必须动到边界外的文件：**停下来，在回报里说明原因与最小改法**，不要擅自修改；
- **不得**顺手重构、改名、调整格式或"优化"无关代码；
- **不得**删除或弱化与本任务无关的测试；**不得**为了让测试通过而放宽被测试的行为；
- **不得**改动提示词措辞 / 生成物 / 配置默认值，除非本任务明确要求（若要求，必须跑
  `node rust/tools/gen-prompts.mjs` 后 `node rust/tools/gen-prompts.mjs --check`）；
- 提交信息用英文；回报里**分开**写清：改了什么、可用性测试覆盖什么、目标测试断言什么且依据哪一行设计、有无越界需求。

### 收尾门槛
`cargo clippy --all-targets -- -D warnings` + `cargo test` 全过；涉及 Node 侧时 `node --test test/*.test.mjs` 全过。

---

## 任务 1：§14「无害经历虚构放开」被更高优先级的人设层压掉（审计 D1，high）

### 设计依据
- `docs/working/prompt-and-learning-design.md` **§14 L842-854**：把「不得编造亲身经历」卡死并不必要……
  **无害的经历虚构 ✅ 放开**、情绪/状态 ✅ 放开；L856 指明落地位置是**改提示词文本**。

### 现状（已复核，代码实证）
- `rust/src/defaults.json:45` 默认 persona 末句仍是「表达自然、有温度，**不编造亲身经历**，不冒充真人」；
- `src/config.mjs:11` 同一句；`rust/src/config.rs:212` 的 `LEGACY_PERSONAS` 同义；
- 该 persona 经 `persona()`（`rust/src/persona/mod.rs:332-343`，无本群人格时原样返回 seed）
  进入 `engine/mod.rs:1327` / `:1837` 的 `payload.persona`；
- `rust/src/prompts.rs:10`（= `src/prompts.mjs:3`）的 Layer① IDENTITY 还有「兴趣是选题线索，**不是编造经历的许可**」。
- 结果：§14 只改了 `responsibility`（行为规则层），**优先级更高的人设层与 IDENTITY 仍保留被 §14 废弃的"经历线"**，
  §14 想要的放开被上层压掉。

### 修法要求
1. 按 §14 的口径更新**默认 persona** 与 **IDENTITY** 中涉及"不得编造亲身经历"的表述：
   **无害的经历虚构与情绪/状态表达应当放开**；同时必须保留 §14 仍然禁止的部分
   （涉及对方决策或利益、第三方具体事实、被问是不是 AI 时不得冒充真人等——以 §14 原文为准）；
2. 注意 **IDENTITY 属于 Layer①**，按 §1 L105 的口径它**不应承载行为准则**——若你发现这条正是该问题的根源，
   请在回报中说明你如何处置（移动 / 精简 / 保留），**不要**顺手重排整个 IDENTITY；
3. 迁移路径：`LEGACY_PERSONAS` 的迁移目标也要跟着改，避免老配置迁移回旧表述；
4. 提示词有改动 → 必须重新生成并跑 `--check`，同时同步 Node 侧 `test/prompts.test.mjs` 覆盖的生成一致性检查。

### 任务边界
- **允许修改**：`rust/src/defaults.json`、`src/config.mjs`、`rust/src/config.rs`（仅 `LEGACY_PERSONAS` 相关）、
  `src/prompts.mjs` 的 `identity` 常量、`rust/src/prompts.rs`（由生成器产出）、以及本任务新增/更新的测试。
- **禁止修改**：`responsibility` / `boundary` 两个规则的内容（§14 已在此落地且正确）；
  发送/决策/记忆/媒体任何逻辑；`docs/` 下的文档（除在提交信息里说明外）。
- **不得改变的行为**：断言"不冒充真人"仍然成立（被直接问是不是 AI 时不得声称是真人）。

### 测试
- 可用性：`persona` 与 IDENTITY 的新表述确实进入 `payload.persona` 与 system；老配置迁移后不再是旧句；
- **目标测试**：断言 §14 的两组语义**同时**成立——①"无害经历虚构/情绪状态"**不再被禁止**；
  ②§14 仍列为禁止的类别**仍被禁止**。注明依据 §14 L842-856。

---

## 任务 2：`topic` 在 `reply` 通过时永不参与抽签（审计 A5，medium）

### 设计依据
- `docs/working/prompt-and-learning-design.md` **§九 L642-656**：①回复必要性与②发起话题必要性**必须分开判定**，
  「两者的触发条件、冷却周期与失败后果都不同」；L654「只有通过初筛才值得花模型调用」。

### 现状（已复核，代码实证）
```rust
// rust/src/engine/mod.rs:944-953
if screened.reply.is_none() { "message" }
else if screened.topic.is_none() { 概率筛 → "topic" }
else { continue; }
```
- `reply` 通过时**直接返回 `message`** → `topic` 的概率即使已算出也被丢弃 → **"先回复还是先开话题"的选择没有实现**；
- 且每 tick 都跑 `reservoir`/`expectation`/`handled` 查询并算出 probability 后丢弃（无谓查询与日志噪声）。

### 修法要求
1. 让**①与②各自独立判定**：两个闸门分别评估，再按明确口径决定本轮走哪条（口径要写清楚并给理由，
   例如"两者都通过时优先回复、把话题意图保留到下一轮"）；
2. **不得**因此引入"同一 tick 既回复又开话题"的双发；
3. 顺带消除无谓查询：概率只在**确实要用于决策**时计算；
4. 与已有闸门（静默时段、主动冷却/配额、作息）的语义保持一致——注意 `proactive` 现已按**触发路径**判定，
   不要把本任务改成按 hint 判定。

### 任务边界
- **允许修改**：`rust/src/engine/mod.rs`（tick 筛选与 cycle 分派相关区域）、`rust/src/engine/decision.rs`、以及相关测试。
- **禁止修改**：`sending.rs` 的概率因子与 veto 语义；`targeting.rs`；记忆/媒体/transport 任何逻辑；
  提示词与生成物。
- **不得改变的行为**：`proactive` 的触发路径判据、静默/配额/冷却闸门的既有语义。

### 测试
- 可用性：reply 与 topic 两闸门各自的判定与组合；两闸门同时通过时的分派符合你声明的口径；无重复发送；
- **目标测试**：断言"在 `reply` 可通过的情况下，**`topic` 的概率确实参与了决策**"（而不是被无条件丢弃），
  并断言该轮**没有双发**。注明依据 §九 L642-656。

---

## 任务 3：回填未隔离 `db.observe`，历史消息冒充"我在等的回应"（审计 D3，medium）

### 设计依据
- `docs/working/prompt-and-learning-design.md` **§九 L648**：①回复必要性的输入包含
  `expectation`（**我在等的回应**）；L781「有人在等回应……优先回应」。

### 现状（已复核，代码实证）
- `rust/src/engine/mod.rs:423-571` 处理 backfill 时，**只隔离了 `state`（L519-527）与指令分支（L482/L500）**，
  而 **`db.observe(&value, now)`（L551）照常执行**；
- → `store/operations.rs:239-243` 会把 `expectation.observation` 写成
  `{event:"human_message", addressed:<历史消息 hint>, at:now}`（判据只用 expectation 自身的 ts/expires，**与消息自身 ts 无关**）；
- 同文件 `engine/mod.rs:419` 自述意图是 *History contributes to perception **without changing live reply scheduling***，
  也就是说唯一会改变实时回复输入的 `observe` 恰好被漏掉；
- 后果：agent 刚发言、尚未收到回应时，任何一次回填（连接/reconnect/定时）都会把一条**历史**消息登记为"有人回应了"，
  污染 FORECAST 的 `priorExpectation` 与 §九 ① 的 expectation 闸门（`decision.rs:106`）；
  且因 `observation IS NULL` 只填一次，先入库的历史消息必胜。

### 修法要求
1. 让 backfill 路径**不产生 expectation 观测**（与 version/last_human/pending 的既有隔离保持一致）；
2. **保持**回填原有的用途：历史仍应贡献"感知"（如活跃度、记忆候选），只是**不得改变实时回复调度**；
3. 检查是否还有其它"只在实时路径才应生效"的副作用被回填绕过（如同类时间戳敏感写入），一并说明；
4. 不得改变正常实时消息的 observe 行为。

### 任务边界
- **允许修改**：`rust/src/engine/mod.rs`（backfill/ingest 隔离区域）、`rust/src/engine/backfill.rs`、
  `rust/src/store/operations.rs`（仅在确有必要时）、以及相关测试。
- **禁止修改**：记忆分层与学习逻辑、媒体、transport、提示词。
- **不得改变的行为**：实时消息的 `observe` 行为；回填的允许群/去重/时效判定。

### 测试
- 可用性：回填后 `expectation` 不被写入；实时消息仍会写入；回填的其它感知效果保留；
- **目标测试**：构造"agent 刚发言 → 触发一次回填 → 历史消息**不得**被当成'有人回应了'"，
  断言 `priorExpectation` 与 expectation 闸门未被污染。注明依据 §九 L648 / L781 与 `mod.rs:419` 的自述意图。

---

## 任务 4：relay 去重依赖 `short_term`，被无关开关静默关掉（审计 D4-relay，medium-low）

### 设计依据
- `docs/working/prompt-and-learning-design.md` **§21.7 L1237**：
  「**去重**：先查本群 `short_term` 是否已有该链接/内容的记录，有则不发（避免同一条反复刷）」。

### 现状（已复核，代码实证）
- `rust/src/topic/relay/links.rs:57-66` 只读 `memory_layers` 的 `short_term`；
- 而 `short_term` 的**唯一写入点**是 `LayeredMemory::capture`（`rust/src/memory/mod.rs:363`），
  engine 仅在 `a.learning.enabled` 时调用（`rust/src/engine/mod.rs:548-550`）；
- relay 自身开关是 `agent.relay.enabled`（`rust/src/config.rs:960`）——**两者设计上互不隶属**；
- 后果：`relay.enabled=true, learning.enabled=false` 时 `short_term` 恒空 → `is_duplicate` 永远 false →
  同一链接每小时可重复进入候选，正是 L1237 要避免的"反复刷"。

### 修法要求
1. 让 relay 的去重**不依赖学习开关**：去重所需的记录应由 **relay 自己**维护（例如 relay 自己的已发记录表），
   或明确改为读取一个不受 `learning.enabled` 影响的来源；
2. 去重语义保持 §21.7 的意图（同一条内容不要反复刷），并说明**去重窗口**取多长、依据是什么；
3. 不得因此改变 `short_term` 的既有语义与写入时机；
4. 若你判断更合理的做法是"保留读 short_term，但增加一条独立记录"，请说明理由与两者关系（避免出现两份真相）。

### 任务边界
- **允许修改**：`rust/src/topic/relay/links.rs`、`rust/src/topic/relay.rs`、
  `rust/src/store/media.sql` 之外的存储定义需谨慎——**新增表只在确有必要时**，并给出迁移方式；相关测试。
- **禁止修改**：`LayeredMemory`/`capture` 的语义、`learning.enabled` 的行为、媒体与发送逻辑、提示词。
- **不得改变的行为**：relay 的出站外溢防护（只扫 allowedGroups、跳过 current、绝不扫 private）、
  既有的 fail-closed 审核链。

### 测试
- 可用性：relay 去重在 `learning.enabled=false` 时**仍然生效**；重复链接不再重复进候选；
- **目标测试**：断言「`relay.enabled=true, learning.enabled=false` 时，同一条链接**不会被反复推送**」，
  并断言去重窗口的边界行为。注明依据 §21.7 L1237。
