# X4 + X6 + X7：行为插件合并设计（草案，2026-10-14）

依据：`docs/working/thinking-module-architecture.md` §2–5、`TODO.md:880-910,952-1006,529-551,635-658`、`docs/working/io-port-plan.md` §5.1/§9。本文定义目标契约和实施顺序，**不是**对现有代码已具备插件能力的描述。当前 `Engine` 仍直接使用 `Store`/Provider/Transport；`io/` 的六端口聚合尚未接管整条思维链（架构 §1）。

## 1. 分界：内核拥有事实与副作用，行为拥有选择和变换

| 领域 | 内核保留、不得替换或关闭 | 可组装行为及边界 |
|---|---|---|
| 消息身份与时间 | `ChatAddress`/账号隔离、协议 ID 与本地 `message_key`、服务端 `ts` 与 `received_at` 的各自语义、去重与入库顺序、活动窗口及单调调度时钟；回填只作感知。 | 协议解析是 `ChatPort` 适配；观察、OCR、短期记忆 capture 可做行为，但只能读内核验过的证据，不得改身份/时间/去重结论。 |
| 安全与隐私 | 最终实际发送内容的结构/长度/禁用标签/内容红线、外部资料信任标签与注入边界、聊天/账号授权、跨群数据授权、敏感配置脱敏。外部内容未过审核不得进入模型可信指令区。 | 风格、上下文、外部话题、学习、候选与表达可插；其输出始终是不可信提案，交内核重新验证。插件不能声明免检。 |
| 提示词契约 | 宿主拥有 FORMATION/EVALUATION/FORECAST/ARTICULATION 的角色、系统基座、结构化字段及版本；插件不能覆盖基座或把引用数据升格为 system。 | 行为可提供受限的上下文片段、候选或结果变换，按获授权的 slot 注入，须符合该 slot 的 schema 与预算。独立模型调用须显式声明依赖。 |
| 决策和发送账本 | 最终配额/静默/作息/在线/参与/版本/epoch/活动块复核、同 chat 单飞和并发名额、`send_assessments` 去重、`deliveries=pending` 先于发送、ACK 后记自身消息、不确定不重发。 | 回复/话题/media/pause 的资格算法、候选形成/评价/选择、预测概率、长度/装饰/目标建议可替换；它们只能提出 `Permit/Deny/Proposal`，最终准入仍在宿主。 |
| IO 与运行环境 | 注册、依赖绑定、权限授予、资源限制、协议校验、状态查询和审计由宿主执行。`DbPort` 的本地事务句柄仍是进程内例外。 | `Chat/Model/Memory/External/Persona` 负责进出信息；行为负责处理已有事实并提出动作。两类插件可共用包装和服务依赖机制，但接口类型不同。 |

**两套接口、一个插件目录。** 用共同的 `PluginEnvelope { id, api_version, transport, dependencies, config_fields, host_permissions }`（复用 AB2 的字段语义），其中 `kind=port|behavior`。Port 继续使用 `PortManifest/PortReply`；Behavior 增加处理 slot、输入输出 schema、偏序与状态声明。`dependencies` 只声明需求，不授予权限；绑定由用户配置、宿主校验并注入。`host_permissions` 经用户/部署授权后由宿主逐调用执行；声明本身也不授权。宿主绝不把完整 `Io`、原始 `Store`、发送句柄或密钥交给行为插件。与 `io-port-plan.md:563` 一致。旧 `PortManifest.kind` 仅有六面（`rust/src/engine/io/types.rs:298-344`），具体是扩成带类别的通用包装，还是在其旁新增 `BehaviorManifest` 并共享嵌入字段，**待确认**；不得把 Behavior 假装成第七个 IO 端口。

## 2. 行为契约草案

下列是设计用 Rust 形状，不要求进程外插件用 Rust 或 SDK。序列化结构需生成版本化 JSON Schema；宿主验证每一次调用的输入和输出。插件返回结果不可含直接发送命令、任意 SQL 或可执行代码。

