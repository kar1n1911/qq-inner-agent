你是 qq-inner-agent 的 Rust 移植工程师。当前工作区是**隔离的 git worktree**,基线是 P6 之后的
`main`。改动**会影响 Node 侧**,这是唯一一个允许改 `src/` 的阶段。

## 必读

- `docs/working/rust-port/ARCHITECTURE.md` —— **第 5 节是控制协议 v1 的权威定义**,第 6.1 节是不可变更契约
- `docs/working/rust-port/SURVEY.md` —— 第 5 节是仪表盘全部路由与 `snapshot()` 字段
- `src/dashboard.mjs`、`src/settings.mjs`、`src/cli.mjs`

## 任务:P7 —— 控制套接字 + 仪表盘瘦客户端化

### 1. Rust:`control.rs`

- 监听 `<dataDir>/control.sock`(目录已是 0700,天然按用户隔离);启动时若存在旧 socket 文件先清理
- **NDJSON 分帧**,单行上限 1 MiB;请求/响应带 `id`,事件不带
  - 请求 `{"id","method","params"}` → 成功 `{"id","ok":true,"result"}` / 失败 `{"id","ok":false,"error":{"code","message"}}`
  - 事件 `{"event","data"}`
- 实现 ARCHITECTURE 第 5 节的方法表:`state.get`、`models.list`、`test.model`、`contacts.list`、
  `debug.send`、`debug.receive.start|status|stop`、`learning.reset`、`learning.list`、`logs.tail`
- 事件:`onebot`、`decision`、`send_assessment`、`message_sent`、`chat_learning_updated`、
  `config_applied`、`cycle_error` —— **字段与现有 `log()` 保持一致**
- 多客户端:允许仪表盘与 CLI 同时连接;慢客户端不得阻塞引擎(每个连接一个写任务 + 有界队列,
  队列满时丢弃**事件**而不是断开)
- 引擎停止时干净关闭 socket 文件

**注意**:`config.json`/`secrets.json` 的读写**仍然由 Node 侧负责**(含恢复日志与 `.settings-write`),
Rust 只做 1 秒 revision 轮询。不要新增 `config.set`。

### 2. Node:`src/control.mjs` + 仪表盘改造

- 新增一个控制客户端模块:连接、NDJSON 分帧、按 `id` 关联请求、超时、断线重连与退避、
  事件订阅、`available` 标志
- `dashboard.mjs` 改为**双读**:
  - socket 可用 → 用 `state.get` / `learning.list` / 诊断方法;
  - socket 不可用 → **回退到现有逻辑**(读 `data/status.json` + 只读 SQLite),
    仪表盘在任何情况下都不能因此不可用
- 保持 **HTTP 路由与响应体形状完全不变**(前端不做任何改动即可继续工作)

### 3. `agent` 启动脚本

- `start` 指向 Rust 内核;`check`/`contacts`/`add-memory` 走 Rust 二进制子命令;
  `setup`/`install-*` 仍用 Python;`dashboard*` 仍用 Node
- 新增 `./agent core-status`(或等价命令)用于查看控制套接字是否可用

## 硬性约束

1. **不得引入新依赖**(Node 侧只能用内置模块)。
2. 除 `src/control.mjs`、`src/dashboard.mjs`、`agent` 外,不要改动其他 `src/` 文件。
3. 中文注释,重点标注:分帧边界处理、慢客户端背压策略、断线与重连、以及"回退路径必须始终可用"。

## 验收标准

1. `cargo build --release`;`cargo clippy --all-targets -- -D warnings`;`cargo test` 全过;
   Node 侧 `node --test test/*.test.mjs` 全过(允许既有的 macOS `127.0.0.2` 平台失败)。
2. Rust 侧测试:分帧(半包/粘包/超长行)、`id` 关联、未知方法、多客户端并发、
   慢客户端不阻塞、socket 清理。
3. Node 侧测试(用 mock Unix socket 服务端):
   - 请求/响应关联、超时、事件分发、断线重连
   - **socket 不存在时 `available === false`,且仪表盘走回退路径仍能返回 `state`**
   - **仪表盘现有测试必须全部继续通过**(`test/dashboard.test.mjs` 是不回归的底线)

## 交付

1. `git add -A && git commit`,英文 commit message。
2. 用**一段话**回报:实现了什么、测试怎么跑、双读回退如何验证、与 JS 的已知差异或不确定点。

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

