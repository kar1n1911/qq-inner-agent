# 三类外部接口的 IO 契约

本文只回答三件事：每类接口**传出什么**、**传入什么**、两边**怎么对齐**。
末尾附一个示例插件，演示如何照着这些契约写一个自己的插件。

三类接口分别是消息（`ChatPort`）、模型（`ModelPort`）、数据库（`DbPort`）。
它们共用同一套返回语义：`Ok` 表示做到了，`Unsupported` 表示这一面不做这件事，`Failed { code, uncertain }` 表示试过但失败。

## 1. 消息面：ChatPort

### 1.1 传出内容（内核 → 插件）

内核通过 `deliver(Command)` 发出命令，插件执行后返回 `PortReply<CommandResult>`。

| 命令 | 关键字段 | 用途 |
| --- | --- | --- |
| `Send` | `chat`、`segments`、`reply_to` | 发一条消息，段可含文本/提及/表情/图片 |
| `SendMedia` | `chat`、`media` | 单发一个媒体（当前只接受图片） |
| `Recall` | `port`、`message_id` | 撤回 |
| `FetchHistory` | `chat`、`count`、`before` | 拉历史；`before` 映射到协议游标 |
| `FetchForward` | `port`、`id` | 展开合并转发，返回类型化节点 |
| `FetchMembers` / `FetchGroupInfo` | `chat` | 群成员/群资料 |
| `FetchFriendList` / `FetchGroupList` / `LoginInfo` / `Health` | `port` | 联系人、登录信息、健康 |
| `Fetch` | `port`、`request` | 群公告、成员资料、陌生人资料等原始查询 |
| `SetGroupCard` / `SetProfile` / `SetAvatar` / `SetLongNick` | 各自字段 | 改名、头像、签名、群名片 |

| 结果 | 含义 |
| --- | --- |
| `Sent { message_id }` | 协议确认发送成功，带回协议 ID |
| `History` / `Forward` / `Members` / `GroupInfo` / `Friends` / `Groups` / `LoginInfo` / `Health` | 类型化查询结果 |
| `Fetched(Value)` | 原始查询数据，**原样**返回，不做字段重排 |
| `Ok` | 做完了，没有数据 |
| `OutboundEvidence` | 读回得到的出站证据 |

返回语义必须分清：

- `Unsupported { capability }`：这一面不做这件事，属于正常；
- `Failed { code, uncertain: false }`：确定没发生（校验失败、离线且尚未调用）；
- `Failed { code, uncertain: true }`：可能已经发生（超时、断连后没有 ACK）。

### 1.2 传入内容（插件 → 内核）

插件通过 `attach(sink)` 拿到一个 `Sender<PortEvent>`，把事件投进去。

| 事件 | 内容 | 用途 |
| --- | --- | --- |
| `Incoming(Box<Incoming>)` | 一条普通消息 | 入站主路径 |
| `Notice(Notice)` | 群公告类事件 | 入群、退群等状态变化 |
| `OutboundEvidence(OutboundEvidence)` | 出站证据 | 自回显与 `get_msg` 读回 |

`Incoming` 的字段：

| 字段 | 语义 |
| --- | --- |
| `port` | 端口名，必须与 chat 地址里的端口一致 |
| `chat` | `kind:port:id` 三段地址 |
| `message_id` | 协议 ID，只用于审计与匹配，不是存储主键 |
| `self_id` | 机器人账号 |
| `ts` | **协议服务器时间** |
| `received_at` | **本机收到的时间**，永不替代 `ts` |
| `sender` | `id`、`name`、`card`、`role`、`title`、`is_robot` |
| `segments` | 文本、提及、回复、表情、图片、转发、卡片 |
| `reply` | `NoReply` / `Target` / `MissingId` / `InvalidId` |
| `self_message` | 是否自己发出 |
| `raw` | 协议原始帧，**只给适配器与审计**，内核不得解释 |

`Notice` 的字段：`kind`（`GroupJoined` / `Unknown(原始类型)`）、`chat`、`actor`、`self_id`、`at`。
没有 `notice_type` 的畸形帧不是"未知种类"，应当明确报错。