```rust
struct BehaviorManifest {
    base: PluginEnvelope,
    behavior_api: VersionReq,
    slots: Vec<SlotDecl>,          // 单一或多个明确 slot，不接受任意阶段名
    state: Option<StateDecl>,      // namespace/schema hash/migration policy
    failure_policy: FailurePolicy, // AbortTurn | SkipOptional；宿主可收紧
}
struct SlotDecl {
    slot: SlotId,
    consumes: Vec<ArtifactType>,
    produces: Vec<ArtifactType>,
    after: Vec<NodeId>,
    before: Vec<NodeId>,
    exclusivity: Option<String>,   // 触发路由等互斥分支
    needs: Vec<PortDependency>,    // AB2: role/needed_for/required + 绑定实例
    effect: EffectClass,           // Pure | ModelCall | HostStateRead | HostStateWrite
    invalidates: Vec<ArtifactType>,
}
trait Behavior: Send + Sync {
    fn manifest(&self) -> &BehaviorManifest;
    fn run<'a>(&'a self, input: BehaviorInput, host: &'a dyn BehaviorHost)
        -> BoxFuture<'a, BehaviorReply<BehaviorOutput>>;
}
trait BehaviorHost: Send + Sync {
    fn call_model<'a>(&'a self, binding: &str, request: ModelRequest<'a>)
        -> BoxFuture<'a, PortReply<ModelReply>>;
    fn read_state<'a>(&'a self, namespace: &str, key: &str)
        -> BoxFuture<'a, Result<Option<Vec<u8>>, HostError>>;
    fn propose_state_write(&self, write: StateProposal) -> Result<(), HostError>;
    fn log(&self, event: &str, data: SafeLogFields);
}
```

`BehaviorInput` 是只读、最小必要字段的不可变 `TurnSnapshot`（chat 地址与作用域、`message_key`、`version`、活动/观察/学习 epoch、触发意图、经过权限筛选的历史/候选/文本及 provenance）；不含 `MutexGuard`、数据库连接、`ChatPort`、完整 `Io` 或密钥。`BehaviorOutput` 是 slot 专属 tagged union，如 `Eligibility`, `ContextFragment`, `Candidates`, `Ratings`, `TextRevision`。宿主拥有 artifact 版本号与污点标记。外部行为拿到的是脱敏快照，sidecar/remote 只能经 `BehaviorHost` 反向服务访问已绑定依赖；`ModelPort` 调用经过宿主模型预算、超时和权限检查。`EffectClass` 只定义可请求的**宿主服务**，不可信插件的进程级联网/文件访问仍须由沙箱和部署策略限制，见 §4。

行为调用分两段：宿主在无 Core/Store guard 时构造快照并释放锁；调用插件（可 `await`）；返回后重取 **Core→Store**、核对 `version/epoch/activity`、验证 schema/权限/预算，然后在宿主事务里提交提案。需要原子判定的节点（去重、`busy` 占位、配额与 delivery 预留）只能由宿主内建执行，不暴露同步锁回调。明确禁止插件持锁/返回 guard、在事务内发网络请求、直接访问 SQLite/ChatPort；即使进程内实现也只得此 trait。这把架构 §5 第 8 条落实为 API 形状和宿主调度规则，而非插件的自律承诺。

## 3. 偏序：由宿主编译并逐次校验的有向无环图

采用**类型化 artifact DAG + 互斥路由**。`after/before` 允许表达插件间偏好；语义必需边由**宿主 policy graph 固定**，插件只能增加边，不能删除、覆盖或重定义核心节点。每个节点声明 `consumes/produces`、scope、`invalidates` 和 effect。宿主从启用状态与触发路由生成该配置的图，做拓扑排序、类型检查、可达性检查及安全节点的全路径覆盖。循环、缺生产者、缺依赖、多个互斥生产者、无安全覆盖、非法插入位置均在**配置保存/启动前报错**，不运行旧图以外的隐式 fallback。配置/插件健康变化时重新编图为新 generation；在途 turn 固定 generation，并按版本/epoch 取消，旧 generation 排空后卸载。

