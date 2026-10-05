# 更新日志

本文件按语义化版本记录 qq-inner-agent 的发布。当前版本见 `package.json` 与 `rust/Cargo.toml`。

## 1.0.0 —— Rust 内核正式交付

日期:2026-10-05

### 核心变化

后端运行时已从 Node.js **整体迁移到 Rust**。Node 侧仅保留仪表盘与 CLI,二者通过本地
NDJSON 控制套接字(`data/control.sock`)通信。配置、数据库 schema、状态文件格式与模型
提示词 JSON 契约**保持不变**,可与旧 Node 内核随时互换回退。

### 已完成

**Rust 内核(17 个阶段)**

| 类别 | 内容 |
| --- | --- |
| 配置/存储 | `config`、`settings`(revision 哈希)、`store`(17 张表) |
| 传输 | OneBot v11(WS/心跳/重连)、provider(HTTP/重试/预算) |
| 记忆/表达 | 三层记忆、排序、表达/黑话学习、词嵌入缓存 |
| 引擎 | 48 步决策循环、策略/发送/活跃度/入群观察、素材采集与选择 |
| 人类化 | 长度分档、按群表情频率、只发表情(门控)、三层决策、主人教学指令 |
| 控制 | 控制套接字服务端 + 方法表 + 事件 |

**Node 保留**:`dashboard.mjs`(HTTP 5097 / HTTPS 5098)+ CLI,双读回退(控制套接字可用则
订阅事件,否则回退 `status.json` + 只读 SQLite)。

**前端个性化**(视觉改版)与**提示词更像真人**(反 AI 腔黑名单、长度分档、人格 persona)。

**主人教学**:主账号私聊用 `/记住` `/黑话` `/忘记` 直接教学,仅 `ownerUin` 可用、来源标记
不伪造。

### 测试

- Rust **161 项**、Node **111 项**,本机(macOS)与远端 Linux 双平台全部通过,clippy 零警告。
- **实况验证**:远端 systemd 常驻,连 OneBot、模型调用(`deepseek-flash`)、发送与诊断/控制
  接口全通;核心链路完成"接收 → 决策 → 调模型 → 发送"的组件级端到端验证。
- **实况中修复**:`max_tokens` 由浮点改为整数序列化(DeepSeek 等 OpenAI 兼容端点拒绝浮点,
  期望 u32)—— 该问题只有真实调用才能暴露,单测/parity 均未覆盖。

### 部署

三个 systemd 用户服务,均 `Restart=always` 自愈:

- `qq-inner-agent.service` → `rust/target/release/qq-inner-core start`(Rust 内核)
- `qq-inner-dashboard.service` → `node src/dashboard.mjs`(Node 仪表盘)
- `snowluma.service` → SnowLuma QQ 桥(OneBot v11)

### 已知遗留(非阻塞)

- **P11 测试重写**(把 parity 测试固化为 Rust 金标准、移除 `node` 依赖)因 codex 额度暂停,
  规格见 `docs/rust-port/tasks/phase11-test-rewrite.md`,留有 WIP 检查点于分支 `pebrel/rust-testrewrite`。
- **性能对比**(内存 / CPU / 延迟)留待后续。
- `docs/prompt-and-learning-design.md` 中的**提示词分层、学习分诊、affect 指标、记忆召回
  下钻**等设计已记录,尚未实施。
