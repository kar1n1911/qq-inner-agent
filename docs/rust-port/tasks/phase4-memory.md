你是 qq-inner-agent 的 Rust 移植工程师。当前工作区是**隔离的 git worktree**,基线是 P1b 之后的
`main`。改动不影响主分支。

## 前置

`rust/src/` 下已有 `config.rs`、`settings.rs`、`onebot.rs`、`provider.rs`、`store.rs`。
先读它们,复用错误类型、JSON 处理与测试组织方式,并把新表/新查询接到 `store.rs` 已有结构上。

## 必读

- `docs/rust-port/ARCHITECTURE.md`(第 6.1 节列出本阶段必须落地的两项性能优化)
- `docs/rust-port/SURVEY.md` —— **第 8.7、8.8 节**是记忆与表达的算法要点,第 3 节是相关表结构
- `src/memory.mjs`、`src/memory-ranking.mjs`、`src/expression.mjs`(逐行读完,唯一真源)

## 任务:P4 —— 记忆 / 排序 / 表达

实现 `memory.rs`(或 `memory/` 子模块)、`ranking.rs`、`expression.rs`。

### 1. `memory.rs`

- `memory_subjects(chat, sender)` → 群聊返回 `["group", "person:<id>"]`,私聊只返回
  `["person:<id>"]`;非法 scope 报 `invalid_memory_scope`(**错误码逐字一致**)。
- `parse_memory_updates(...)`:完整复刻校验规则 —— 最多 4 条;`subject`/`layer`/`operation`
  白名单;`sourceIds` 1..6 且**必须命中真实人类消息**;**group 条目需 ≥2 个不同 sender**,
  **person 条目必须全部来自该人**;任一不符则**整批拒绝**。
- 三层结构:`short_term`(默认 72h / 40 条 / 每条 1000 字符)、`long_term`(365 天 / 1800 字符 /
  最多 24 项)、`traits`(180 天 / 900 字符 / 最多 24 项)。
- `capture`:群聊要写**两条**(群视图 + person 视图,同一句带署名原话);
  `short()` 反向遍历使 person 优先。
- `put()` 的「**不复活 / 不降级**」规则要逐条对齐:更旧的证据丢弃;**同一证据不刷新
  `updated`/`expires`**;文本变化时**先归档 revision** 再改写(`memory_revisions` 表)。
- 容量压力下的淘汰:重要性高的优先保留,最近更新的在并列时胜出。

### 2. ⚡ 性能改进 1(必须做,不要 1:1 照搬 JS)

JS 的 `LayeredMemory.enforce` **在每条人类消息上都对该 chat 的全部 subject × 全部层做全量扫描**
(默认 200 人 × 3 层,十几次到几十次 SQL + JSON 编解码)。这是整个程序最大的热点。

Rust 侧改为:
- **入站路径只做增量追加**(O(1)),不做全量裁剪;
- 全量 `enforce` 改为**定期批量**执行(与 `configure` 共用同一实现),并在**容量超阈值**时触发;
- 必须保证最终状态与 JS 的 `enforce` **收敛到同一结果**(用测试证明:同一串操作后,
  增量+批量路径与一次性 enforce 得到的行集合一致)。

### 3. `ranking.rs`

- `tokens(text)`:汉字取**逐字二元组**,非汉字取长度>1 的词;与 JS 的
  `\p{Script=Han}` / `[\p{L}\p{N}]{2,}` 语义对齐(注意 Unicode 类别,不要用 ASCII 近似)。
- `rank_memories(...)`:BM25 式词汇分 + 半衰期时效性 + **RRF 融合**
  (lexical 权重 3 / recency 1 / importance 1 / confidence 1),owner_note **+0.04**,
  同 subject overlap **惩罚 0.08**;`requireMatch` 在 `retrieve_scoped` 为 true、
  在 `context` 为 false。

### 4. ⚡ 性能改进 2(必须做)

JS 的贪心循环里每次 `overlap()` 都**重新 `tokens()` 两个字符串**(O(n²·m))。Rust 侧必须:
- **预计算并缓存**候选与查询的 token 集合(或建倒排索引);
- 让评分循环不再重复分词。用基准或断言证明没有重复分词。

### 5. `expression.rs`

