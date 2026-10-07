# qq-inner-agent Rust 内核:架构与移植方案

状态:**v1.0.0 —— 已交付,远端实况运行中**
基准提交:`6978d8b`(JS 版;移植前基线)
目标主机:`100.114.145.17`(Ubuntu / x86_64)

---

## 相关文档

| 文档 | 内容 |
| --- | --- |
| `docs/working/rust-port/SURVEY.md` | 逐模块代码勘察(调用链、表结构、性能热点) |
| `docs/working/rust-port/tasks/phase*.md` | 各阶段的可派发规格(含「本阶段不要实现的设计」禁令) |
| `rust/README.md` | Rust 内核模块表、构建方式、**交叉验证测试必须先让 `node` 进 PATH** 的提醒 |
| `rust/PROVIDER.md` | provider 传输契约,含与 JS `AbortSignal` 的取消语义差异 |
| `rust/ONEBOT.md` | OneBot 传输契约与已知差异 |
| `docs/working/prompt-and-learning-design.md` | 提示词分层、学习分诊、affect 指标、记忆召回下钻等设计;**部分已实施**(主人教学 P6e、三层决策 P6d、groupActivity/注意力漂移 P6b),其余为待实施的 Rust-only 设计。派活时须遵守其中的禁令 |
| `docs/working/human-like-replies.md` | "更像真人"的设计与改动清单 |

## 1. 目标与范围

把**每条消息都会走到的运行时路径**用 Rust 重写,以降低内存占用、消除 GC 停顿、
提高 SQLite 与 JSON 处理的吞吐,并用 tokio 做真正的并发;同时**保留 Node 侧的
仪表盘与 CLI**,避免重写 TLS/HTTP 服务端与前端。

明确的范围划分:

| 归属 | 模块 |
| --- | --- |
| **Rust 内核**(常驻进程) | `config`、`store`、`onebot`、`provider`、`engine`、`policy`、`sending`、`activity`、`orientation`、`memory`、`memory-ranking`、`expression`、`learning`、`prompts`、`diagnostics`(QQ 收发诊断) |
| **Node 保留** | `dashboard.mjs`(HTTP/HTTPS 服务端 + 静态 `web/`)、`cli.mjs` 的交互部分、`settings.mjs`(配置文件读写与 revision)、`scripts/*`(安装脚本) |

> 说明:虽然选的是"只重写性能关键路径",但 `engine.mjs` 的决策循环依赖
> `policy/sending/memory/expression/activity/orientation/prompts/store/provider`,
> 这些都在热路径上。因此实际落地等价于"**除仪表盘与 CLI 之外的全部后端**"。
> 这仍然显著小于全量重写,因为最重的 TLS/会话/前端部分留在 Node。

### 非目标

- 不重写 `web/` 前端框架(仍由 Node 提供静态资源);前端只做个性化改版。
- 不改动 `config.json` / `secrets.json` / `agent.sqlite` 的**格式与语义**。
- 不改动模型提示词输出的 JSON 契约(否则行为会漂移)。

---

## 2. 部署约束(已实测验证)

部署主机**没有** `cmake`、`make`、`pkg-config`,**没有** 系统 sqlite/openssl 头文件,
且**没有免密 sudo**,无法 `apt-get`。因此:

- ✅ `rusqlite` + `libsqlite3-sys`(`bundled`):用 `cc` 从源码编译 SQLite,不依赖 pkg-config。
- ✅ `ureq` + `rustls`:**解析为 `ring` provider**(实测 `Cargo.lock` 中为 `ring`,非 `aws-lc-rs`),不需要 cmake。
- ✅ `tokio-tungstenite`:**不使用 TLS**,因为 OneBot 桥是回环上的明文 `ws://`。
- ⛔ **禁止**引入需要 cmake / make / pkg-config / OpenSSL / 系统 SQLite 的 crate。
  任何新增依赖都必须在部署主机上跑一次干净构建验证。

