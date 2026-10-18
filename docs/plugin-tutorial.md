# I/O 插件：从编写到部署

I/O 插件的角色是**桥**：它把一侧的线格式翻译成另一侧的中立类型，并保证这段路是通的、顺序是对的、坏了会说话。
桥本身**不改变信息**，也**不改变两侧任何一方的行为**。

这份文档按"不变量 → 编写 → 接入 → 测试 → 启用 → 部署 → 观测 → 回滚"的顺序展开。
示例源码在 `docs/examples/example-io-plugin/`，**不参与构建**，只作形状参考。

## 0. 核心不变量：桥不得改变两侧行为

先立判据，再写代码。判断一个改动是否越界，只需问：**换掉这座桥，内核观察到的结果会变吗？对端观察到的结果会变吗？**

| 桥的职责（允许） | 越界（禁止） |
| --- | --- |
| 协议帧 ↔ 中立类型的映射 | 改写、截断、合并正文 |
| 鉴权、心跳、重连、背压 | 自行去重导致语义变化 |
| 同一连接内保持到达顺序 | 跨连接重排、为提速乱序投递 |
| 如实的错误与 `uncertain` | 把失败变成功，或静默降级 |
| 报告连接状态与能力 | 在桥里决定发不发言、回不回复 |
| 持有连接与缓冲区 | 在桥里维护业务状态（pending、记忆、学习） |

三条最容易违反的：

1. **不让内核解释协议 JSON**：`Incoming.raw` 只给适配器与审计用。引用关系、卡片文本、自回显证据都要在桥里变成中立字段。
2. **不吞错误**：队列满、帧畸形、发送超时都必须留下可查询的痕迹。`try_send` 失败而无人查看，等于静默丢消息。
3. **不追加业务判断**：桥只回答"这条线能不能通、这条帧长什么样"，不回答"该不该说、说什么"。

可验收的等价判据（这四条成立，才谈得上"桥是中立的"）：

- **parity**：同一条逻辑消息，经插件与经内置路径，内核入库的正文、`Hint`、引用状态一致；
- **single ingest**：同一条帧只入库一次，包括双连接过渡窗口期间；
- **ordering**：消息与 notice 的相对顺序不变；
- **failure visible**：超时/断连保留 `uncertain: true`，不自动重发，且状态可查。

## 1. 选端口面与能力边界

先回答两个问题，再写代码：

1. 接入哪一面？消息收发是 `ChatPort`，模型调用是 `ModelPort`，数据库是 `DbPort`，记忆/外部/人格各有其面。
2. 这一面里哪些命令**真的**实现，哪些明确不支持？

能力顺序不能反：**先实现，再声明**。仓库里已有先例——OneBot 插件是在媒体发送实现并通过契约测试之后，才把 `media` 从 `unsupported` 移到 `supported`。

只写清单不写实现的，标成骨架：

```json
{ "status": "skeleton" }
```

系统认识这个状态并拒绝启用。骨架不是失败，是"承诺尚未兑现"的显式标记。

## 2. 写清单

清单是对外的唯一声明面。

| 字段 | 含义 | 常见错误 |
| --- | --- | --- |
| `name` | 端口名，非空且不含 `:` | 与目录名混用 |
| `kind` | 端口面 | 大小写或面写错 |
| `transport` | `InProcess` 或 `Sidecar` | 声称 Sidecar 却无协议实现 |
| `core_api_version` | 与内核的兼容区间 | 写成具体版本 |
| `capabilities.supported` | 已实现并验证 | 把"计划做"写进来 |
| `capabilities.unsupported` | 明确不做 | 与 supported 重叠 |
| `config_schema` | 可配置字段 | 把密钥写成普通字段 |
| `host_permissions` | 宿主活动声明，供审计 | 当成权限门禁 |
| `hot_reload` | 能否不重启替换 | 声明 `true` 但无重载实现 |

`version` 统一 `0.1.0`，表示插件契约版本，不是仓库版本。
`host_permissions` 是信息不是门禁：空列表不阻止执行，非空列表也不自动授权。

## 3. 写适配器

`ChatPort` 有五个方法，分工明确：