`OutboundEvidence` 的字段：`chat`、`message_id`、`sender_id`、`text`、`server_ts`、`received_at`、`source`（`MessageSent` / `Readback`）。

### 1.3 对齐方案

| 主题 | 规则 |
| --- | --- |
| 时间 | `ts` 只放协议时间，`received_at` 只放本机时间；缺失即拒绝，不互相顶替 |
| 身份 | `self_id` 与发送者分别核验；未知用 `None`，不用空串冒充 |
| 引用 | 引用关系由插件判定成 `ReplyObservation`，内核不读 `raw` |
| 卡片 | 展示文本由插件生成 `display_text`，内核不解析卡片 JSON |
| 顺序 | 消息与 notice 走**同一条流**，同一连接内按到达顺序投递 |
| 背压 | 队列满时等待容量或记录可查故障，不静默丢帧 |
| 错误 | `Unsupported` 与 `Failed` 不可混用；`uncertain` 决定能否重试（不能） |
| 自回显 | 只由 `OutboundEvidence` 承担；普通 `message` 即使发送者是自己也不能冒充 |

## 2. 模型面：ModelPort

### 2.1 传出内容（内核 → 插件）

| 字段 | 语义 |
| --- | --- |
| `instance` | 目标实例名 |
| `system` | system 提示词 |
| `payload` | 中立 JSON：`messages` 数组，或 `{ messages, tools }` |
| `format` | `Json` 或 `Text` |
| `max_tokens` | 本次上限 |

`payload` 的形状是中立约定，不是某家供应商的线格式：

| 位置 | 形状 |
| --- | --- |
| `messages[].role` | `user` / `assistant` / `tool` |
| `assistant.tool_calls` | `ModelToolCall { id, name, arguments }` |
| `tool` 消息 | `ModelToolResult { tool_call_id, name, content, is_error }` |
| `tools[]` | `ModelToolDefinition { name, description, input_schema }` |

### 2.2 传入内容（插件 → 内核）

| 返回 | 内容 |
| --- | --- |
| `ModelReply` | `text`，以及 `tool_calls: Vec<ModelToolCall>` |
| 流式 `ModelDelta` | `Text` / `ToolCallStart` / `ToolCallArguments` / `ToolCallEnd` / `Done` |
| `ModelCapabilities` | `json_mode`、`thinking_field`、`truncation_signal`、`list_models`、`context_window_tokens` |

### 2.3 对齐方案

| 主题 | 规则 |
| --- | --- |
| 空回复 | `text` 为空只有在至少一个 `tool_calls` 存在时才算成功 |
| 上下文 | `context_window_tokens` 为 `None` 表示未知，调用方不得假设窗口大小 |
| 能力 | `json_mode` 等是声明；不支持时返回 `Unsupported`，不要静默降级成文本 |
| 截断 | 有 `truncation_signal` 才标记截断；没有就不要猜 |
| 错误 | `Failed { code }` 直接上报；模型请求不自动重试 |
| 密钥 | 只从 `secret_refs` 读取，日志与错误里永不出现明文 |

## 3. 数据库面：DbPort

### 3.1 传出内容（内核 → 插件）

`DbPort` 只暴露一个入口：

```rust
fn store(&self) -> Result<MutexGuard<'_, Self::Store>, IoError>;
```

调用方持有锁与事务边界，通过 `Store` 写入。写入按主题分类：

| 主题 | 代表方法 |
| --- | --- |
| 消息 | `record_inbound_message`、`record_message`、`record_distinct_message` |
| 投递 | `delivery`、`finish_delivery`、`confirm_outbound`、`reconcile_outbound` |
| 学习 | `learn`、`learn_topic_reply`、`reset_learning` |
| 观察 | `observe`、`ensure_orientation`、`orientation_joined`、`orientation_failed` |
| 话题 | `link_topic_send`、`topic_revisit`、`classify_topic_once` |
| 维护 | `prune`、`migrate_message_keys` |