验证记录:`~/depcheck` 一次性编译通过,耗时 33.8s;`Cargo.lock` 中 TLS 相关包为
`ring` + `rustls`,SQLite 相关为 `rusqlite` + `libsqlite3-sys`。

工具链:远端 `rustc 1.99.0` / `cargo 1.99.0`(含 clippy、rustfmt),经 rustup 安装在 `~/.cargo`。

---

## 3. 进程拓扑

```
                    ┌──────────────────────────────┐
   QQ 客户端 ──────►│ SnowLuma / NapCat (OneBot v11)│
                    └──────────────┬───────────────┘
                          ws://127.0.0.1:3001/
                                   │
                    ┌──────────────▼───────────────┐
                    │ qq-inner-core (Rust,常驻)     │
                    │  · OneBot 传输                │
                    │  · SQLite 存储 + 记忆         │
                    │  · 决策引擎 + 策略            │
                    │  · provider HTTP 调用         │
                    │  · 写 data/status.json        │
                    │  · 监听 data/control.sock     │◄──┐
                    └──────────────────────────────┘   │ NDJSON
                                                       │ Unix socket
                    ┌──────────────────────────────┐   │
                    │ qq-inner-dashboard (Node)    ├───┘
                    │  · HTTP 5097 / HTTPS 5098    │
                    │  · 静态 web/ 前端            │
                    │  · 读 config/secrets/status  │
                    └──────────────────────────────┘
```

服务单元:

- `qq-inner-agent.service` → `rust/target/release/qq-inner-core start`(工作目录 = 仓库根)
- `qq-inner-dashboard.service` → `node src/dashboard.mjs`(保持不变,改为通过控制套接字取实时数据)

`./agent` 启动脚本的映射调整:`start` 调 Rust 内核;`status`/`logs`/`stop`/`restart`
仍走 systemd;`check`/`contacts`/`add-memory` 改为 Rust 内核的子命令(或经套接字);
`setup`/`install-*` 沿用 Python 脚本;`dashboard*` 保持 Node。

---

## 4. 配置与热重载契约(**保持不变**)

保留现有的**基于文件**的配置与重载机制,不改协议:

- 配置来源仍是 `config.json` + `secrets.json`(0600)。
- `settings.mjs` 计算的 **revision 哈希**是 Node 与 Rust 之间的唯一同步信号。
  **Rust 必须逐字节复刻该哈希算法**,否则热重载会失效。
  → Phase 1 的第一件事:读 `src/settings.mjs`,在 Rust 中实现与之完全一致的
  `revision(root)`,并加一个交叉验证测试(Node 与 Rust 对同一目录算出相同哈希)。
- 保存过程中出现的 `.settings-write` 标记文件语义保留:存在即视为"写入进行中,暂不重载"。
- 内核每 1s 比较 revision;变化则重载;重载失败保留上一份并记录 `config_reload_rejected`。
- 仪表盘继续直接写 `config.json`/`secrets.json`(含恢复日志),**不需要**经过控制套接字。

这样切分的好处:配置写入路径完全不动,风险最低。

---

## 5. 控制协议 v1(Rust ↔ Node)

- **传输**:Unix 域套接字 `<repo>/data/control.sock`(目录 0700,天然按用户隔离)。
- **分帧**:一行一个 JSON 对象(NDJSON),单行上限 1 MiB。
- **方向**:客户端请求/响应带 `id`;服务端可主动推事件(无 `id`)。

请求:`{"id":"<string>","method":"<string>","params":{...}}`

响应:`{"id":"...","ok":true,"result":{...}}`
或 `{"id":"...","ok":false,"error":{"code":"...","message":"..."}}`

事件:`{"event":"<name>","data":{...}}`

### 方法(v1)