核心节点用稳定 ID，核心边如下。`B:` 是可替换行为槽，`K:` 为不可关闭的宿主节点；符号 `A → B` 表示 B 执行须有 A 的有效结果。分支条件是显式路由，不代表两支必须都运行。

| 架构 §5 | 固定边/图约束 | 拒绝的错误组合 |
|---|---|---|
| 1 去重先于感知 | `K.normalize → K.authorize → K.deduplicate → B.capture/observe/pending`；主人命令 `K.deduplicate → B.command` | 在去重前写记忆、重复命令执行；回填到 `pending` 的边非法。 |
| 2 版本、失效与原子调度 | `B.pending → K.reserve_chat(version,due,busy,concurrency) → B.async_* → K.revalidate(turn_token) → K.commit_*`；每次异步之后、每次持久写/发送前插入新 revalidate；校验是**支配节点**，不能被旁路。 | 异步结果直接写学习/发言，或同 chat 两轮同时占位。 |
| 3 回复优先、主动边界、积压 | `B.reply_gate` 与 `B.topic_gate` 独立产资格，`K.route_reply_first` 决定唯一触发；topic 还需 `K.handled_match`；`K.intent_classify` 固定 reply/proactive 用于配额；`B.backlog_digest` 只产生 `PromptHistory`，不能覆盖 `EvidenceHistory`。 | 把 pending 回复改成主动计数；未处理消息时启动话题；简读片段作为学习证据。 |
| 4 观察/外部审核 | `B.orientation_ready → K.speak_admission`；`B.external_fetch → K.untrusted_audit → B.context_inject`，且注入结果不能具有 system trust。 | 未观察完发言、未审核外部资料进入 FORMATION。 |
| 5 模型主链和 assessment | 两支均先经 `B.formation → K.validate_candidates → B.evaluation → K.validate_ratings → B.selection`。`sending.enabled=true` 固定走 `B.forecast → K.assess_once → B.articulation`，`K.assess_once` 支配该支表达；`sending.enabled=false` 固定走显式 `K.no_assessment_route → B.articulation`，不经 forecast、抽签或 `K.assess_once`，对应现有代码的无 assessment 分支。两支互斥，且都继续进入表达后安全校验。 | 无候选评价、开启发送时重复或绕过 assessment；关闭发送时把无 assessment 路由藏成隐式旁路。 |
| 6 表达后安全与目标 | `B.articulation → B.text_revision* → K.canonicalize_segments → K.validate_final_output → B.repetition/topic_lifecycle/targeting → K.final_admission`；所有会改变待发内容的节点必须位于最终校验前，或使 `ValidatedOutput` 失效并重走校验。`topic_lifecycle` 消费已选候选与实际表达。 | 在安全校验后润色并发送旧校验结果；用新想法时间代替相关话题时间。 |
| 7 投递账本 | `K.final_admission → K.reserve_delivery(pending) → K.delay → K.revalidate_segment → K.deliver → K.classify_ack → K.record_outbound`，按气泡逐次执行；不确定态无重放边。 | 先网络发送后建账；延迟后不复核；未确认便记自身消息。 |
| 8 并发和锁 | 对同 chat 的 `K.reserve_chat` / `K.commit_*` 标原子阶段，唯一锁序 `Core→Store`；`B.*`、IO、模型全部只能在 `NoLockAsync` 阶段。编图器拒绝 `EffectClass` 与 slot 阶段不匹配；宿主实现不把锁传给插件。 | 插件在 Core/Store guard 内 `await`，或反序拿锁。 |

**编图算法**：先解析 schema/版本/权限/依赖 → 选择每个 `exclusivity` 组的唯一实现 → 将固定核心节点与启用插件节点合并 → 按 consumes/produces 建数据边，叠加核心边和插件附加边 → 检查 DAG/类型/必要支配关系（`K.validate_final_output` 支配每个 `K.deliver`、`K.reserve_delivery` 支配每个发送）→ 枚举每条可达触发分支做覆盖检查 → 固定排序生成 `graph_hash`。动态关闭、降级、超时或 `SkipOptional` 不能把节点直接从运行中图抹去；必须切到预先编译且覆盖同样安全边的备用分支，否则中止本轮。`after/before` 引用禁用节点时，必要边报错；可选边仅在清单显式标 `optional_order` 且两节点无数据依赖时才忽略并报告。宿主状态 API 返回 effective graph、节点顺序、固定边来源、禁用/拒绝原因。

