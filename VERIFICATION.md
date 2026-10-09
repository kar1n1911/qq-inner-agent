# Verification — 2026-10-01

> **当前状态(1.1.0)**:Rust 内核 `cargo test` 192 项通过(本机 + 远端 Linux,clippy 零警告)。
> 旧 Node 实现与其测试已归档到 `js-legacy` 分支,`./agent test` 现只跑 6 个运维 Node 测试。

## Telegram 网关（2026-10-09）

新增与 OneBot 平级的第二传输层（`rust/src/transport/telegram.rs`、
`rust/src/transport/gateway.rs`）。离线 mock 测试与本机真实 bot 联调均已执行。

**已覆盖（离线 mock，无网络）**

- Telegram mock HTTP（本地 `TcpListener`）：`check` 认证/401、`send` 成功/离线/payload/429/401、
  `call` 映射与 `unsupported_action`、offset 跨重启持久化、退避与停机、未授权 chat 记录与
  `telegram_chat_ignored` 日志。
- 翻译纯函数 `update_to_event`：私聊/群（负 id）、UTF-16 emoji 实体偏移、`mention`/`text_mention`、
  回复 bot、媒体/位置/转发标记、服务与未知 update 忽略、`my_chat_member` → `group_increase`。
- 路由（`gateway_tests`）：按 chat/action 归属、`send`/`send_media` 与身份解析
  （`event_self_id`/`chat_self_id`）、来源网关过滤、仅 OneBot 时行为不变、通知合并与停机。
- 记忆作用域回归：负数群 id 接受、`-0`/非法/私聊不匹配拒绝（`memory::tests`）。
- 配置：Rust/Node 双侧默认值、校验（proxy、负群 id、超时范围）、密钥来源（`secrets.json` /
  `TELEGRAM_BOT_TOKEN`）、readiness、白名单条件扁平化；`config_parity` 保留 Telegram 分段。
- 回归：`cargo test` 29 个测试目标共 **289 项**通过（其中 `telegram` 28 项、`gateway` 5 项、
  `config_parity` 8 项、`memory::tests` 2 项）；`node --test test/*.test.mjs` 23 项（22 通过，
  1 跳过：`src/main.mjs` 已移除，热重载由 Rust 侧覆盖）。`cargo clippy --all-targets` 0 警告。

**已覆盖（本机真实 bot 联调，2026-10-09）**

- 真实 Telegram bot 经本机 HTTP 代理访问 Bot API（直连不可达）。
- AC1：`./agent check` 输出 `telegram = ok username=… selfId=…`；`data/status.json` 含
  `telegramConnected/telegramOnline/telegramUsername/telegramSelfId`。
- AC2：私聊消息入库、完整决策流程触发并回复；`test.model` 经控制套接字验证成功。
- AC3：群聊在关闭 Bot API 隐私限制后普通消息可达；@bot（`mention`）与“回复 bot 消息”均被
  识别为直呼并回复；观察期走完并写入 `analysis`（`status=ready`）。
- AC7：systemd user service 安装并运行，`data/telegram-offset.json` 持久化 offset，重启不重复
  消费；`data/status.json` 每 5 秒更新。
- 联调中发现并修复一个真实缺陷：`memory_subjects` 仅接受正数群 id，导致 Telegram 超级群消息
  在 `LayeredMemory::capture` 返回 `invalid_memory_scope`、消息虽入库但被 `event_rejected`
  中断、不进入回复流程。现允许群 chat id 前导 `-`，并新增回归测试。

**未验证 / 环境限制**

- 公网 webhook 冲突（409）未用真实 webhook 触发（代码路径有 mock 覆盖）。
- 代理可用性取决于本机 mihomo 订阅与节点；联调期间多次出现节点失效导致的
  `telegram_network_error`，靠自动退避与重新刷新订阅恢复，非代码缺陷。
- 仪表盘 Telegram 面板与状态为代码/单测覆盖，未做浏览器人工验收。

## 历史记录（2026-10-01）

