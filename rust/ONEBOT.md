# OneBot 传输层

`OneBot::new(config::Onebot, token)` 返回可克隆句柄和
`mpsc::UnboundedReceiver<Notification>`。调用 `run(watch::Receiver<bool>)`
运行重连循环；向 watch 发送 `true` 或释放发送端，然后等待 run 返回即可停机。
调用 `state()` 读取 connected、online、self_id、reconnects，调用
`call(action, params)` 或 `send(chat, text, face_id)` 请求桥。
同一对象的 run/check 会串行执行，其他句柄可以并发 call/send。

状态与事件共用一个 mpsc 队列，确保 connected 通知、最多 200 条 early
事件及后续事件保持顺序。选择 mpsc 而不是 watch（会合并通知）或 broadcast
（慢消费者可能丢事件）；上层应持续消费队列，必要时自行分发给多个订阅者。
无界队列不会给 WebSocket 读循环施加背压，但慢消费者会增加内存占用。
心跳更新可通过 state 读取，不额外发送状态通知，与 JS 一致。

`qq-inner-core --root /path/to/repo check` 单次鉴权并打印在线状态，不发消息、
不重连；鉴权、账号匹配、连接或在线检查失败均以非零码退出。
配置/密钥沿用 Phase 1 加载流程，包括 ONEBOT_TOKEN 环境覆盖。

兼容边界：

- 按任务要求严格校验整个 chat；JS split 实际会接受 `group:123:extra`，Rust 拒绝。
- JS 响应缺失 data 的 undefined 在 Rust JSON 接口中表示为 null。
- 停机/取消 future 会释放 TCP 连接并清理 pending；普通会话退出会先尝试限时
  WebSocket close。取消不等待对端 close 握手。
- 仅支持现有依赖提供的明文 ws，不增加 TLS 依赖。
- 未连接真实 NapCat/SnowLuma，桥特有行为仍需后续实况验证。

验证（在 rust/ 下）：`cargo build --release`、`cargo clippy --all-targets -- -D warnings`、
`cargo test`。网络测试仅使用 127.0.0.1 上的 tokio-tungstenite mock，覆盖鉴权、
握手/错误码、early 缓冲、乱序 echo、超时/断线、心跳防重入/重连、发送与校验、
UTF-16 帧上限及 CLI 退出码；退避严格 >30 秒重置与上限另有确定性单元测试。