| 方法 | 参数 | 返回 | 用途 |
| --- | --- | --- | --- |
| `state.get` | — | 实时状态对象(字段与 `status.json` 一致,另加学习条目计数) | 概览页 |
| `models.list` | — | `{models:[string]}` | 加载可用模型 |
| `test.model` | — | `{ok:true, latencyMs, model}` | 测试 API 连接 |
| `contacts.list` | — | `{groups:[{id,name}], friends:[{id,name}]}` | 加载 QQ 联系人 |
| `debug.send` | — | `{ok:true, messageId}` | 发送自检消息 |
| `debug.receive.start` | — | `{listening:true, until}` | 开始接收测试 |
| `debug.receive.status` | — | `{listening:bool, events:[...]}` | 轮询捕获 |
| `debug.receive.stop` | — | `{stopped:true}` | 停止接收 |
| `learning.reset` | `{chat, subject?}` | `{ok:true}` | 重置某 subject 的三层记忆 |
| `learning.list` | `{limit?}` | `{entries:[...]}` | 学到的风格与记忆列表 |
| `logs.tail` | `{lines?}` | `{lines:[string]}` | 运行日志尾部 |

### 事件(v1)

`onebot`(state)、`decision`(chat/action/score/tags/ts)、`send_assessment`、
`message_sent`、`chat_learning_updated`、`config_applied`(revision)、
`cycle_error`(code)。事件内容与现有 `log()` 的字段保持一致。

---

## 6. 模块映射(JS → Rust)

| JS | Rust 目标 | 备注 |
| --- | --- | --- |
| `config.mjs` | `config.rs` | 结构、默认值、校验、`readiness()` 必须完全等价 |
| `store.mjs` | `store/mod.rs` | 表结构/列/索引逐字复刻;全部 SQL 保持语义一致 |
| `onebot.mjs` | `onebot.rs` | 重连退避、心跳、echo 关联、early 事件缓冲、错误码 |
| `provider.mjs` | `provider.rs` | endpoint 构造、重试、退避、错误码映射、预算 |
| `engine.mjs` | `engine/mod.rs` | 决策循环四阶段,顺序与取消语义必须一致 |
| `policy.mjs` | `engine/policy.rs` | allowed/normalize/quiet/select/repeated |
| `sending.mjs` | `engine/sending.rs` | 六因子乘积、预测校验 |
| `activity.mjs` | `engine/activity.rs` | 高斯曲线、块抽样与持久化 |
| `orientation.mjs` | `engine/orientation.rs` | 观察期阈值、ORIENT 请求、闸门 |
| `memory.mjs` | `memory/mod.rs` | 三层记忆、补丁校验、容量淘汰 |
| `memory-ranking.mjs` | `memory/ranking.rs` | 词汇重叠 + 中文双字 + 时效性 |
| `expression.mjs` | `expression.rs` | 表达学习、装饰(emoji/face)选择 |
| `learning.mjs` | `learning.rs` | 学习节奏门控 |
| `prompts.mjs` | `prompts.rs` | 中文提示词;本阶段同时做"更像真人"的优化 |
| `diagnostics.mjs` | `diagnostics.rs` | QQ 收发自检与捕获 |
| `settings.mjs` | `settings.rs`(只读部分) | **revision 哈希必须逐字节一致** |
| `dashboard.mjs` | 保留 Node | 改为控制套接字客户端 |
| `cli.mjs` | 保留 Node / 或 Rust 子命令 | 交互向导留 Node |
| `main.mjs` | `main.rs` | 1s tick、5s status、1s revision 检查 |

---

## 6.1 移植要点与优化机会

详细逐模块规格见 [`SURVEY.md`](SURVEY.md)(19 个模块逐行勘察,含全部默认值、
17 张表 schema、48 步 cycle 调用链、仪表盘路由全表)。以下是必须记住的几条。

### 必须改进的两处算法(而不是 1:1 照搬)

