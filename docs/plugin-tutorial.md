# 插件开发教程：从零写一个 I/O 面插件

这份教程用一个不接任何协议的示例插件，走完"决定端口面 → 写清单 → 写适配器 → 接入构建 → 契约测试 → 启用 → 验收"的全过程。
示例源码在 `docs/examples/example-io-plugin/`，**不参与构建**，只作展示。

## 为什么是"端口面"

内核不直接依赖某个具体协议或某个具体中间件，而是依赖六个中立端口：
`ChatPort`、`ModelPort`、`DbPort`、`MemoryPort`、`ExternalPort`、`PersonaPort`。

这样做的判据不是"能跑"，而是**能不能被证伪**：如果换一个实现只需要换清单，端口就是抽象；如果必须内核改代码，那只是搬家。

因此有两条硬约束贯穿全文：

- **声明不等于实测**：manifest 与 `capabilities()` 写的是声明，运行时状态另算；
- **不得伪装**：没实现的能力不许先声明，骨架插件必须标 `skeleton` 且被拒绝启用。

## 全流程一览

| 步骤 | 产出 | 怎么验证 |
| --- | --- | --- |
| 1 选端口面与能力边界 | 一份能力清单（支持/不支持） | 每条能力都能指出实现位置 |
| 2 写清单 | `plugins/<id>/manifest.json` | `plugins` 子命令能解析 |
| 3 写适配器 | `plugins/<id>/adapter.rs` | `cargo build` 通过 |
| 4 接入构建 | `engine/io/adapters/mod.rs` 的 `#[path]` | `io_contract` 白名单同步 |
| 5 契约测试 | `rust/tests/<id>_contract.rs` | 先红后绿 |
| 6 启用与观测 | `plugins.json` 条目 | 状态里能读到路由与健康 |
| 7 验收 | 全量 + clippy + 边界扫描 | 见"验收清单" |

## 1. 选端口面与能力边界

先回答两个问题，再写一行代码：

1. 这个插件接入的是哪一面？（消息收发是 `ChatPort`，模型调用是 `ModelPort`，以此类推）
2. 这一面里，哪些命令你**真的**实现，哪些明确不支持？

能力顺序不能反：**先实现，再声明**。仓库里有一条现成的先例——OneBot 插件是在媒体发送实现并通过契约测试之后，才把 `media` 从 `unsupported` 移到 `supported`。

如果只写清单不写实现，正确做法是把它标成骨架：

```json
{ "status": "skeleton" }
```

系统会认识这个状态并拒绝启用它。骨架不是失败，而是"承诺还没兑现"的显式标记。

## 2. 写清单

清单是插件对外的唯一声明面，字段与含义如下：

| 字段 | 含义 | 常见错误 |
| --- | --- | --- |
| `name` | 端口名，不能为空也不能含 `:` | 与目录名混用 |
| `kind` | 端口面：`Chat` / `Model` / `Db` / `Memory` / `External` / `Persona` | 写小写或写错面 |
| `transport` | `InProcess`（编译进内核）或 `Sidecar` | 声称 Sidecar 但没有协议实现 |
| `core_api_version` | 与内核的兼容区间 | 写成具体版本号 |
| `capabilities.supported` | 已实现并验证的能力 | 把"计划做"写进来 |
| `capabilities.unsupported` | 明确不做的能力 | 与 supported 重叠 |
| `dependencies` | 依赖的其他端口角色 | 用它表达安装顺序 |
| `config_schema` | 可配置字段 | 把密钥写成普通字段 |
| `host_permissions` | 宿主活动声明，供审计展示 | 当成权限门禁 |
| `hot_reload` | 能否在不重启的情况下替换 | 声明 `true` 但没有重载实现 |

`version` 统一 `0.1.0`，它表示"插件契约版本"，不是仓库版本——不要拿仓库版本号冒充插件版本。

`host_permissions` 是**信息，不是门禁**：空列表不会阻止执行，非空列表也不会自动授权。真正的约束来自代码与端口契约。

## 3. 写适配器

适配器要实现端口 trait 的全部方法。以 `ChatPort` 为例，五个方法各有分工：

