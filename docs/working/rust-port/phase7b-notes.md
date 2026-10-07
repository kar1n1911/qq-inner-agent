# P7b 控制客户端交付说明

Node 控制客户端仅使用内置模块，支持 1 MiB 字节分帧、请求 id 关联、独立超时、事件订阅、有界待处理请求与慢读背压、指数退避重连及关闭清理。available 反映连接状态；请求超时不会重放，也不会中断同连接的其它请求。迟到响应被忽略。

仪表盘保留 HTTP 路由、鉴权及响应字段；state.get、learning.list、logs.tail 用于覆盖文件快照的对应部分，失败时保留 status.json 和只读 SQLite 的结果。没有 socket 时不等待连接即可读取状态。所有状态来源经过凭据脱敏。诊断、模型和学习重置在连接可用时走控制协议；写操作与发送失败后不自动重试。配置保存仍走原文件协议。

## 已知设计差异与服务端限制

- P7a 没有查询 decisions/thoughts/assessments/observations 的控制方法，这些字段始终读只读 SQLite。learning.list 的记忆和表达共用 1000 条上限；饱和时表达仍读 SQLite，以保持旧 API 各 200 条的语义。state.get 新增的 learningCounts 不透传到旧 HTTP 状态字段。
- P7a debug.send 不返回随机正文；适配层保留 account/messageId/text/message 字段，text 返回空字符串，不伪造正文。接收诊断的 state/account/until/error 由 Node 适配保存；仪表盘重启后若 Rust 仍在捕获，协议无法恢复 account/until，返回空 account 和 null until。
- agent 已把 start/check/contacts/add-memory 路由到 Rust 二进制（默认 rust/target/release/qq-inner-core，可用 AGENT_CORE 指定），core-status 由 Node 实际连接套接字并调用 state.get。Rust 当前仅实现其中的 start/check，尚无 contacts/add-memory，也不接受 check --api。因此这三个旧用法仍需后续 Rust 阶段补齐；本阶段遵守禁止修改 rust/，没有退回 Node 执行它们。
- 控制事件通过 ControlClient.subscribe(name, listener) 订阅；前端继续原 HTTP 轮询，不新增推送协议。

## 验证

新增 mock Unix socket 测试覆盖乱序 id、UTF-8 分包与粘包、事件、服务端错误、超时、断线重连、不重放、非法/超限/未结束帧、有界请求、关闭清理。HTTP 测试验证 socket 不存在时 available=false 且状态回退正常、连接成功后实时读取及脱敏、学习列表截断保护、无响应时回退、诊断旧字段适配及不确定发送不重复。

原 test/dashboard.test.mjs 全部通过。完整 Node 测试仅有既有 macOS 127.0.0.2 EADDRNOTAVAIL 平台失败；cargo test 全部通过。无 npm 依赖、rust/ 或前端修改。