1. **`LayeredMemory.enforce` 是每条消息都跑的全量扫描。**
   现状:`engine.ingest` 对每条人类消息都对该 chat 的**全部 subject × 3 层**做
   SELECT + 裁剪(默认 200 人 × 3 层),每条消息十几次到几十次 SQL + JSON 编解码。
   → Rust 侧改为**增量追加 + 定期批量 enforce**(例如 1h 定时 + 容量阈值触发),
   保持最终状态一致但把每消息成本降到 O(1)。这是本次重写收益最大的地方。

2. **`rankMemories` 存在 O(n²·m) 无缓存 tokenize。**
   贪心循环里每次 `overlap()` 都重新 `tokens()` 两个字符串,没有记忆化。
   → 预计算并缓存 token 集合(或改倒排索引),把评分循环降到 O(n·m) 以下。

### 可以跳过的死代码

- `learning.mjs` 的 `parseLearning`:生产路径**零引用**,仅被 `test/learning.test.mjs` 使用。
- `Store.handled()`:定义但无调用者。
- `learned_memories` 表在运行时几乎不增长(engine 传 `memories: []`),但**表结构仍需保留**
  以兼容旧库。

### schema 来源不要搞错

`config.example.json` **已过期**:缺 `agent.learning` / `observation` / `sending` 三整块,
`agent.memory` 只有 4 个键,`provider.thinking` 也与默认值不符。
**真正的 schema 是 `src/config.mjs` 的 `defaults` + `validate`**(见 `SURVEY.md` 第 4 节)。
移植完成后应顺手把 `config.example.json` 与真实默认值对齐。

### 不可变更的契约(回归红线)

- `status.json` 字段名与语义;`agent.sqlite` 表/列结构与 JSON 编码;
  时间列一律为**秒(REAL)**,不是毫秒。
- `revision = sha256(config.json + '\0' + secrets.json)` 与 `.settings-write` 暂停重载语义。
- 全部错误码字符串(`waiting_for_setup`、`http_401_check_provider_config`、
  `invalid_formation`、`invalid_ratings`、`invalid_articulation`、
  `output_truncated_increase_maxTokens`、`hourly_api_budget`、`qq_offline`、
  `delivery_uncertain` …)——前端 `translate()` 依赖字面量。
- 模型提示词输出的 **JSON 字段契约**(改了会导致记忆/评分校验整批拒绝)。
- 文件权限:`data/` 0700、`secrets.json`/`status.json`/`dashboard-access.txt` 0600。

### 灰度策略

Node 侧先**双读**:控制套接字可用则订阅事件,否则回退到 `status.json` + 只读 SQLite。
这样 Rust 内核可以灰度上线而不影响现有仪表盘,出问题可立即把
`qq-inner-agent.service` 的 `ExecStart` 切回 Node。

## 7. 分期计划

状态随实际推进更新。全部阶段已完成(除 P11 收尾与 P10 的性能对比留待后续)。

