# Rust 内核（qq-inner-core）

这是 qq-inner-agent 的 Rust 内核：把**每条消息都会走到的运行时路径**从 Node 重写过来，
Node 侧只保留仪表盘与前端。

## 模块

| 文件 | 对应 JS | 说明 |
| --- | --- | --- |
| `config.rs` | `config.mjs` | 默认值、深合并、校验、归一化、env 优先级、readiness |
| `settings.rs` | `settings.mjs` | revision 哈希、原子 JSON 写入（0600） |
| `text.rs` | `store.mjs` 的 `terms`/`similarity` | 词元与相似度 |
| `policy.rs` | `policy.mjs` | 准入、静默时段、活跃时间表、候选选择、长度分档、重复检测、两步时区 |
| `sending.rs` | `sending.mjs` | 预测校验与准入概率 |
| `prompts.rs` | `prompts.mjs` + `orientation.mjs` | **由脚本生成**，勿手改 |
| `activity.rs` | `activity.mjs` | 高斯形静默曲线 |
| `provider.rs` | `provider.mjs` | 端点构造、`parseObject`、状态码分类、正文提取 |
| `onebot.rs` | `onebot.mjs` | OneBot v11 正向 WebSocket 传输 |

## 构建与测试

```sh
cargo build --release
cargo clippy --all-targets -- -D warnings
cargo test
```

## 分模块行为测试

改哪个模块就跑哪个模块的测试，只在合并前跑一次全量。例如在本目录运行
`cargo test --test engine_parity` 或 `cargo test --lib settings::tests`。
迁移期 golden、逐值 parity 及专用辅助已删除；`engine_parity` 保留名称与行为断言。
完整套件清单、Node 测试和合并前命令见[开发文档](../docs/DEVELOPMENT.md#测试在哪)。
Rust 测试无需 Node；提示词生成一致性在 Node 测试中检查。

## 提示词由脚本生成

`src/prompts.rs` 由 `rust/tools/gen-prompts.mjs` 从 JS 侧生成，避免手抄引入不可见的字符差异。
以下命令均在仓库根目录执行：

1. 先改 `src/prompts.mjs`（或 `src/orientation.mjs`）；
2. 运行 `node rust/tools/gen-prompts.mjs`；
3. 跑 `node rust/tools/gen-prompts.mjs --check` 确认生成物一致（只检查，不写文件）。

## 依赖约束

部署主机上**没有** `cmake`、`make`、`pkg-config`，也没有系统 SQLite/OpenSSL 头文件，
并且**没有免密 sudo**。因此：

- SQLite 走 `rusqlite` 的 `bundled`（用 `cc` 从源码编译）；
- TLS 走 `ureq`/`rustls`，实测解析到 **`ring`**（不是需要 cmake 的 `aws-lc-rs`）；
- OneBot 是回环上的明文 `ws://`，`tokio-tungstenite` **不使用 TLS**。

**新增任何依赖前，必须在部署主机上跑一次干净构建验证。**