插件位置示例：X5 声明 `after=[B.articulation]`, `before=[K.canonicalize_segments]`, `consumes=[ArticulatedDraft]`, `produces=[TextRevision]`, `invalidates=[ValidatedOutput,TargetingDecision,TopicDecision]`。即使用户拖到安全校验后，编图器也拒绝；若未来允许校验后变换，宿主自动插入重新 canonicalize/validate/targeting/最终闸门，不能沿用旧许可。

## 4. X6：语言无关的发现、协议与失败编码

安装单元是目录或归档，含 `plugin.json`、协议 schema 版本、签名/摘要、入口描述、资源限额及按平台选择的可执行文件或远程 endpoint。宿主从配置的插件目录扫描，校验 manifest/API 兼容/路径和摘要/管理员授权，再创建 sidecar 或受控 remote 连接；不执行 `pip/npm/cargo install`，也不要求 SDK。进程内实现是受信任内建组件的部署方式；“任意语言”靠 **sidecar/remote 只依赖公开协议**实现。进程外不能假设同机文件、原始 socket、SQLite 连接或 Rust 类型。安装启用分开：发现只读，授权/绑定依赖/激活由宿主事务完成，失败保持旧有效图。

协议 v1：UTF-8 JSON，独立请求/响应信封；sidecar 使用长度前缀帧的 stdio，remote 可用 HTTP，但**HTTP 状态不承担业务失败**。二者字段语义相同。每次请求有 `protocol_version`, `request_id`, `plugin_id`, `graph_hash`, `slot`, `turn_token`, `deadline_ms`, `input_schema`, `input`；每次响应有匹配 ID/version、`output_schema` 和以下 tagged union。返回大小、深度、字段白名单、数值范围、字符串长度和 UTF-8 都由宿主校验，未知/缺字段默认拒绝；插件自报 `schema` 版本只用于兼容选择，不代替验证。宿主还验证其输出是否属于该 slot 和当前会话/账号，并重新计算不可由插件决定的 `chat/message_key/time/intent`。

```json
{"status":"ok","result":{"...":"slot-specific"}}
{"status":"unsupported","capability":"style_tuning"}
{"status":"degraded","result":{"...":"slot-specific"},"missing":["tone_control"]}
{"status":"failed","code":"model_timeout","uncertain":false,"retryable":false}
```

这四态对齐 `PortReply` 的 `Ok/Unsupported/Degraded/Failed`，但 Behavior 有自己的结果 schema。`degraded` 必须带已验证、可独立使用的结果；不能拿它表示“也许完成”。`uncertain=true` 仅表示外部副作用结果未知，宿主按该 slot 的不重试规则处理；对于纯文本变换超时，`uncertain` 不赋予发送权。传输失败、超时、崩溃或无效 JSON 由宿主生成 `failed`，错误码如 `protocol_invalid`, `schema_invalid`, `dependency_unavailable`, `deadline_exceeded`，不信任插件自报的 `retryable` 来决定发送重试。可选行为的失败可走**预编译的安全跳过分支**；必需行为失败中止相应 turn 并留痕。状态页分别显示 installed / configured / enabled / bound / effective / healthy / degraded / disabled_reason / graph_hash 和最近错误；`Unsupported` 不是“成功但空结果”。

权限执行：进程外插件只看最小输入，宿主托管模型/网络/状态调用并逐次校验授权；sidecar 用 OS 沙箱或等价隔离阻止绕过 `BehaviorHost` 的直接网络/文件/进程能力。远程插件无法由本机沙箱约束自身环境，因此发送给它的字段必须先经隐私授权和最小化；远程启用需明确显示数据范围。侧车/远程安装的信任和签名机制具体采用何种发行密钥，**待确认**，不影响协议可实现性。