```rust
impl ChatPort for EchoChat {
    fn manifest(&self) -> &PortManifest { &self.manifest }
    fn capabilities(&self) -> Capabilities { /* 诚实的能力 */ }
    fn state(&self) -> PortState { /* connected / online / self_id / degraded */ }
    fn deliver(&self, command: Command) -> BoxFuture<PortReply<CommandResult>> { /* 命令 */ }
    fn attach(&self, sink: Sender<PortEvent>) -> Result<(), IoError> { /* 单次登记 */ }
}
```

### 构造与校验

构造时就该拒绝明显非法的输入，例如空端口名或含 `:` 的名字。让错误在构造期出现，比在运行时散落各处更好定位。

### 状态

`PortState` 有四个字段：`connected`、`online`、`self_id`、`degraded`。

- `self_id` 未知就是 `None`，**不要用空字符串冒充未知**；
- "已连接但离线"与"未连接"必须能区分，前者 `degraded` 给一个稳定短码（例如 `qq_offline`）；
- 状态是给内核做判断用的，不是给页面装饰用的——`degraded` 会影响参与与发送闸门。

### 命令

三个返回语义必须分清，这是最容易写错的地方：

| 返回 | 含义 | 例子 |
| --- | --- | --- |
| `Ok(CommandResult::…)` | 做了，且能给出结果 | 发送成功并带回 `message_id` |
| `Unsupported { capability }` | **这个端口不做这件事** | 示例插件不支持撤回 |
| `Failed { code, uncertain }` | **试过但失败** | 超时、断连、协议拒绝 |

`uncertain` 只有两种合法含义：

- `false`：确定没发出去（例如校验失败、离线且尚未调用）；
- `true`：可能已经发出去了（例如超时、断连后收不到 ACK）。

群聊里"发出去了"撤不回来，所以这条不能糊。任何"可能已发出"的失败都不能被上层当成成功，也不该自动重发。

### attach 与背压

`attach` 只接受一次 sink，第二次必须返回 `sink_already_attached`。入站事件通过这个 sink 交给运行时，因此有三条要求：

- **单流保序**：消息与 notice 走同一条流，不要为每条事件另开通道；
- **不要在 attach 里为每条事件开任务**：顺序会乱，排查困难；
- **队列满时不要静默丢帧**：要么等待容量（保序背压），要么把溢出变成可查询的故障。

`try_send` 在满队列时会直接失败；如果没人看这个错误，就等于静默丢消息。

### P-5：插件不得依赖内核实现

插件源码里禁止出现：

| 禁止 | 原因 |
| --- | --- |
| `crate::` | 直接引用内核实现，端口就不再中立 |
| `super::super` | 越过适配器边界，回到内核 |
| `include!` | 隐藏源码，绕过审查 |
| `#[path]` | 隐藏模块，绕过模块清单 |

验证方式是一条契约测试，扫描 `plugins/**` 与 `engine/io/**`：

```bash
grep -rn "crate::" plugins rust/src/engine/io
```

输出必须为空。插件只能通过 `super::{ports, types}` 使用端口契约本身。

## 4. 接入构建

`InProcess` 插件要进入构建，必须在 `rust/src/engine/io/adapters/mod.rs` 里登记：

```rust
#[path = "../../../../../plugins/example_echo/adapter.rs"]
pub mod example_echo;
```

同时，`rust/tests/io_contract.rs` 里有一份**精确的** `#[path]` 白名单。它不是随手写的列表，而是用来防止"偷偷藏一个模块"的断言：

- 白名单必须与 `adapters/mod.rs` 完全一致（顺序与内容）；
- 数量必须等于实际 `#[path]` 行数；
- 除 `adapters/mod.rs` 之外的文件出现 `#[path]` 即失败。

所以**加插件必须同步改白名单**，只改一半会直接红。这条设计有意让"新增模块"成为一次显式动作。

## 5. 写契约测试

契约测试应当钉住"作者必须遵守的语义"，而不是实现细节。建议至少覆盖：

