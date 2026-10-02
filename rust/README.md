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

## ⚠️ 让交叉验证测试真正运行

多个测试会把 Rust 的实现与**真实的 JS 实现**比对（`config_parity`、`policy_parity`、
`prompts_parity`、`provider_parity`、`activity_parity`、`sending_parity`）。
它们都会调用 `node`，**`node` 不在 PATH 时会静默跳过** —— 那会变成"假绿灯"。

本仓库自带 node，请把它加进 PATH 再跑测试：

```sh
export PATH="$PWD/.runtime:$PATH"   # 从仓库根目录执行
cd rust && cargo test
```

在部署主机上同样如此，可以这样一次跑完：

```sh
cd ~/github_repo/qq-inner-agent
export PATH="$PWD/.runtime:$PATH"
(cd rust && cargo build --release && cargo clippy --all-targets -- -D warnings && cargo test)
```

## 提示词由脚本生成

`src/prompts.rs` 由 `rust/tools/gen-prompts.mjs` 从 JS 侧生成，避免手抄引入不可见的字符差异。
改动提示词的流程是：

1. 先改 `src/prompts.mjs`（或 `src/orientation.mjs`）；
2. 运行 `node rust/tools/gen-prompts.mjs`；
3. 跑 `cargo test --test prompts_parity` 确认逐字一致。

## 浮点比对的两条经验

- **纯 `+ - * /` 可以逐位对齐**（`sending_parity` 就是这么做断言的）；
- **含 `exp` 等超越函数的不行** —— V8 自带 fdlibm 移植、Rust 调用系统 libm，
  末几位本就会不同（`activity_parity` 因此用 1e-12 容差）；
- 另外 `serde_json` 解析十进制浮点**不保证正确舍入**，所以跨语言比对应传**二进制位**
  而不是十进制字面量（见 `sending_parity.rs` 里的记录）。

## 依赖约束

部署主机上**没有** `cmake`、`make`、`pkg-config`，也没有系统 SQLite/OpenSSL 头文件，
并且**没有免密 sudo**。因此：

- SQLite 走 `rusqlite` 的 `bundled`（用 `cc` 从源码编译）；
- TLS 走 `ureq`/`rustls`，实测解析到 **`ring`**（不是需要 cmake 的 `aws-lc-rs`）；
- OneBot 是回环上的明文 `ws://`，`tokio-tungstenite` **不使用 TLS**。

**新增任何依赖前，必须在部署主机上跑一次干净构建验证。**