## 5. X7：开关、交互矩阵和默认组合

开关以**组件实例**为单位：`requested_enabled`（用户意图）与 `effective_state`（运行结果）分开，另有 `reason`、绑定依赖、图版本和最近错误。端口实例也可选择停用，但仍被启用行为依赖时必须先切换绑定或拒绝停用；核心节点没有开关。默认配置从当前已验证路径生成固定 `baseline_graph_hash`，新行为（含 X5）安装后默认关闭；现有 `threeLayerDecision=true`、`sending.enabled=true`、观察/简读等值以部署的配置快照为准，**不在升级时悄悄改写**。旧配置转换须显式映射并展示 effective graph；未能等价映射则拒绝自动启用。每次保存先离线编图和预检依赖，原子切换 generation；不能为了“能启动”静默退回另一条发言路线。独立于可选组件的安全校验、最终发送闸门、隐私隔离始终开启。

| 关联开关对/组 | 合法组合与语义 | 非法或必须显式报告 |
|---|---|---|
| `reply_gate` / `topic_gate` / `legacy_pause` / `media_trigger` | 配置一个明确的**触发路由 profile**：当前三层模式=回复优先+话题；旧模式=pending 回复+media 优先+pause。也可明示某类触发关闭；至少一个触发器启用才会思考。 | 关闭三层**不能**自动解释成无决策，也不能在 X7 中静默切旧模式；profile 变更需展示新的分支和配额语义。无触发器=明确 `disabled_no_trigger`。 |
| `topic_gate` / `thought_reservoir` / `topic_source` / `relay` | 主动话题至少有一个合法候选来源；来源均关时可保留话题组件但 effective 为 `blocked_no_source`。 | 缺想法来源仍抽主动话题签。 |
| `media_trigger` / `media_select` / `ChatPort` 媒体能力 | 三者齐备方可媒体发送；媒体有独立 `media_cycle` 和账本。 | 关闭文本三层便暗中启媒体；端口无媒体能力却返回“已启用”。 |
| `forecast` / `sending_probability` / `send_assessment` | 预测与概率可选，但其关闭只能切到显式 `no_forecast_policy`；assessment 去重与最终发送闸门不可关。 | 关闭 `sending` 后直接跳过幂等账本。 |
| `orientation` / `observation` / `backfill` | 观察闸门若启用，资料采集/计数必须有可用来源；回填可独立开关且只感知，不设 pending。 | 观察关掉时旧 ready 状态不能冒充本轮已观察；回填开启不能触发发言。 |
| `backlog_digest` / `history_provider` / `learning` | 简读只改变 PromptHistory；EvidenceHistory 完整提供给学习/发送判断。 | 简读替代证据并触发不完整学习。 |
| `X5` / `ModelPort` / `articulation` / `final_validator` | X5 可独立关；开时须绑定可用 ModelPort，位于表达后、最终校验前；校验器永不可关。 | 缺模型依赖仍显示 X5 生效；关闭校验来容纳润色。 |
| `multi_bubble` / `X5` / `targeting` | X5 若修改气泡，必须重建 canonical segments、重新校验长度/内容/目标；不支持气泡的 X5 可只处理单条，否则跳过并显示原因。 | 只校验 `text`，实际发送 `bubbles`；沿用润色前 target/许可。 |
| `MemoryPort` / 记忆 capture/retrieval | 两行为分别开关，读端依赖须绑定；持久写入关后历史状态保留但读取是否继续由独立开关明确决定。 | “关闭记忆”却仍从旧库检索或继续写。 |

开关状态查询须同时列 requested、effective、原因、触发路由、依赖健康和当前图，记录 `disabled_by_user`, `missing_dependency`, `unsupported_capability`, `invalid_graph`, `degraded`, `active` 等原因；配置 UI 可用 AB2 声明生成，密钥仅显示是否已配置。运行时不可用也要更新 effective 状态和审计事件，按图的失败策略处理。

## 6. 持久状态与 V/Z 更新

