# I/O 插件隔离审计

审计问题：I/O 类插件是否可能（即使是无意地）修改内核内部行为？以及反方向——内核是否反向依赖了某个具体插件？

## 结论

**方向上确实存在双向耦合，当前接口不满足"插件只传数据、不碰行为"。**

- 插件 → 内核：事件就是状态变更指令。`Incoming` 会驱动入库、学习、记忆、入群观察与 `pending/due`，最终决定是否发言；`OutboundEvidence` 会把投递升级为 `confirmed`。这属于 I/O 面的固有属性，但**接口没有、也无法区分"真实帧"与"插件编造帧"**。
- 内核 → 插件：运行时持有的是**具体插件类型**（`Arc<OneBotAdapter>`、`Arc<HttpModel>`），并调用插件专有方法（`forwarding_fault()`、`calls()`）。换一个 Chat/Model 插件必须改内核代码。
- 数据库面在生产路径上**根本没有接线**：`DbPort` 只出现在 `Io`/`PortRegistry` 里，`Engine` 直接持有 `Arc<Mutex<Store>>`。

**通过项**：内核不读 `Incoming.raw`；模型插件返回的 `tool_calls` 不会被内核执行；端口之间没有内核回调句柄。

## 审计范围与方法

只读。逐条检查端口 trait 面、内核消费事件的位置、运行时持有的类型，以及工厂交给插件的资源。

```bash
grep -rn "OneBotAdapter\|HttpModel\|PublishedPorts" rust/src/main.rs rust/src/engine/io/factory.rs
grep -rn "forwarding_fault\|\.calls()" rust/src --include=*.rs
grep -rn "\.raw" rust/src/engine --include=*.rs | grep -v "io/"
grep -rn "tool_calls" rust/src --include=*.rs | grep -v "io/types.rs"
grep -rn "DbPort" rust/src/engine rust/src/main.rs
```

## 发现

### F1（高）内核持有具体插件类型

证据：

- `main.rs:462` `model_port: Option<Arc<HttpModel>>`
- `main.rs:465` `onebot_io: Option<Arc<OneBotAdapter>>`
- `factory.rs:42/44` `PublishedPorts` 内部字段是 `Arc<HttpModel>` / `Arc<OneBotAdapter>`
- `main.rs:864` 调用 `p.calls()`，`main.rs:869` 调用 `port.forwarding_fault()`

影响：接口只在中转处是 `dyn`，运行时仍绑死具体实现。新增或替换一个 Chat/Model 插件必须改内核；插件专有方法被内核直接读取，等于内核依赖插件的内部状态。

### F2（中，固有）事件直接驱动内部状态

证据：

- `Incoming` → `ingest_incoming_resolved`：入库、学习、记忆、`orientation.observe`、`state.pending/due`（`engine/mod.rs`）
- `Notice(GroupJoined)` → `orientation.joined`、清 `pending`
- `OutboundEvidence` → `reconcile_outbound`，投递升级为 `confirmed` 且 `time_source=server`

影响：插件可以通过投递事件让内核回复、学习、确认投递。这是 I/O 面的职责，不是缺陷；**但契约必须写明"插件是可信代码，不是沙箱"**，否则会误以为接口提供了隔离。

### F3（中）入站校验是自证

证据：`ingest_incoming_resolved` 里

```rust
ensure!(incoming.self_id == self.transport.self_id(), "wrong_self_id");
```

而 `RoutedTransport::state()` 的 `self_id` 来自 `port.state()`（插件自报）。插件控制两边，等式恒成立。

影响：无法用自报的 `self_id` 校验自报的 `self_id`。应改为与内核配置中的账号比对（`config.onebot.self_id`），那是插件无法改写的独立真值。

### F4（中）数据库面未接线

证据：`engine/mod.rs` 持有 `store: Arc<Mutex<Store>>`；`DbPort` 仅被 `Io`/`PortRegistry` 引用。

影响：数据库插件当前既不能影响内核（安全），也不能被替换（`DbPort` 是装饰性声明）。这与"三类接口统一处理"的目标不符。

### F5（中）秘密解析没有命名空间

证据：`factory.rs:201/216/256` —— `request.secret_refs[...]` 经 `(host.secret)(reference)` 解析任意键名；`secret_refs` 来自 `plugins.json` 条目（`plugin_list.rs:26`），不是插件清单。

影响：插件本身不能自选引用名，解析后的值才交给构造器，所以不是插件可控漏洞。风险在于运维配置失误时，解析器不限制键范围，可能把无关凭据（如 QQ token）交给某个插件。建议按插件 id 前缀限定，或校验引用名与 `config_schema` 的对应关系。

### F6（中）工厂按**目录名**分派连接凭据

证据：`prepare_one` 的匹配键是 `(directory, kind, transport)`，`("onebot", "Chat", "InProcess")` 分支直接拿到 `host.onebot_connect`；`directory` 来自 `plugins.json`（`plugin_list.rs:22`）。

影响：目录名成为能力凭据。运维把某个插件登记在 `onebot` 目录下，它就会拿到 QQ 连接配置，即使清单里写的是别的名字。应改为按清单 `name` 匹配并拒绝重名。

### F7（低，潜在）命令结果可以合成入站

证据：`CommandResult::History(Vec<Incoming>)` 存在（`io/types.rs`），但当前无生产调用方；生产用的是 `Fetch::History` → `Fetched(Value)`。

影响：一旦有人用 `CommandResult::History` 并把结果送进 ingest，就绕过了 `attach` 事件通道及其校验，插件可用"命令返回值"伪造入站消息。建议删除该变体或明确禁止其进入 ingest 路径。

### F8（通过）内核不解释原始协议、不执行模型工具调用

- `grep "\.raw" engine` 为空：内核不读 `Incoming.raw`；
- `grep "tool_calls"` 在 `io/types.rs` 之外为空：模型插件返回的工具调用不会被执行。

## 修复建议（按优先级）

1. **F1**：运行时与 `PublishedPorts` 改为 `Arc<dyn ChatPort>` / `Arc<dyn ModelPort>`；`forwarding_fault()`、`calls()` 改为内核侧计数或中立诊断字段，不再调用插件专有方法。
2. **F3**：入站 `self_id` 校验改用配置真值，插件自报的状态只用于展示与闸门，不参与身份校验。
3. **F4**：要么把 `Engine` 改为泛型 `D: DbPort` 并接线，要么在文档与状态中明确"数据库面尚未成为端口"，不要让声明与事实不符。
4. **F5/F6**：秘密按插件命名空间解析；工厂按清单 `name` 分派并拒绝重名。
5. **F7**：删除 `CommandResult::History`，或加断言禁止其结果进入 ingest。
6. **F2 不做"修复"**：改为在契约文档里写清信任模型——I/O 插件是可信代码；接口提供的是**行为一致性**保证（事件语义固定），不是**隔离**保证。

## 附：这份审计不覆盖的部分

- 未验证插件崩溃/死锁对内核算力的影响（属于可用性，不是行为修改）；
- 未审计 `ExternalPort` / `MemoryPort` / `PersonaPort`（本次范围限定 I/O 三面）；
- 未做动态验证（只读代码取证），F1/F3 建议修复时补黑盒测试。