### 3.2 传入内容（插件 → 内核）

| 查询 | 返回 |
| --- | --- |
| `messages` / `history` | 行集合，按既有排序与分页参数 |
| `recent_unconfirmed_outbound` | 待确认的出站事实 |
| `counts` / `score` / `call_budget` | 聚合标量 |
| `orientation_state` / `orientation_sources` | 入群观察状态 |
| `learning_state` / `decision` / `assessment` | 学习与决策上下文 |

### 3.3 对齐方案

| 主题 | 规则 |
| --- | --- |
| 主键 | `message_key` 是本地单调主键；`message_id` 只是协议字段，二者不互换 |
| 时间来源 | `time_source` 区分 `server` 与 `estimated`；估算值不得伪装成服务器时间 |
| 事务 | 调用方持有 `MutexGuard` 与事务边界，端口不自己在内部提交 |
| 过滤 | 过滤、排序、分页、计数、去重一律下推到数据库，不把整表读进内存 |
| 热路径 | 不 SELECT 大 JSON/text 列，除非本次确实要用 |
| 错误 | 打开失败、写入失败必须显式返回，不吞异常 |

## 4. 示例插件：自己写一个

示例源码在 `docs/examples/example-io-plugin/`，**不参与构建**，只作形状参考。
真实实现以 `plugins/onebot/` 为参照。

### 4.1 目录

```text
plugins/my_chat/
  manifest.json
  adapter.rs
```

### 4.2 清单

```json
{
  "name": "my_chat",
  "version": "0.1.0",
  "core_api_version": { "major": 1, "min_minor": 0, "max_minor": null },
  "entry": { "type": "built_in", "adapter": "plugins/my_chat/adapter.rs" },
  "kind": "Chat",
  "transport": "InProcess",
  "capabilities": { "supported": ["mention", "reply"], "unsupported": ["media"], "unknown": [] },
  "dependencies": [],
  "config_schema": [],
  "host_permissions": [],
  "hot_reload": false
}
```

能力顺序不能反：**先实现，再声明**。只写清单不写实现的，标 `"status": "skeleton"`。

### 4.3 适配器：按三段写

```rust
impl ChatPort for MyChat {
    fn manifest(&self) -> &PortManifest { &self.manifest }
    fn capabilities(&self) -> Capabilities { /* 与实现一致 */ }
    fn state(&self) -> PortState { /* connected / online / self_id / degraded */ }
    fn deliver(&self, c: Command) -> BoxFuture<PortReply<CommandResult>> { /* 1.1 */ }
    fn attach(&self, sink: Sender<PortEvent>) -> Result<(), IoError> { /* 1.2 */ }
}
```

三段各自对应本文的章节：

- `deliver` 实现 1.1 的命令与结果；
- `attach` 登记唯一 sink，按 1.2 投递事件；
- `state` 与 `capabilities` 报告 1.3 里要求的事实。

示例里有一个可直接照抄的细节：未实现的命令返回

```rust
PortReply::Unsupported { capability: Capability::Custom("my_chat_command".into()) }
```

而不是 `Failed`——"不做"与"失败"是两件事。

### 4.4 接入与测试

```rust
// rust/src/engine/io/adapters/mod.rs
#[path = "../../../../../plugins/my_chat/adapter.rs"]
pub mod my_chat;
```

`rust/tests/io_contract.rs` 里有一份**精确的** `#[path]` 白名单，必须同步加入同一行；数量与顺序都要一致。
插件源码里不得出现 `crate::`、`super::super`、`include!`、`#[path]`，只能通过 `super::{ports, types}` 使用契约。

测试至少覆盖：入站可达、命令成功、能力诚实、重复 attach 被拒、未 attach 投递报错、`raw` 为 `None` 时语义不变。

### 4.5 示例不做什么

示例插件不接任何协议，也不进入构建：它没有清单之外的状态，不会连接任何服务。
如果要求它可编译，就必须按 4.4 接线，并进入每次全量验收——那时它就不再是"仅示例"了。