行为可以持有**宿主管理的命名空间状态**，不能直写共享表。`StateDecl` 含 `namespace`, `schema_hash`, `scope`（global/account/chat/turn）、最大大小、保留期、导出/删除策略；键由宿主拼接并检查账号/聊天边界，写入用提案在 freshness 复核后事务提交。`turn` 状态仅内存；可丢弃缓存也需显式声明，不把缓存当唯一事实。需要直接改现有消息、账本、观察等核心表的行为不能作为普通状态插件；须由内核 API 提供受限命令并保持不变量。

遵循 V：**无 schema 变更**且行为/状态契约兼容时才允许 `update apply`；新状态 schema、现有表迁移或 namespace 格式变化，包中声明 `requires_full_update`，宿主拒绝 `apply`，进入备份→排空→幂等一次性迁移→启动，失败恢复旧库/旧图。迁移代码由**宿主发布版本**审核并执行，插件包不得附任意 SQL 在在线进程里跑；状态导入导出须按 `schema_hash` 验证。禁用组件默认保留状态并报告 retained，卸载/清除单独授权。Z 当前 `apply` 的“已最新”判断仍有已知缺陷（`TODO.md:635-658`）；上线行为插件前应复用 full 的 revision+schema 判定，验收同时比对 `runningRevision`、安装版本、`graph_hash`、状态 schema hash，不能只以 `accepted/already_latest` 判定成功。V 的一次性迁移机制在多个独立插件升级后的扩展方式**待确认**；在确定前，持久状态 schema 变更只允许随宿主版本全量更新。

## 7. 第一个实例 X5：模型语言微调

**目标**：对 ARTICULATION 的输出做风格/语气微调，不改变要不要发言、事实、目标、消息身份或模型主链次序。初版只支持文本；气泡只有在插件声明 `multi_bubble` 能力并返回逐条文本及总长度符合 schema 时才走。默认关闭，打开时配置 `model_binding`（一个已注册 `ModelPort` 实例）、语言/风格强度与超时。复用 AB2 的 `dependencies=[{role:"ModelPort", needed_for:"style_tuning", required:true}]` 与 `config_fields`；宿主把绑定注入为 `BehaviorHost.call_model`，插件不拿模型密钥或网络句柄。模型调用计入现有预算。

```json
{
  "id":"x5.language_tune", "kind":"behavior", "version":"1.0",
  "slots":[{"slot":"post_articulation_text_revision",
    "after":["B.articulation"], "before":["K.canonicalize_segments"],
    "consumes":["ArticulatedDraft"], "produces":["TextRevision"],
    "invalidates":["ValidatedOutput","TargetingDecision","TopicDecision"],
    "effect":"ModelCall"}],
  "dependencies":[{"role":"ModelPort","needed_for":"style_tuning","required":true}],
  "failure_policy":"SkipOptional", "default_enabled":false
}
```

流程：宿主在表达后生成脱敏的 `ArticulatedDraft`，其中含实际待发的单条/气泡文本、语言偏好与受限风格指令；X5 调 ModelPort 并只返回 `TextRevision{text|bubbles, rationale_code?}`；宿主验证 UTF-8、结构、长度、气泡数和“只改允许字段”，重新构造**实际要发送的 segments**，再运行完整 `K.validate_final_output`（每条与总量、禁用标签、情绪/安全红线、来源标注），重算重复/话题生命周期/targeting，最终发送前照常重查版本、窗口、静默、配额和投递账本。没有任何“润色已通过”许可可复用。若 X5 `Unsupported/Failed/timeout`，宿主按预编译的可选跳过分支使用原表达稿，**同样重新做最终校验**；若返回 `Degraded`，只在结果完全通过 schema 与最终校验时使用。安全审查失败则中止本轮并记可查询原因，不回退到未审内容。

落地批次：①定义 artifact/slot/schema、固定 DAG 与编图器，先封装现有文本链为内建 Behavior，验证默认图逐分支与现状一致；②实现无锁快照/返回后 revalidate 和协议 runner；③接入 X5 的内建版本，再用进程外不同语言的最小实现验证协议；④接 AB2 依赖配置与 X7 状态查询；⑤通过输入/输出契约检查及**真实 `tick→ARTICULATION→X5→校验→投递` 接线验收**，包含禁用、模型缺失、超时、恶意超长/禁用标签、气泡及在途新消息。此处列的是将来实施验收，不表示本次运行测试。