```rust
impl ChatPort for MyChat {
    fn manifest(&self) -> &PortManifest;
    fn capabilities(&self) -> Capabilities;         // 诚实的能力
    fn state(&self) -> PortState;                   // connected/online/self_id/degraded
    fn deliver(&self, c: Command) -> BoxFuture<PortReply<CommandResult>>;
    fn attach(&self, sink: Sender<PortEvent>) -> Result<(), IoError>;
}
```

### 构造与状态

- 构造期拒绝明显非法输入（空名、含 `:`），错误早出现好定位；
- `self_id` 未知就是 `None`，**不要用空串冒充**；
- "已连接但离线"与"未连接"必须可区分，前者用 `degraded` 给稳定短码（如 `qq_offline`）；
- 状态是给内核判断用的，会进入参与与发送闸门，不是页面装饰。

### 命令语义

| 返回 | 含义 |
| --- | --- |
| `Ok(CommandResult::…)` | 做了，并给出结果 |
| `Unsupported { capability }` | 这个端口不做这件事 |
| `Failed { code, uncertain }` | 试过但失败 |

`uncertain` 只有两种合法含义：`false` = 确定没发出去；`true` = 可能已发出（超时、断连后无 ACK）。
群聊里"发出去了"撤不回来，所以不确定的失败不能被当成成功，也不能自动重发。

### attach：桥的纪律

- **单次**：第二次 attach 必须返回 `sink_already_attached`；
- **保序**：消息与 notice 走同一条流，不要为每条事件另开任务；
- **背压**：队列满时不要静默丢帧。要么等待容量（保序），要么把溢出变成可查询故障。

### P-5：插件不得依赖内核实现

插件源码禁止出现 `crate::`、`super::super`、`include!`、`#[path]`。
它只能通过 `super::{ports, types}` 使用端口契约本身。验证：

```bash
grep -rn "crate::" plugins rust/src/engine/io
# 输出必须为空
```

## 4. 接入构建

`InProcess` 插件要在 `rust/src/engine/io/adapters/mod.rs` 登记：

```rust
#[path = "../../../../../plugins/my_chat/adapter.rs"]
pub mod my_chat;
```

同时，`rust/tests/io_contract.rs` 里有一份**精确的** `#[path]` 白名单：顺序与内容必须一致，数量必须等于实际行数，其他文件出现 `#[path]` 即失败。
这条设计有意让"新增模块"成为显式动作，防止藏模块。**两处必须同步改**。

## 5. 契约测试

分两层，缺一不可。

### 端口层（证明桥本身守约）

| 用例 | 断言 |
| --- | --- |
| 入站可达 | attach 后投递一帧，sink 收到且字段正确 |
| 命令成功 | `Send` 返回 `Sent`，发送可观察 |
| 能力诚实 | 未实现命令返回 `Unsupported` |
| 重复 attach | 第二次返回 `sink_already_attached` |
| 未 attach 投递 | 明确错误，不 panic |
| 不依赖 raw | `Incoming.raw = None` 时语义不变 |

### 等价层（证明桥没有改变行为）

这是"桥"这个定位的真正验收，端口层测不出来：

| 用例 | 断言 |
| --- | --- |
| 正文/Hint/引用 | 经插件与经内置路径入库结果一致 |
| 转发展开 | 边界文本、图片预算、失败归因一致 |
| 媒体采集与 OCR | 处理次数与去重键一致，不重复消费 |
| 主人命令 | 授权/未授权行为与内置路径一致 |
| 自回显 | 只确认唯一匹配的 ACK，伪造帧不确认 |
| 双来源窗口 | 同一帧只入库一次 |

不要写进契约测试：内部字段名、日志文案、私有函数调用次数。它们会让重构变成改测试。

## 6. 启用

启用是配置动作，不是代码动作。在 `plugins/plugins.json` 加入条目：

```json
{ "id": "my_chat", "directory": "my_chat", "enabled": true,
  "params": {}, "bindings": {}, "secret_refs": {} }
```

密钥不进清单：用 `secret_refs` 指向配置存储，日志与文档里永不出现明文。

启用前先确认三件事：

1. manifest 的 `capabilities` 与实现一致；
2. 契约测试与等价测试都绿；
3. 知道怎么回滚（见第 9 节）。

## 7. 部署与重启

`hot_reload: false` 的插件，启停与连接配置变更都要求**整体重启**。不要在运行中的两条连接之间热切。

推荐的部署顺序：