- `parse_expressions(...)`:只学 `jargon` / `expression`;**`jargon` 的 `term` 必须出现在每条证据中**;
  **`expression` 的 `example` 必须出现在每条证据中**(强校验,不要放宽)。
- `ExpressionMemory`:`apply` / `context` / `used` / `prune` / `reset`;
  使用时要求 `sources ≥ 2`,且群条目需 ≥2 个不同 sender;`reuseSeconds` 冷却。
- `personality_context` / `decoration_choices` / `decorate`:
  装饰三重闸门(开关 / 每 chat 冷却 / 概率);`decorate` 只允许白名单内的 emoji 或 faceId;
  **emoji 按码点计数(占 1 个字符预算)**,与 JS 的 `[...str]` 一致。

### 6. 跳过死代码

`src/learning.mjs` 的 `parseLearning` 在生产路径零引用,**不要实现**;`store.handled()` 也无调用者。

## 硬性约束

1. **不得引入新依赖。**
2. **不得修改 `rust/` 以外的任何文件。**
3. 中文注释,重点标注:隔离性边界(同一个人在不同群/私聊的记录必须分开)、
   `sourceIds` 归属校验、revision 归档时机、Unicode 分词差异、码点 vs 字节。

## 验收标准

1. `cargo build --release` 成功;`cargo clippy --all-targets -- -D warnings` 零警告;`cargo test` 全过。
2. 必须覆盖(可参考 `test/memory.test.mjs`、`test/expression.test.mjs`、`test/learning.test.mjs` 的断言意涵):
   - QQ-ID 归属、个人/群/私聊三层隔离、多人证据要求、错误来源被整批拒绝
   - 短期去重与过期、长期选择性修订/遗忘/容量淘汰、三层独立保留期
   - `put()` 的不复活/不降级四处早返回
   - **增量+批量 enforce 与一次性 enforce 结果一致**
   - **ranking 无重复分词**(可用计数断言)
   - 表达的字面出现强校验、复用冷却、装饰白名单与码点预算
3. 不访问真实 `data/`,测试用内存库或 `rust/target/` 下的临时库。

## 交付

1. `git add -A && git commit`,英文 commit message。
2. 用**一段话**回报:实现了什么、测试怎么跑、两项性能改进的实测效果、与 JS 的已知差异或不确定点。

---

## ⚠️ 补充:本阶段还要补上被 P1b 延后的 store 方法

P1b(store)因为 5 小时额度限制,**有意只做了 schema 与核心查询**,把下面这几个
分层记忆相关的方法留到了本阶段:

- `Store::retrieve_scoped(chat, sender, query, now, settings, opts)`
  —— 按 subject 范围检索短期/长期细节(对应 `store.mjs` 的同名方法)
- `Store::learn(chat, update, now, last_id, settings, epoch, layered)`
  —— 必须在 **BEGIN IMMEDIATE 事务**内落库并归档 revision
- `Store::reset_learning(chat, now, subject?)`
  —— 只清该 subject 的三层,保留原始聊天历史与其他 subject

动手前先确认已合并的 `store.rs` 里确实没有它们(有就复用,不要重复实现)。
返回结构要与 JS 一致;注意 P1b 已确定:Rust 侧这些方法返回**解析后的结构**,
而不是 JS 的原始 JSON 文本 —— 差异要在测试里显式断言并写进注释。

---

## ⛔ 本阶段**不要**实现的设计(重要)

`docs/prompt-and-learning-design.md` 里记录了几项**已设计但尚未实施**的改动。
本项目的铁律是**与现行 JS 行为逐字对齐**,所以在本阶段:

- **不要**实现提示词分层重构(把身份/背景与任务契约、行为准则拆开);
- **不要**实现学习分诊(`learn` / `partial` / `skip` 三档与自动升格);
- **不要**实现 affect 指标(心情值 / 好感度 / 认同度,二维心情与四象限 disposition);
- **不要**实现记忆召回下钻(`recall` 字段、回查原始历史与语境)。

**一律以 `src/*.mjs` 的现行实现为准**;遇到"设计文档说要改、但 JS 还没改"的地方,
按 JS 现在的行为移植,并在回报里提一句你注意到了这处设计。

这些改动会在 JS 侧先行落地后,再单独派发移植任务。