| 用例 | 断言 |
| --- | --- |
| 入站可达 | attach 后投递一条事件，sink 收到且字段正确 |
| 命令成功 | `Send` 返回 `Sent`，且能观察到发送记录 |
| 能力诚实 | 声明的能力与实现一致；未实现的命令返回 `Unsupported` |
| 重复 attach | 第二次返回 `sink_already_attached` |
| 未 attach 即投递 | 返回明确错误，不是 panic |
| 不依赖 raw | `Incoming.raw` 为 `None` 时语义不变 |

不要把这些写进契约测试：内部字段名、日志文案、私有函数调用次数。它们会让重构变成改测试。

## 6. 启用与观测

启用是配置动作，不是代码动作。在 `plugins/plugins.json` 里加条目：

```json
{ "id": "example_echo", "directory": "example_echo", "enabled": false,
  "params": {}, "bindings": {}, "secret_refs": {} }
```

启用后要看三处，而不是只看"插件已加载"：

1. `status.json.interfaceAvailability`：三类外部接口的 `route` 与 `health`；
2. `ingressSource`：入站实际来自 `chat_port` 还是 `legacy`；
3. 结构化日志：`external_interface_availability_changed`、`event_rejected`、`no_available_io`。

当初次接管还没完成、或插件被关闭时，状态必须如实说明：

- 回落到内置实现 → `route: built_in`、`health: degraded`，并记一次告警；
- 完全没有可用实现 → `health: missing`，并记 `no_available_io`。

不允许出现"页面说已接管、实际仍走旧路径"的半完成状态。

`hot_reload: false` 的插件，启停与连接配置变更都要求整体重启。不要在运行中的两条连接之间热切，否则容易出现"旧代已停、新代没接上"的窗口。

## 7. 验收清单

```bash
cd rust
cargo test -- --test-threads=1          # 必须串行：并行会因资源竞争偶发失败
cargo clippy --all-targets -- -D warnings
grep -rn "crate::" ../plugins src/engine/io
git diff --check
```

逐条说明：

- **串行测试**：并行执行会出现随机失败，仓库已约定始终 `--test-threads=1`；
- **clippy 零警告**：`-D warnings` 是硬门槛，不要用 `#[allow]` 掩盖；
- **边界扫描**：输出必须为空；
- **`a_memorix` 偶发**：gRPC 相关失败通常是环境问题（服务未运行），单跑确认，不要为它改代码。

## 8. 常见坑

| 症状 | 原因 | 正确做法 |
| --- | --- | --- |
| 插件显示已启用但没效果 | 只改了清单没接线，或清单未进入运行代 | 看 `interfaceAvailability` 与 `ingressSource` |
| 能力显示支持但调用失败 | 声明早于实现 | 先实现并通过测试，再改 `supported` |
| 撤回失败被当成成功 | 用 `Ok` 表示"没做" | 不做的能力返回 `Unsupported` |
| 超时后消息重复 | 把 `uncertain` 当 `false` 并自动重试 | 保留 `uncertain: true`，不重发 |
| 入站顺序错乱 | 每条事件单独 spawn | 单任务串行读取并投递 |
| 高负载下丢消息 | `try_send` 满队列失败被忽略 | 等待容量或记录可查询故障 |
| 契约测试红在"隐藏模块" | 只改了 `adapters/mod.rs` 没改白名单 | 两处同步 |
| 教程里的代码无法编译 | 示例没接入构建 | 见下一节：要么接线，要么显式声明不参与构建 |

## 9. 示例插件的边界

`docs/examples/example-io-plugin/` 下的源码**不参与构建**。原因是明确的：

- 一旦接入 `adapters/mod.rs`，它就成为生产二进制的一部分，并被迫进入 `io_contract` 的精确白名单；
- 一个"永远不连任何东西"的 ChatPort 留在部署面里，只会增加误用风险；
- 为它跑全量测试，验证的只是它自己，对真协议零覆盖。

所以示例的定位是**文档资产**：它展示形状与边界，真实实现请以 `plugins/onebot/` 为参照。

如果你确实要求示例可编译，那就没有"仅示例"这回事了——它必须按第 4 步接线、按第 5 步写契约测试、并进入每次全量验收。这个代价要提前认。
