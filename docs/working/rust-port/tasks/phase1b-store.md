你是 qq-inner-agent 的 Rust 移植工程师。当前工作区是**隔离的 git worktree**,基线是 Phase 2
之后的 `main`。改动不影响主分支。

## 前置

`rust/src/config.rs`、`settings.rs`、`onebot.rs` 已存在,请先读,复用其错误类型与测试组织方式。

## 必读

- `docs/working/rust-port/ARCHITECTURE.md`(第 4/6 节:不可变更契约与模块映射)
- `docs/working/rust-port/SURVEY.md` —— **第 3 节是 17 张表的完整 schema 与保留期规则**,第 9 节有性能热点结论
- `src/store.mjs`(唯一真源,**逐行读完**)

## 任务:P1b —— SQLite 存储层

在 `rust/src/store.rs`(文件较大时可拆 `store/` 子模块)中实现 `Store`,并接入 `main.rs`
(新增子命令 `db-schema`,打印实际表结构,便于与 JS 对比)。

### 1. 打开与 PRAGMA(必须与 JS 一致)

- `journal_mode = WAL`
- `busy_timeout = 5000`
- **必须能直接打开 JS 版创建的 `agent.sqlite`**,并与其读写同一份数据(双向兼容)。

### 2. Schema:集中在一处创建

JS 把建表分散在 5 个模块里;Rust 侧请**集中在一处**建全部 17 张表,但**语义必须逐字对齐**:

`messages`、`thoughts`、`notes`、`decisions`、`deliveries`、`calls`、`send_assessments`、
`expectations`、`handled`、`chat_learning`、`learned_memories`、`memory_layers`、
`memory_revisions`、`expressions`、`decoration_usage`、`activity_rhythm`、`group_orientation`

- 列名、类型、主键、`UNIQUE`、`CHECK(id=1)`、`DEFAULT` 全部照抄(见 SURVEY 第 3 节)。
- 索引:`learned_memories(chat,expires)`、`messages(chat,ts)`、`deliveries(chat,ts)`、
  `thoughts(chat,created)`、`memory_layers(chat,subject,layer,expires)`。
- 触发器 `memory_revision_cleanup`(AFTER DELETE ON memory_layers)。
- **两处 ALTER 迁移探测要保留**:`thoughts` 增 `subject`、`memory_layers` 增 `keywords`/`confidence`
  (对已存在的旧库用 `PRAGMA table_info` 探测后再加,不能直接 `ALTER` 撞错)。
- **所有时间列是秒(REAL),不是毫秒。**
- JSON 以 TEXT 存储的列:`decisions.tags`、`send_assessments.details`、`expectations.forecast/observation`、
  `chat_learning.sources`、`learned_memories.sources`、`memory_layers.sources/keywords`、
  `expressions.sources`、`group_orientation.sources/analysis` —— 编解码格式需与 JS 一致。
- `activity_rhythm` 只保留 `id=1` 的单行。

### 3. 方法(逐一实现,签名自定但语义必须对齐)

`recover_deliveries`、`message`(INSERT OR IGNORE 去重,返回是否新插入)、
`history(chat, limit)`(DESC 查后 reverse,时间升序)、`learning_state`、
`retrieve_scoped`、`learn`(**BEGIN IMMEDIATE 事务**)、`reset_learning(chat, subject?)`、
`note`、`reservoir`、`add_thought`、`score`、`use`、`decision`、**`call_budget(now, max)`**、
`delivery`、`finish_delivery`、`counts`、`mark_handled`、`assessment`、`sending_timing`、
`assess`、`assessment_status`、`expect`、`observe`、`expectation`、`active_chats`、`prune`。

要点:
- **`call_budget` 同时做清理与准入**(滚动 3600 秒,非固定窗口、非 token bucket);
  **未通过时不插入记录**。
- `recover_deliveries`:启动时把所有 `pending` 置为 `uncertain`,**永不自动重发**。
- `history` 的 limit 语义、`reservoir` 的 ttl/limit/subject 过滤、`counts` 的
  `{total, proactive, last}` 语义都要与 JS 一致。
- `tags`/`details` 等 JSON 字段读出来要是**已解析的结构**,与 JS 返回形态一致。
- `prune` 的九类清理(见 SURVEY 第 3 节末尾),保留期单位是天,但比较用秒。

### 4. 性能要求(本阶段的重点之一)

- 全部查询用**预编译语句缓存**(`rusqlite::CachedStatement` 或自行缓存 `prepare` 结果),
  避免每次 `prepare` 的开销。
- **批量写用事务**:同一批到达的多条消息应能合并进一个事务(提供批量入口供 engine 使用)。
- `rowid NOT IN (SELECT rowid ... LIMIT ?)` 这类语句请**显式写出 `rowid`**,不要依赖隐式行为。

## 硬性约束

1. **不得引入新依赖**(`rusqlite` 已在,`bundled` 特性已启用)。
2. **不得修改 `rust/` 以外的任何文件。**
3. 中文注释,重点标注:JSON 编解码格式、时间单位、`INSERT OR REPLACE` 与 `INSERT OR IGNORE` 的区别、
   触发器与 ALTER 探测、NULL 与缺失键的差异。

## 验收标准

1. `cargo build --release` 成功;`cargo clippy --all-targets -- -D warnings` 零警告;`cargo test` 全过。
2. **必须有一个"与 JS 共库"的测试**:在测试内调用 `node` 运行 `src/store.mjs` 的 `Store`
   创建一个临时 `agent.sqlite` 并写入若干数据;然后用 Rust `Store` 打开**同一文件**:
   - 断言**表/列/索引集合完全一致**(用 `PRAGMA table_info` / `index_list` / `sqlite_master` 比对)
   - 断言 Rust 能正确读出 JS 写入的行(含 JSON 字段)
   - 断言 Rust 写入后,JS 也能正确读出
   `node` 不可用时**跳过而非失败**。
3. 每个方法都要有单元测试,覆盖边界:空库、重复 `message`、`call_budget` 刚好达上限、
   `recover_deliveries`、过期清理、`reservoir` 的 ttl 与 subject 隔离。
4. 不访问真实 `data/` 目录。

## 交付

1. `git add -A && git commit`,英文 commit message。
2. 用**一段话**回报:实现了什么、测试怎么跑、与 JS 的已知差异或不确定点。
