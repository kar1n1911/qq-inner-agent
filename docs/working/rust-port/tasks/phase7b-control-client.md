# P7b Node 控制客户端 + 仪表盘双读回退

你是 qq-inner-agent 的工程师。工作区是隔离的 git worktree。

## 定位

**唯一允许改 Node 侧(`src/`)的阶段**。P7a 已把 Rust 控制服务端(`control.rs`,Unix socket + NDJSON)合并,
本阶段写 Node 客户端,并让仪表盘在"控制通道可用/不可用"之间**双读**。

协议权威定义:`docs/working/rust-port/ARCHITECTURE.md` 第 5 节。

## 必读

- `docs/working/rust-port/ARCHITECTURE.md` 第 5、6.1 节;
- `docs/working/rust-port/SURVEY.md` 第 5 节(仪表盘全部路由与 `snapshot()` 字段);
- `src/dashboard.mjs`、`src/settings.mjs`、`src/cli.mjs`、`src/store.mjs`;
- 已合并的 `rust/src/control.rs`(服务端行为)。

## 范围

### 1. `src/control.mjs`(新增,只用内置模块)

- 连接 `<dataDir>/control.sock`;
- NDJSON 分帧(单行上限 1 MiB)、按 `id` 关联请求、超时;
- 断线重连 + 指数退避;事件订阅;
- **`available` 标志**:首次成功连接后置位,断线后复位 —— Node 侧据此决定是否回退。

### 2. `src/dashboard.mjs` 双读

- socket 可用 → 用 `state.get` / `learning.list` / 诊断方法;
- socket 不可用 → **回退到现有逻辑**(读 `data/status.json` + 只读 SQLite);
- **HTTP 路由与响应体形状完全不变**(前端零改动);
- **回退路径必须始终可用** —— 仪表盘绝不能因控制通道不可用而挂掉。

### 3. `agent` 启动脚本

- `start` 指向 Rust 内核;`check`/`contacts`/`add-memory` 走 Rust 二进制子命令;
  `setup`/`install-*` 仍用 Python;`dashboard*` 仍用 Node;
- 新增 `./agent core-status`(或等价)查看控制套接字是否可用。

## 硬性约束

1. Node 侧**只用内置模块**,不得引入 npm 依赖;
2. **不得改 `rust/`**;
3. 中文注释,重点标注:分帧边界、慢读、断线重连、"回退路径必须始终可用"。

## 验收

1. Node:`node --test test/*.test.mjs` 全过(允许既有的 macOS `127.0.0.2` 平台失败);
   Rust:`cargo test` 仍全过(本阶段不改 rust/,但必须回归确认);
2. Node 测试(用 mock Unix socket 服务端):
   - 请求/响应关联、超时、事件分发、断线重连;
   - **socket 不存在时 `available === false`,且仪表盘走回退路径仍能返回 `state`**;
   - **`test/dashboard.test.mjs` 现有测试必须全部继续通过**(不回归底线)。

## 交付

1. `git add -A && git commit`,英文。
2. 一段话回报:实现了什么、双读回退怎么验证的、与设计的偏差或不确定点。