| 步骤 | 动作 | 检查 |
| --- | --- | --- |
| 1 | 备份 `plugins.json` 与当前 `status.json` | 留下回滚依据 |
| 2 | 改 `enabled: true` | 只改这一处 |
| 3 | 重启服务（`systemctl --user restart qq-inner-agent`） | 由授权方执行或代执行 |
| 4 | 读 `status.json` | `ingressSource`、`interfaceAvailability`、`onebotConnected` |
| 5 | 查日志 | 无 `event_rejected`；有 `external_interface_availability_changed` |
| 6 | 发一条测试消息 | 库里恰好一行，回复恰好一次 |

过渡期会**同时存在两条 WebSocket**（内置 + 插件）。这没问题，但必须满足：**只有一条入站来源进入内核**，内置通知被持续排空而不入库。
若发现同一消息入库两次，立即回滚——那是桥污染了行为，不是"小问题"。

## 8. 观测

启用后看三处，而不是只看"插件已加载"：

- `status.json.interfaceAvailability`：三类接口的 `route` 与 `health`；
- `status.json.ingressSource`：`chat_port` 还是 `legacy`；
- 结构化日志：`external_interface_availability_changed`、`event_rejected`、`no_available_io`、`chat_route_unbound`。

三种状态必须如实：

| 情况 | 状态 | 告警 |
| --- | --- | --- |
| 插件接管 | `route: plugin`，`health: ready` | 无 |
| 回落到内置 | `route: built_in`，`health: degraded` | 一次变化记录 |
| 完全不可用 | `health: missing` | `no_available_io` |

不允许"页面说已接管、实际走旧路径"的半完成状态。

## 9. 回滚

回滚必须是配置动作，不是改代码：

1. 把 `enabled` 改回 `false`（或恢复备份清单）；
2. 重启服务；
3. 确认 `ingressSource` 回到 `legacy`、`interfaceAvailability.chat.route` 回到 `built_in`；
4. 发一条测试消息，确认入库与回复各一次。

回滚不需要改端口契约，也不需要动数据库。若回滚需要改代码，说明桥把状态漏进了别处，这是设计问题。

## 10. 验收清单

```bash
cd rust
cargo test -- --test-threads=1          # 必须串行：并行会因资源竞争偶发失败
cargo clippy --all-targets -- -D warnings
grep -rn "crate::" ../plugins src/engine/io
git diff --check
```

- **串行测试**：并行执行会出现随机失败，仓库约定始终 `--test-threads=1`；
- **clippy 零警告**：`-D warnings` 是硬门槛，不要用 `#[allow]` 掩盖；
- **边界扫描**：输出必须为空；
- **`a_memorix` 偶发**：gRPC 失败多为环境问题（服务未运行），单跑确认，不要改代码。

## 11. 常见坑

| 症状 | 原因 | 正确做法 |
| --- | --- | --- |
| 启用后无效果 | 只改清单未接线，或未进入运行代 | 看 `interfaceAvailability` 与 `ingressSource` |
| 能力显示支持但调用失败 | 声明早于实现 | 先实现并测试，再改 `supported` |
| 撤回失败被当成成功 | 用 `Ok` 表示"没做" | 返回 `Unsupported` |
| 超时后消息重复 | `uncertain` 被当 `false` 并重试 | 保留 `uncertain: true`，不重发 |
| 入站乱序 | 每条事件单独 spawn | 单任务串行投递 |
| 高负载丢消息 | `try_send` 满队列被忽略 | 等待容量或记录可查询故障 |
| 同一消息入库两次 | 双连接窗口未排空内置通知 | 只保留一条入站来源 |
| 契约测试红在隐藏模块 | 只改 `adapters/mod.rs` 未改白名单 | 两处同步 |

## 12. 示例的边界

`docs/examples/example-io-plugin/` 下的源码**不参与构建**：

- 一旦接入 `adapters/mod.rs`，它就成为生产二进制的一部分，并被迫进入精确白名单；
- 一个永远不连任何东西的 `ChatPort` 留在部署面里，只会增加误用风险；
- 为它跑全量测试，验证的只是它自己，对真协议零覆盖。

示例的定位是**形状参考**；真实实现请以 `plugins/onebot/` 为参照。
如果你要求示例可编译，那就没有"仅示例"这回事——它必须按第 4 步接线、按第 5 步写两层测试，并进入每次全量验收。
