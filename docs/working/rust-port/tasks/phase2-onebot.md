你是 qq-inner-agent 的 Rust 移植工程师。当前工作区是一个**隔离的 git worktree**,分支
`pebrel/<你的分支>`,基线是 Phase 1 之后的 `main`。你的改动不会影响主分支。

## 前置

Phase 1(配置层)已经完成:`rust/src/settings.rs` 与 `rust/src/config.rs` 已存在并可编译。
请先读它们,复用其中的类型与错误处理风格。

## 必读

- `docs/working/rust-port/ARCHITECTURE.md` —— 架构与不可变更的契约
- `docs/working/rust-port/SURVEY.md` —— **第 6 节是 OneBot 层的完整规格**(逐条,含全部错误码与状态串)
- `src/onebot.mjs`(119 行,务请逐行读完)—— 要移植的唯一真源

## 任务:Phase 2 —— OneBot v11 传输层

在 `rust/src/onebot.rs` 中实现 `OneBot`,并接入 `main.rs`(新增子命令 `check`,用于校验桥)。
逐条对齐 `src/onebot.mjs` 的行为,**不得凭印象简化**:

### 协议与连接

- 正向 WebSocket 客户端;`access_token` 通过 **URL query 参数**传递。
- 连接超时 `requestTimeoutSeconds` → 错误码 `connect_timeout`;关闭 → `connect_closed`。
- 握手:`get_login_info`;缺失 `user_id` → `missing_account`;`config.selfId` 非空且不匹配 →
  `wrong_qq_account`;否则 `selfId = String(user_id)`。
- 随后 `get_status` → `online = status.online === true`;`connected = true`;
  发出状态 `connected` 或 `qq_offline`。
- **早到事件缓冲**:`connected` 置位之前收到的事件进入缓冲(上限 200 条),置位后按序补发。
- 心跳:每 `heartbeatSeconds` 调一次 `get_status`,**必须有防重入标志**;失败时
  `online = false` 并关闭连接。
- 帧上限 1_000_000 字符;超过则忽略。
- **绝不能打印错误对象**(URL 里带 token);只发状态串 `websocket_error`。

### 请求关联

- 发送 `{action, params, echo}`(echo 用 UUID)。按 `echo` 匹配响应:
  `status === "ok" && retcode === 0` → 成功返回 `data`;
  否则错误码 `onebot_action_failed_<retcode>`。
- 超时 `requestTimeoutSeconds` → `action_timeout`,**`uncertain = true`**。
- 发送失败 → `send_failed`,`uncertain = true`。
- 连接断开时,所有在途请求以 `connection_lost`(`uncertain = true`)失败并清空。
- 错误类型需带 `uncertain` 标志(对应 JS 的 `OneBotError(code, uncertain)`)。

### 重连

- 循环重连;退避 `delay` 从 1 开始,每次 ×2,上限 `reconnectMaxSeconds`,并叠加随机抖动;
  **若上一次会话存活超过 30 秒则把 delay 重置为 1**。
- 每次重连 `reconnects += 1`。回到 Phase 1 之前的状态串语义:
  `connected` / `qq_offline` / `websocket_error` / `connect_timeout` / `connect_closed` /
  `missing_account` / `wrong_qq_account` / `connection_lost` / `connection_failed`。

### 发送

- 前置校验:`connected && online` 否则 `qq_offline`;
  chat 必须匹配 `^(group|private):[1-9]\d*$` 否则 `invalid_chat`;
  `face_id` 为 None 或匹配 `^\d{1,5}$` 否则 `invalid_face`。
- 调用 `send_group_msg` / `send_private_msg`,参数:
  `{ group_id|user_id: 数字, message: [ {type:"text",data:{text}}, (可选){type:"face",data:{id}} ], auto_escape: true }`。
  **`auto_escape` 与数组文本段要同时保留** —— 这是让模型写出的 CQ 码变成惰性文本的双重防护。

### 事件

- **不做 `post_type` 白名单**:任何带 `post_type` 的帧都透传(分类由后续 policy/engine 负责)。
- 对外暴露:状态变更回调 + 事件回调(不要用 EventEmitter 的模拟,直接用
  `tokio::sync::broadcast`/`mpsc` 或 `watch` 通道,选择你认为最清晰的方案并说明理由)。

## 硬性约束

1. **不得引入新依赖。** 现有依赖里已有 `tokio` 与 `tokio-tungstenite`(不使用 TLS —— 桥是
   回环上的明文 `ws://`)。禁止引入需要 cmake/make/pkg-config/OpenSSL/系统 SQLite 的 crate。
2. **不得修改 `rust/` 以外的任何文件。**
3. 中文注释,重点标注**容易出错处**:退避重置条件、early 缓冲时序、echo 关联与超时的竞态、
   `uncertain` 的语义。

## 验收标准

1. `cargo build --release` 成功;`cargo clippy -- -D warnings` 无警告;`cargo test` 全过。
2. **必须有 mock 测试**:在测试内用 `tokio-tungstenite` 起一个本地 WebSocket 服务端,覆盖:
   - token 鉴权(query 参数正确/错误)
   - `get_login_info` / `get_status` 握手,含 `missing_account` 与 `wrong_qq_account`
   - `connected` 置位前的 early 事件缓冲与补发顺序
   - `call` 的 echo 关联、成功、非零 retcode、超时(→ `action_timeout` 且 uncertain)
   - 心跳刷新 online 与心跳失败关连接
   - 断线后重连且 `reconnects` 递增、退避上限
   - `send` 的两种 action、数组消息体、`auto_escape`、`face` 段、以及三类前置校验错误
   - 帧超限被忽略
   测试**不得**连接真实的 NapCat/SnowLuma,不得产生真实网络出站。
3. 提供 `--root` 下的 `check` 子命令:连接桥、打印鉴权/在线状态后退出(非零退出码表示失败)。

## 交付

1. `git add -A && git commit`,英文 commit message。
2. 用**一段话**回报:实现了什么、测试怎么跑、与 JS 的已知差异或不确定点。