- 93 tests passed, 0 failures (`./agent test`).
- Expression tests cover literal-source validation, scope/author boundaries, repeated evidence and two-author group admission, pending/changed meanings, confidence and expiry, confirmed reuse cooldown, persona precedence, bounded decorations, native face segments with inert CQ-looking text, and the complete mock engine articulation/send pipeline without extra model stages. Browser QA confirmed Chinese personality fields and saving multiline interests/style alternatives in an isolated dashboard. Real bridge face rendering and inferred slang quality remain unverified.
- A_Memorix-inspired memory refinements are covered by tests for legacy schema migration, evidence accumulation, stale-evidence rejection, idempotent expiry, bounded revisions and archive cleanup, query-aware notebook ranking, confidence/budget gates, sparse Chinese retrieval, optional metadata validation and preserved scope isolation. Browser QA verified synthetic revised memory details; live recall quality remains unverified.
- Activity rhythm tests cover Gaussian edge/center probabilities, symmetry, overnight schedules and local time zones, invalid settings, durable account-wide blocks, no repeated draws, restart/config-change behavior, clock rollback, schedule-boundary continuity, direct-message suppression during rest, queued-work cancellation and expiry during generation. Browser QA confirmed the Chinese controls and saving in an isolated dashboard; no real model/QQ messages were used.
- HTTPS proxy tests cover same-port HTTP redirects, unknown-host handling, TLS forwarding, upstream failure, repeated redirects without extra upstream connections or accumulating listeners, real TLS login-rate isolation between two client addresses, ignored spoofed forwarding headers, and idempotent shutdown with idle TCP and established TLS connections. Tests use a temporary local certificate and no production credentials.
- Observation tests cover both/either thresholds, direct-mention withholding, duplicate/self filtering, collection of metadata/announcements/history, analysis-before-generation, style injection, persisted readiness and retry delays, unsupported sources, private bypass, rejoin cancellation and bounded cross-group-safe history parsing. Tests use mocked bridge/model responses; live source availability remains unverified.
- Three-layer memory tests cover QQ-ID attribution, personal/group/private isolation, multi-author group evidence, rejection of wrong/self/cross-chat sources, short-term deduplication and expiry, selective notebook revision/forget/capacity, independent long-term retention, per-subject reset, restart persistence, live limits, and candidate-cache isolation between speakers. Notebook behavior is application-level state management, not a trained Mamba model. No live model or QQ calls were used for these tests.
- Adaptive-persona tests cover custom-persona preservation, configuration bounds, human-source validation, applying learned style/RAG in articulation without an extra API call, learning cadence/disable behavior, Chinese retrieval, context exclusion, isolation, memory limits/expiry, persistence and reset during generation. Dashboard tests verify authenticated/CSRF-protected inspection and reset. Browser QA showed Chinese learning controls and a clearly labeled synthetic profile in an isolated preview. Social effectiveness with a live model remains unverified.
- Sending-policy tests cover time/pace factors, probability bounds, forecast validation and vetoes, single-attempt persistence across pauses/restarts, stale forecasts, articulation plans, expiring chat-isolated expectations, and dry-run/uncertain-delivery behavior. Both API formats exercise the four-stage pipeline against local HTTP and WebSocket mocks. Browser QA verified the Chinese controls and saving a probability in an isolated dashboard. No live model or QQ requests were made for this feature.
- Language tests validate independent interface/reply settings, Chinese prompt contracts, translated UI messages, and selected reply-language instructions in the actual engine pipeline. Browser QA uses an isolated configuration to check Chinese/English switching and saving, without live model calls or QQ messages.
- Activity schedule tests cover exact minute boundaries, time zones, overnight intervals, invalid schedules, direct-request suppression, queued-work cutoff, and a response crossing into inactive hours. Model-list tests verify provider URL/authentication selection, deduplication, and failure handling without inference.
- QQ dashboard diagnostic tests cover fixed self-account sending, capture of multiple self-message formats, filtering other accounts/senders, credential redaction, bounded capture, timeout/stop cleanup, and authenticated/CSRF-protected routes. Real self-chat delivery and bridge self-event reporting still require a live QQ bridge.
- NapCat-shaped forward WebSocket mock tests cover array and CQ-string events, group mentions, private replies, contact lists, lifecycle/heartbeat events, self-message filtering, and rejection of invalid tokens after upgrade. Live NapCat login and message delivery remain unverified.
- Dashboard tests cover authenticated access, origin and CSRF checks, secret redaction, configuration validation, stale revisions, and interrupted-save recovery.
- A process-level test verifies live configuration reload, rejection of invalid edits, and recovery without changing the agent PID.
- Both model API adapters exercised against real local HTTP mock endpoints.
- OneBot transport exercised against a local WebSocket mock: authentication, request correlation, sending text segments, heartbeat timeout, reconnection, and duplicate-event suppression.
- Decision tests cover turn allocation, interruption threshold, withholding, retained ideas, quiet hours, cooldowns, chat isolation, stale responses, dry-run mode, invalid model output, API budgets, and ambiguous delivery outcomes.
- A regression test verifies that the DeepSeek profile never uses an unrelated OpenAI or Anthropic environment key.
- Live SnowLuma OneBot authentication succeeded; the test account reported online.
- The installed user service reported `onebotConnected: true`, `qqOnline: true`, and `mode: waiting_for_setup` during verification. SnowLuma and the dashboard were subsequently stopped at the owner's request.
- User-service lingering is enabled. The service can survive logout; the GUI QQ session and host still need to remain available.
- Single-instance protection rejected a second foreground start with exit code 75.
- Local configuration and secrets have mode `0600`; the data directory has mode `0700`.
- The agent's separate Node runtime has no Linux file capabilities.
- Python setup scripts and shell launcher passed syntax checks. The generated service unit loaded successfully after correcting WorkingDirectory formatting.

Not yet verified: a live DeepSeek completion or an actual QQ reply from this program. No DeepSeek key or selected chat IDs have been provided. No real QQ messages were sent during verification, and no paid model calls were made. Use `./agent setup`, followed by `./agent check --api`, to complete local configuration and verify provider access.