| 阶段 | 内容 | 验收 | 状态 |
| --- | --- | --- | --- |
| **P0** | 骨架、依赖验证、架构文档、代码勘察 | 本机与远端均能 `cargo build`;依赖集在**无 cmake/make/pkg-config/免密 sudo** 的主机上可编译 | ✅ 完成 |
| **P1** | `settings`(revision / atomicJson)+ `config`(defaults / merge / validate / loadConfig / readiness) | Node↔Rust 对同一 fixture 的 defaults / loadConfig / validate / revision **逐字相等** | ✅ 已合并；macOS 与远端 Linux 各 55 项测试通过（含真实 Node 交叉验证） |
| **P1b** | **`store`**:17 张表 schema、全部查询、保留期清理、`callBudget` 滚动限流 | 与 JS 打开同一库时表结构一致;查询与保留期行为对齐 | ✅ 已合并（`schema.sql` 17 张表逐字一致 + 24/24 方法 + 共库双向读写测试；双平台 64 项测试通过） |
| **P2** | `onebot` 传输 | 对本地 WS mock 跑通鉴权/心跳/重连/early 缓冲/echo 关联/发送格式 | ✅ 已合并（`onebot.rs` 503 行 + mock 测试 405 行；双平台验证） |
| **P3** | `provider` 适配 | 对本地 HTTP mock 跑通两种 API 格式、重试退避与全部错误码 | ✅ 已合并（纯逻辑 + HTTP 传输 + 重试退避 + 小时预算；10 个传输层测试；双平台 74 项通过） |
| **P4** | `memory` + `ranking` + `expression`(跳过死代码 `learning.mjs`) | 隔离性/淘汰/过期行为对齐;并落地两项优化:**enforce 改增量+批处理**、**ranking 预计算 token 缓存** | ✅ 已合并（分层记忆/排序/表达 + 增量批处理维护；`memory_parity` 870 行 + JS oracle 与 Node↔Rust 基准；双平台 89 项通过） |
| **P5** | `policy` + `sending` + `activity` + `orientation` | 纯函数逐一比对;时区/夏令时用 `chrono-tz` 精确匹配 JS `Intl` | ✅ 已合并（`normalize` + activity 持久化 + orientation；`phase5_parity` 587 行/86 断言 + JS oracle；双平台 96 项通过） |
| **P5b** | 群内装饰素材:采集、落盘、去重、上下文索引、交流阶段分类 | 同一张图只落一份;失败不写记录;分类器对模糊输入如实返回不确定 | ✅ 已合并（采集/即时落盘/哈希去重/上下文 id 引用/两轴分类/**来源可公开性**；109 项测试，默认档位为 `unknown`） |
| **P6a** | 引擎核心(48 步 cycle + golden 逐条比对 + 取消语义) | 与 JS 决策序列逐条一致;先落库后发送 | ✅ 已合并,120 项测试 |
| **P6-runtime** | `main.rs`:status.json 原子写、revision 监视、热重载共用 store、信号停机 | status 字段与 JS 对齐;`.settings-write` 暂停重载 | ✅ 已合并,129 项测试 |
| **P6b** | 装饰素材的选择与发送:`groupActivity`、群作息、按场合适配度、概率触发、信号剔除 | 收束/单发后的沉默**不得**记为负面;温度不足不选;检索无匹配不发 | ✅ 已合并（`media_select.rs` 600 行 + 引擎门控接线；来源闸门/跨群共享/注意力漂移；140 项测试） |
| **P6c** | 人类化行为:长度分档接线、按群表情频率、只发表情、多气泡/打字延迟 | 开关化默认关闭;被点名不得给 `tiny` | ✅ 已合并（长度分档 parity + 表情频率/只发表情门控 + 多气泡/打字延迟(门控,抖动随心情强度)；154 项测试） |
| **P6d** | 三层决策:①/②拆分 + 零模型初筛 + `groupActivity` + 群作息 | ①②独立判定;初筛不调模型 | ✅ 已合并（decision.rs 三层决策 + 显式零模型初筛；开关默认关闭与 JS 逐条一致；161 项测试） |
| **P6e** | 主人教学指令:`/记住` `/黑话` `/忘记` | 仅 ownerUin + 私聊;绕过门控不绕过校验;来源不伪造 | ✅ 已合并（`owner_teaching.rs` + 异步确认队列;158 项测试） |
| **P7** | 控制套接字 + Node 仪表盘瘦客户端化 | 仪表盘全部页面在 Rust 内核下可用;Node 侧双读灰度 | ✅ 已合并（P7a Rust 控制服务端 + P7b Node 控制客户端与仪表盘双读回退；Node 110/111 + Rust 146） |
| **P8** | 个性化前端改版 | 视觉与交互改版,功能不回归 | ✅ 已交付(第一轮精修,视觉验证通过) |
| **P9** | 更像真人的提示词与行为 | 对比样本 + 可判定指标(长度分布、句式黑名单命中率) | ✅ JS 版已交付；Rust 侧由 `tools/gen-prompts.mjs` 从 JS 同步，并有逐字比对 |
| **P10** | 实况验证、切换与回退 | 在远端跑通;含性能对比(内存 / CPU / 延迟) | ✅ 实况切换完成(Rust 内核 systemd 常驻、连 OneBot、模型调用与发送/控制接口全通)；性能对比留待后续 |
| **P11** | 冻结 JS 参照值为黄金文件、翻转过渡开关、把 parity 测试**重写为 Rust 原生测试**、移除对 `node` 的依赖 | `node` 移出 PATH 时**不跳过任何测试**;每条测试可追溯到设计文档的某条规则 | ✅ 已合并（黄金文件 + node 零跳过） |

| **P6f** | 提示词分层 + 虚构责任线 | 三层提示词逐字对齐;责任线红线不丢失 | ✅ 已合并 |
| **P6g** | 学习分诊 learn/partial/skip + 落库前自我审核 | verdict 解析 / partial 升格 / 自审 keep-drop-rewrite | ✅ 已合并 |
| **P6h** | affect 指标 + 二维心情 disposition + 记忆召回下钻 recall | decay / 单向约束 / 熔断 / 四象限 + recall | ✅ 已合并 |
| **P6i** | §21 新话题来源(外部内容 + 群间转发 + forward 收发 + 安全) | 相关度 / 节流 / 预算 / 缓存 + 分级门控 + forward 收发 | ✅ 已合并 |

依赖顺序:P1 → P1b → {P2, P3} → P4 → P5 → P6 → P7 → P10。
P8/P9 与主线解耦,可并行。

---

## 8. 测试策略

1. **以黄金文件为契约**:parity 测试的期望值冻结在 `rust/tests/golden/`,不依赖 node。
2. **交叉验证**:对同一份 SQLite 与同一份输入,Node 与 Rust 的决策输出必须一致
   (可做 golden-file 对比)。
3. **Mock 依赖**:OneBot 用本地 WS mock,provider 用本地 HTTP mock,不产生真实调用。
4. **不可回归项**:提示词 JSON 契约、错误码字符串、配置键名、表结构、revision 哈希。
5. **实况验收**:在远端用真实 SnowLuma 桥 + 真实 DeepSeek key 做一次端到端验证。

---

## 9. 部署与回退

- 构建:远端 `~/.cargo/bin/cargo build --release`,产物 `rust/target/release/qq-inner-core`。
- 切换:改 `qq-inner-agent.service` 的 `ExecStart` 指向 Rust 二进制。
- **回退**:旧 Node 内核入口已归档到 `js-legacy` 分支(不再更新);需要时从该分支签出旧实现。
  main 分支以 Rust 内核为唯一实现。
- 因此 **P1 起就必须保证数据库 schema 与文件格式的双向兼容**,不允许破坏性迁移。

---

## 10. 风险与未决点

1. **revision 哈希一致性**:必须逐字节复刻 `settings.mjs`,否则热重载静默失效。→ P0/P1 交叉验证。
2. **决策等价性**:`engine.mjs` 有大量边界条件(取消、版本号、冷却、配额),
   移植偏差会造成"行为漂移"。→ 用 golden-file 对比 + 逐分支核对。
3. **提示词契约**:模型返回的 JSON 字段一旦改变,`parseMemoryUpdates`/评分校验会拒绝。
   → P8 优化提示词时保持字段不变,只改措辞与风格指令。
4. **并发模型差异**:Node 是单线程事件循环,Rust 是 tokio 多任务。
   `busy`/`version`/`running` 的互斥语义需要显式映射为 Rust 的所有权与锁。→ 逐处核对。
5. **时区与夏令时**:`activity` 的高斯曲线依赖 IANA 时区,需用 `chrono-tz` 精确对齐。
6. **仪表盘瘦客户端化**:需要把 `snapshot()`、`learning reset/list`、诊断等从进程内调用
   改为套接字调用,注意超时与断线处理。