批次①须分开验收：**结果等价**比较旧路径与显式新路径的决策、持久状态、投递与错误；**模型保真**逐分支对照实际触发、模型阶段与账本记录，确认对应图中有明确路由节点和边，且关闭的阶段不出现在该分支。两者不能互相替代；仅比较最终输出，不能证明图准确描述执行路径。

## 7.5 多插件迁移：逐步升级 + 明确的 EOL（用户 2026-10-14 决定）

**决定**：**允许插件独立迁移**，但采用**两步走**：
1. **逐步升级**（**不允许跨版本跳跃**）；
2. **明确声明 EOL** —— 某个版本之后，某形式的数据**不再支持**。

### 为什么这两条必须一起才有意义
| 只有"逐步升级" | 只有"明确 EOL" |
| --- | --- |
| 版本一多，**兼容矩阵爆炸**（任意旧版 → 任意新版都要支持） | 没有中间步骤，**跨版本升级仍然无解** |

**两者合起来**：

```
插件 v1.0   数据格式 F1
插件 v1.5   引入 F2，仍能读 F1        ← 兼容窗口打开
插件 v2.0   【明确声明】F1 不再支持    ← EOL
```

—— 每次只需处理**相邻一步**，而**陈旧格式有明确的死亡时间**。

### 对 V/Z（更新机制）的要求
1. **每个插件登记自己的 schema 版本**（宿主侧的 state registry）；
2. **`apply` 拒绝不兼容状态** —— 不能热更到一个"需要迁移"的版本；
3. **`full` 按插件逐个执行"相邻一步"的迁移**，**每个插件一个事务**；
4. **EOL 必须可查询** —— 用户要能问："我现在的版本，哪些格式已被弃用 / 即将弃用？"
5. **回滚要成对** —— 宿主回滚时，插件回到**对应的相邻版本**，而不是跳到某个任意版本。

### ⚠️ 仍未定（涉及产品取向，需用户决定）
**"数据不受支持"时的行为**：
**拒绝启动** / **拒绝加载该插件** / **只读降级**？
三者的失败面不同 —— 拒绝启动最安全但影响全系统；只加载该插件失败影响面小；
只读降级最温和但可能产生"看起来能用其实不能写"的错觉。

## 8. 最易出错处与待确认

1. **“重排”若只改线性列表会漏安全依赖。** 必须使用固定核心边、artifact 类型、支配关系及每分支验图；关闭组件后重新编图，不许静默跳节点。
2. **X5 会改变实际待发内容。** 校验必须针对修改后的最终 segments，而非旧 `response.text`；目标、话题与投递前时效也要重算。
3. **跨语言与锁/权限容易形成旁路。** 不给进程外插件原始 Store/ChatPort/密钥；调用时不持 Core/Store 锁，返回后做 freshness 和宿主提交；sidecar 必须有 OS 级隔离或等价部署边界。
4. **关闭组件会产生新分支。** X7 开关的每种有效组合都要有显式路由节点和固定边，并按分支检查安全覆盖；不能把被关闭节点从图上删掉后留下隐式旁路。`sending.enabled=false` 的无 assessment 路由就是这一约束的现有实例。

**待确认**：①共同 manifest 在 `PortManifest` 上加类别还是另设 `BehaviorManifest` 共享基字段；② sidecar 包签名/隔离的具体发行和部署机制，以及远程行为插件允许的数据范围；③ V 的一次性迁移在多插件状态升级后的长期机制；④基线图对各部署现有配置的转换与固定 `baseline_graph_hash` 的生成/存档位置；⑤ X5 对多气泡、引用/提及、来源标注的首版支持范围（本稿采取文本优先，其他能力显式 `Unsupported`）。这些不影响先实现固定 DAG、宿主最终校验与 v1 协议。
