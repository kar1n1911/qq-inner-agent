# I/O 端口化与插件系统 —— 实施计划（第一类插件）

**日期**：2026-10-10
**状态**：**计划 / 规格草案**（未开工）
**范围**：**只做第一类（IO）插件。第二类（决策层）插件明确不在本计划内。**
**关系**：本文取代 [`docs/PLUGIN-PORTS.md`](../PLUGIN-PORTS.md) §4；吸收 [`docs/working/audit-io.md`](audit-io.md) 的结论；落地条目见 `TODO.md` F1。

---

## 0. 一句话目标

把散在 6 个目录、20 个文件里的 I/O，收敛成 **`engine/io/` 一个目录 + 6 个端口 + 1 个聚合入口**，使"换协议 / 换模型 / 换存储 / 换人设来源"成为**插件**的事，而不是改内核的事。

---

## 1. 插件分类与边界判据（本计划的根本前提）

插件分**两类**，性质完全不同，必须分开做：

| | **第一类：IO 插件** | **第二类：决策层插件** |
|---|---|---|
| 改变什么 | "怎么收、怎么发、从哪读、往哪写" | "说什么、怎么说、看什么" |
| 落点 | `engine/io/` 的 6 个端口之后 | FORMATION / EVALUATION / ARTICULATION / 上下文组装 |
| 代表 | OneBot、Telegram（通讯）；Ollama（模型）；A_memorix（记忆）；SillyTavern 卡**加载**（人设源） | lorebook 关键词注入、自定义候选与评分维度、人格行为片段 |
| 本计划 | ✅ **做** | ⏸ **不做**（单独立项） |

### 边界判据（写死，避免分类漂移）

> **一个功能如果只改变「字节从哪来 / 到哪去」，是第一类。**
> **如果它改变「内核给模型的输入语义」（提示词内容、候选集合、评分维度），是第二类。**

用这条判据处理三个原本悬着的问题：

| 问题 | 判据应用 | 结论 |
|---|---|---|
| SillyTavern 卡**读进来** | 只改变"字节从哪来" | **第一类** → `PersonaPort.load()` |
| 卡内容**如何进入 prompt**（放哪个区、能不能覆盖基座） | 这是内核的组装策略 | **内核代码**，不是插件能力；因此分类不模糊 |
| `character_book` 关键词注入 | 改变内核给模型的输入语义 | **第二类** → 本轮只保留"落成记忆"这条第一类通路 |
| `first_mes`（机器人先说话） | 这是内核行为策略变更 | **内核议题**，不进任何端口契约 |

**因此本计划只交付：6 个端口的契约 + 端口机制 + 能力降级 + 一致性测试。不做注入器、不做候选/评分扩展、不做行为插件。**

---

## 2. 设计原则

| # | 原则 | 反面教材 |
|---|---|---|
| **P-1** | **核心语义为准，适配器吸收差异**：端口契约以我们的键空间为准；后端模型的阻抗失配由适配器解决。核心**不得**为迁就后端改自己的模型 | 为迁就某记忆后端而改 `MemoryKey` |
| **P-2** | **例子是能力包络，不是路线图**：Ollama / A_memorix / Telegram / SillyTavern 用来界定"架构必须能表达什么"，**不是**要实现的东西 | 把"先做 Ollama 验证抽象"当里程碑 |
| **P-3** | **不预定义插件的运行方式**：进程内 / sidecar / 远程由**插件自己的 manifest 声明**，内核读它 | 现在就在 `HostIo` 里定死 HTTP 还是子进程 |
| **P-4** | **可替换性由测试证明**：一致性测试 + 测试替身，不是"设计上应该能换" | 只画 trait 不写契约测试 |
| **P-5** | **`engine::io` 是零依赖叶子**：不得 `use` `Engine` / `policy` / `cycle` | 端口反向调用引擎 → 进程外插件永远做不成 |

**P-5 要可 CI 断言**（例如一个检查 `engine/io/**` 的 `use` 列表的测试，或把 `io` 拆成独立 module 并用 `pub(crate)` 收窄）。

---

## 3. 现状（决定方案形态的数字）

| 面 | 做 I/O 的代码 | 调用点 | 现有抽象 |
|---|---|---|---|
| 通讯（OneBot） | `transport/mod.rs` 675 行 | 裸 `call("…")` **8** + `send*` **10** | `OrientationTransport` / `EngineTransport` |
| 模型（LLM） | `provider.rs` 206 + `provider_transport.rs` 302 | **9** | `OrientationProvider` |
| 操作数据 | `store.rs` + `store/*` 1123 行 | **~120 SQL 点** | 无 |
| 记忆 | `memory/*` + `persona/{expression,backstory,recall}` | **~80 SQL 点** | 无 |
| 外部内容 | `topic::fetch`、`media::read_image`、`ocr::run_process` | `ureq::` **7** + 子进程 1 | 无 |
| 人设 | `agent.persona` + `agent.personality` + `identity_persona` | 2 处组装点 | 无 |

散布：I/O 横跨 **6 个目录、20 个文件**。

**关键判断**：通讯 + 模型只有 27 个调用点，收敛是机械工作；**数据面 ~200 个 SQL 点不能整体抽象**，必须拆成"操作数据"与"记忆"两半（§5.4 / §5.5）。

---

## 4. 目标结构

```
rust/src/engine/io/                 ← I/O 系统唯一所在（零依赖叶子，P-5）
  mod.rs          ← 唯一入口 Io：聚合 6 个端口
  types.rs        ← 中立类型（只用 serde/futures，可跨进程序列化）
  ports.rs        ← 6 个 trait + 通用端口契约（manifest/capability/error）
  registry.rs     ← 端口注册（manifest 驱动，P-3）
  router.rs       ← 同面多实例的路由/组合（§6）
  host.rs         ← 插件受限宿主句柄 HostIo（够说话，不够决策）
  adapters/
    onebot.rs       ← 通讯（现有代码搬家）
    openai.rs       ← 模型（现有代码搬家）
    sqlite.rs       ← 操作数据（现有 Store 搬家，SQL 原地）
    memory_sqlite.rs← 记忆（现有 memory/* 语义实现提取）
    http.rs         ← 外部内容（topic::fetch + media::read_image）
    process.rs      ← 外部内容（ocr 子进程）
    persona_file.rs ← 人设源（本地文件；卡格式解析属适配器）
    null.rs         ← 零 I/O 空实现（测试 / dry-run）
```

`Engine` 字段从 `transport + provider + store` 收敛为 `io: Arc<Io>`；`Io` 的最终形状以 §6.1 的端口组为准，§5.0 不再给出相互矛盾的单实例结构。

**每个面是「端口组 + 路由」，不是一个端口** —— 通讯面会同时跑 OneBot / 微信 / TG / Discord，详见 §6。

**关于"减少模块数量"的诚实说明**：文件数不会减少（20 → 约 15），减少的是**入口数量与跨目录散布** —— 从 6 个目录里各自为政的 ~30 个原始 I/O 调用，收敛成 1 个聚合入口 + 6 个端口组。这是本计划真正交付的东西。

---

## 5. 第一类插件：端口规格

### 5.0 聚合入口

`Io` 是六面聚合入口，最终字段类型、实例基数及路由策略见 §6.1–6.2。P1 可先注册每面一个实例，但公共访问入口仍暴露路由器，不能把 `&dyn ChatPort` 的单实例形状固化为引擎契约。六面的中立类型、trait、注册表和路由只在 `rust/src/engine/io/` 定义；适配器不得另建同名公共契约。

### 5.1 通用端口契约（所有端口共用）

```rust
/// 适配器自述（P-3）：内核只读，不预设运行方式。
pub struct PortManifest {
    pub name: String,                       // "onebot" / "openai" / …
    pub kind: PortKind,                     // Chat | Model | Db | Memory | External | Persona
    pub transport: TransportKind,           // InProcess | Sidecar | Remote
    pub core_api_version: VersionReq,       // 兼容的端口契约版本
    pub host_permissions: Vec<HostPermission>, // http / spawn / fs_read / none
}

pub enum PortReply<T> {
    Ok(T),
    Unsupported { capability: Capability },   // 一等公民：内核必须降级
    Degraded { result: T, missing: Vec<Capability> },
    Failed { code: String, uncertain: bool }, // uncertain 保留"不确定不重试"语义
}
```

P0 必须把 `CommandResult` 与 `PortReply<T>` 的层次定死：前者只承载**成功**的通讯结果（见 §12）；`PortReply<T>` 给各可替换面承载本次能力变化与失败，通讯面的 `T=CommandResult`，记忆读取的 `T=Vec<MemoryHit>`，依此类推。`DbPort` 的本地锁句柄是过渡例外。不得在两层重复定义失败态。`Degraded.result` 是已取得的成功结果，`missing` 是本次缺失的能力；没有可用结果时用 `Unsupported` 或 `Failed`。`uncertain` 只描述外部副作用结果未知（尤其发送），不表示普通查询失败。

**三条通用不变量**：
1. **能力状态随每次响应返回**，不是只在启动时查一次 —— 后端会在运行中降级（依据：A_memorix 的 `SearchMemoryResponse` 带 `degraded` / `available_channels` / `unavailable_channels`，属收敛设计）。
2. **`uncertain` 永不自动重试**（现有语义，不得丢）。
3. **适配器只持 `HostIo`，不持 `Io`**（§9），出站消息只能经 `Command` 回流内核。

### 5.2 `ChatPort` —— 通讯面

```rust
pub trait ChatPort: Send + Sync {
    fn manifest(&self) -> &PortManifest;
    fn capabilities(&self) -> Capabilities;
    fn state(&self) -> PortState;                                  // 连通 / 在线
    fn deliver<'a>(&'a self, cmd: Command) -> BoxFuture<'a, PortReply<CommandResult>>;   // 出站/查询
    fn attach(&self, sink: mpsc::Sender<Incoming>);                       // 入站
}

pub struct Capabilities {
    pub mention: bool, pub reply: bool, pub media: bool, pub forward: bool,
    pub group_card: bool, pub profile: bool, pub avatar: bool, pub signature: bool,
    pub member_title: bool, pub member_role: bool, pub is_robot: bool,
    pub history: bool, pub group_info: bool, pub members: bool,
}
```

指令集与段模型以 [`PLUGIN-PORTS.md`](../PLUGIN-PORTS.md) §3.1–3.2 为语义起点；其旧 `Incoming`、`CommandResult` 示例**不是可直接复制的最终签名**，由本文 §12 的 P0 契约补足身份、时间、查询结果和错误层次。P0 收敛成一套定义，不保留两套并行类型。

**不变量（可测）**：

| # | 不变量 | 现在违反在哪 |
|---|---|---|
| C-1 | **协议 chat id / message_id 是不透明字符串**：负数、超长由适配器判定；内核只解析自己的 `kind:<port>:<id>` 地址信封 | `send_media` 要求 `parse::<u64>()`、`send_targeted` 要求首字符 `1-9`（`transport/mod.rs:194-200, 230-236`） |
| C-2 | **段顺序由内核给定，适配器不得重排** | 现在固定 `[reply][at][text][face]` 写在端口层，需保持 |
| C-3 | **模型输出永不成为段**：适配器只接受类型化 `Segment` | 现已满足，搬家时不得退化 |
| C-4 | `Unsupported` 必须可返回，且内核每个使用点都必须能降级 | 现在无此概念，`resolve_forwards` 等功能缺失即整段不可用 |
| C-5 | `uncertain` 语义在适配器与内核两端都不得丢 | 现已满足 |
| C-6 | 入站 `message_id` 仅作协议字段；存储分配内部单调 `message_key`，回填、引用、去重、审计均不得把协议 ID 当内部主键 | `messages.message_key`、`store/identity.rs` 已落地 |
| C-7 | 入站 `ts` 是协议服务器时间；`received_at` 是本机实际接收时刻，回填与实时消息均不能用接收时刻改写 `ts` | `messages.ts` / `received_at` 已分列 |

**契约测试**：`FakeChatPort` 分别构造 `history=false`（断言内核关回填、学习门控改只看实时）、负数 id、`Unsupported{profile}`、`uncertain` 发送 —— 四条都必须**降级而非报错**。

### 5.3 `ModelPort` —— 模型面

```rust
pub trait ModelPort: Send + Sync {
    fn manifest(&self) -> &PortManifest;
    fn capabilities(&self) -> ModelCapabilities;   // json_mode / thinking_field / truncation_signal / list_models
    fn complete<'a>(&'a self, req: ModelRequest<'a>) -> BoxFuture<'a, PortReply<ModelReply>>;
    fn list_models<'a>(&'a self) -> BoxFuture<'a, PortReply<Vec<String>>>;
}

pub struct ModelRequest<'a> {
    pub system: &'a str,
    pub payload: &'a Value,
    pub format: OutputFormat,        // Json | Text
    pub max_tokens: u32,
}
pub struct ModelReply { pub text: String }
```

**必须修掉的契约缺陷（与任何具体后端无关）**：

| # | 缺陷 | 位置 | 后果 |
|---|---|---|---|
| M-1 | **凭据是必填** | `provider_transport.rs:203-205` 空 key → `SaveApiKeyFirst` | 无鉴权/本地端点**永远发不出请求** |
| M-2 | **预算是全局一张表** | `store/schema.sql:8` `calls(ts REAL)` 无 provider 维度 | 本地与云端共用 120/小时 |
| M-3 | **端点推导写死** | `provider.rs:62-88` 只有 `/chat/completions`、`/messages` | 第三种 API 形态无法表达 |
| M-4 | **`kind` 是封闭枚举** | `config.rs:462` 只允许 `["openai","anthropic"]` | 加协议要改配置校验 |

**不变量**：错误码字符串是用户可见的（日志与仪表盘），**必须保持既有取值**；截断/空正文/非法 JSON 各有独立错误码且不重试；退避序列不得变。

### 5.4 `DbPort` —— 操作数据（**非插件点**，仅统一句柄）

```rust
pub trait DbPort: Send + Sync {
    fn store(&self) -> MutexGuard<'_, Store>;   // 过渡：现有 SQL 原地可用
}
```

覆盖表：`messages` `deliveries` `outbound_message_facts` `message_identity_audit` `send_assessments` `expectations` `handled` `decisions` `calls` `group_hours` `group_orientation` `activity_rhythm` `decoration_usage` `media_*`（现有 **11 张**）`humanize_*` `identity_*`。其中 `outbound_message_facts` 的 `(chat,id)`、`message_identity_audit.chat`、媒体双地址字段都要进入 P2c 的迁移清单；审计记录不可因无法解析旧地址而静默丢弃。

**为什么只统一句柄、不统一 SQL**：这 ~120 个点是**我方私有的事务型数据**，没有第二个实现者；把它们搬进一个"仓库"只会得到一个巨大的 god object，并破坏 `store/` 现有分层。**只有出现第二个真实后端时才值得抽象。**

**不变量**：
- `Store` **不是 `Sync`**（`RefCell`/`Cell`：`group_sharing` / `ocr_enabled` / `memory_pending`，`store.rs:7-15`），必须继续用 `Arc<Mutex<Store>>` 串行化；抽象不得破坏这一点。
- 事务边界由调用方持有（现有 `immediate()` 模式不变）。
- 启动 `recover_deliveries` 只标 `uncertain`、不重发，语义不变。

### 5.5 `MemoryPort` —— 记忆面（**真正的插件点**）

```rust
pub trait MemoryPort: Send + Sync {
    fn manifest(&self) -> &PortManifest;
    fn upsert<'a>(&'a self, entry: MemoryEntry) -> BoxFuture<'a, PortReply<()>>;
    fn forget<'a>(&'a self, key: &'a MemoryKey) -> BoxFuture<'a, PortReply<()>>;
    fn retrieve<'a>(&'a self, q: RetrievalQuery) -> BoxFuture<'a, PortReply<Vec<MemoryHit>>>;
    fn reset<'a>(&'a self, scope: MemoryScope) -> BoxFuture<'a, PortReply<()>>;
}

pub struct MemoryKey {
    pub chat: String,
    pub subject: MemorySubject,     // Group | Person(qq)
    pub layer: MemoryLayer,         // ShortTerm | LongTerm | Trait | Expression | Backstory
    pub slot: String,
}
pub struct MemoryEntry {
    pub key: MemoryKey,
    pub text: String,
    pub sources: Vec<Evidence>,     // 内部 message_key + 协议 message_id + 服务器 ts；完整定义见 §12
    pub confidence: f64, pub importance: f64,
    pub expires: Option<f64>, pub keywords: Vec<String>,
}
pub struct MemoryHit {
    pub key: MemoryKey,             // 必须还原
    pub text: String, pub score: f64, pub sources: Vec<Evidence>,
}
```

覆盖表：`memory_layers` `memory_revisions` `expressions` `learned_memories` `persona_backstory(+detail)` `chat_learning` `notes`。

**两条契约测试不变式**：

| # | 不变式 |
|---|---|
| Mem-1 | **往返无损**：`upsert(key, …)` 后 `retrieve(scope)` 能取回该条，且 `chat/subject/layer/slot` 全部还原 |
| Mem-2 | **按 key 可寻址**：`forget(key)` 命中**该条**，不依赖后端自增 id |

**前置修复（换后端之前必须做）**：`external_id = 确定性(chat, subject, layer, slot)`。
现在 `memory_layers.id` 是随机 `uuid()`（`memory/mod.rs:333`），`/记住` 更把随机 uuid 塞进 slot（`owner_teaching.rs:85`），导致 `ON CONFLICT(chat,subject,layer,slot)` **永不触发** —— 同一句 `/记住` 每次新增一条。在 SQLite 下只是多几行；**换成任何以幂等键为契约的后端，同一句话会变成 N 条独立记忆被检索出来。**

**内核保留（端口不得代劳）**：形式校验、§16 自我审核、学习分诊（learn/partial/skip）、`partial→learn` 升格、跨群 `overlap` 门槛。**且端口返回结果必须再过滤一遍** —— 不信任后端守规矩（§15.4 最后一道闸）。

**已接受的语义变化**：`/忘记` 变为**异步 + 可恢复**。

### 5.6 `ExternalPort` —— 外部内容面

```rust
pub trait ExternalPort: Send + Sync {
    fn manifest(&self) -> &PortManifest;
    fn fetch<'a>(&'a self, req: FetchRequest) -> BoxFuture<'a, PortReply<Vec<FeedItem>>>;
    fn read_bytes<'a>(&'a self, src: MediaSrc, limit: u64) -> BoxFuture<'a, PortReply<Vec<u8>>>;
    fn ocr<'a>(&'a self, bytes: Vec<u8>) -> BoxFuture<'a, PortReply<String>>;
}
```

**不变量**：
- **跳转策略统一**：现在 provider 显式 `.redirects(0)`（`provider_transport.rs:169-177`），而 `media::read_image`（`media/mod.rs:442-446`）与 `topic::fetch`（`topic/mod.rs:321-324`）沿用 ureq 默认 5 跳 → 三种客户端三种答案（[`audit-io.md`](audit-io.md) S1）。
- **外部抓取/下载不得跨事务、跨 `core` 锁**：现在媒体下载发生在 `store.immediate()` 事务**与** `core` 锁内（[`audit-io.md`](audit-io.md) IO-08）—— 而同一仓库对 OCR 已明文要求"绝不持有数据库互斥锁"（`MEDIA.md:125`）。搬家时按 OCR 的正确写法统一。
- **转发展开有界且不占主事件循环**：IO-07 是 ChatPort 查询/入站调度的问题，不是 `ExternalPort::fetch` 可单独修复的；P6 与通讯面协作，按每事件展开次数/耗时设上限，将网络等待移出主循环，完成后再按原入站顺序交给内核。不能只把 `get_forward_msg` 搬到适配器。
- 预算独立于模型面。

### 5.7 `PersonaPort` —— 人设面（**只读来源，第一类**）

```rust
pub trait PersonaPort: Send + Sync {
    fn manifest(&self) -> &PortManifest;
    /// 只产出【声明式人设数据】。
    fn load<'a>(&'a self, chat: &'a str) -> BoxFuture<'a, PortReply<PersonaSource>>;
}

pub struct PersonaSource {
    pub name: Option<String>,
    pub description: Option<String>,   // → 人设区
    pub personality: Option<String>,   // → 人设区
    pub scenario: Option<String>,      // → 引用数据区
    pub examples: Vec<String>,         // → 人设区（few-shot，可裁剪）
}
```

**契约里刻意不存在的字段** —— 它们属于第二类或属内核策略：

| 不存在的字段 | 原因 |
|---|---|
| `system_prompt` | 替换系统提示词 = 改变内核给模型的输入语义 → **第二类**；且见 §7.3 硬边界 |
| `post_history_instructions` | 同上（规范里它就是"越狱位"） |
| `character_book` 的注入行为 | 关键词触发注入 = **第二类**；本轮只保留"落成记忆"通路 |
| `first_mes` 的**发送行为** | 内核行为策略（与观察期冲突），不是端口能力 |

**不变量**：`PersonaPort` 只产出**声明式数据**，绝不产出系统指令；内核负责把它放进人设区（§7.3）。

---

## 6. 同面多实例：路由与冲突处理

### 6.0 问题

上一版把每个面写成**恰好一个实例**（`chat: Arc<dyn ChatPort>`）。但通讯面现实中会**同时**跑 OneBot(QQ) + 微信 + TG + Discord。不处理冲突就会出这些问题（均已核实到具体位置）：

| 冲突 | 具体后果 | 代码位置 |
|---|---|---|
| **地址歧义** | QQ 群 `123` 与 TG 会话 `123` 在以 `chat` 为键的表中互相覆盖；`messages` 现由内部 `message_key` 作主键，仍须隔离查询、去重和关联键 | `store/schema.sql:4,13-18` |
| **路由不可判** | 引擎要发消息时无法知道走哪个端口 | — |
| **能力串味** | 全局 `Capabilities` 会让 TG 的 `history=false` 施加到 QQ，或让 QQ 的 `group_card` 被 TG 用上 | §5.2 |
| **身份串味** | 四个账号共用一份冷却与原值备份 | `persona/mod.rs:67` `CHECK(id=1)` |
| **节律串味** | QQ 与 TG 两种人群共用一个作息节律 | `store/schema.sql:32-34` `CHECK(id=1)` |
| **故障连坐** | 一个端口断线影响其它端口 | `main.rs:224` `connection: Option<JoinHandle>` |
| **红线越界** | §15 跨群共享会在**不同协议之间**发生（QQ 群记忆流进 TG 群） | `memory/sharing.rs` |
| **预算串味** | 四条链路共用一个小时配额 | `store/schema.sql:8` `calls(ts)` |

### 6.1 每面改为「端口组 + 路由策略」

```rust
pub struct Io {
    chat:     ChatRouter,        // 1..N
    model:    ModelRouter,       // 1..N
    db:       Arc<dyn DbPort>,   // 恰好 1
    memory:   MemoryRouter,      // 1..N（单写者）
    external: ExternalRouter,    // 1..N
    persona:  PersonaRouter,     // 1..N
}
```

### 6.2 各面的组合策略（必须显式声明，不允许隐式默认）

| 面 | 多实例语义 | 策略 | 冲突处理 |
|---|---|---|---|
| 通讯 | 并列多协议 | **按来源路由** | 回复走消息来的那个端口 |
| 模型 | 主备 / 按任务分工 | **选择 + 故障转移** | 仅 `ProviderUnavailable` 时转移；**`uncertain` 永不转移** |
| 记忆 | 主后端 + 只读镜像 | **单写者 + 读合并** | 写只进 primary；读用 **rank-fusion**（后端分数不可比） |
| 外部 | 多来源并列 | **扇出合并** | 按 URL / 哈希去重 |
| 人设 | 多层覆盖 | **优先级 + 按 chat 选择** | 同一 chat 只有一个胜出 |
| 数据 | 不支持多实例 | **恰好 1** | 配置校验拒绝 >1 个 |

### 6.3 通讯面：地址必须带端口维度（最硬的一条）

`chat` 现在是两段 `kind:id`，必须变成三段 **`kind:<port>:<id>`**：

```
group:qq:663614559      private:tg:123456789      group:dc:987654321
```

**为什么端口放第二段**：审计到 **31 处**非测试代码字面使用 `group:` / `private:` 前缀；放中间可保留单纯判别种类的调用点，但不能断言这 31 处**全部**不变：逐处区分“仅判种类”和“顺便解析/构造地址”。需复核的解析、构造及协议 ID 使用点**至少 18 处**，含 `transport/mod.rs` 的发送路径；必须逐处列出旧输入、预期新输入、约束及错误码。前缀存在不等于原始协议 ID 已正确拆出。

**不采用"只有一个端口时不加前缀"**：条件格式会制造两种数据形状，是歧义 bug 的温床。要么全加，要么全不加。

**迁移成本**：实测有 **33 张直接含 `chat` 字段的持久表**，另有 **2 张双地址媒体表**（源/目的 chat 均须迁移）。不能仅按 15 张表做机械 `UPDATE`：须逐表清点键、索引、外键、JSON 内地址、嵌入式来源、缓存与回填游标；特别核对 `outbound_message_facts`、`message_identity_audit`、链接共享及媒体跨 chat 关系。`group:X` → `group:qq:X` 的既有数据迁移必须保持 `message_key` 不变、`message_id` 不变、`ts` 与 `received_at` 各自不变。§5.5 的确定性 `external_id` 是同一键空间变更：**在 P2c 的单次迁移中一并重算/建立映射**，P5 只接入已迁好的语义，不能到 P5 再改一次地址键。**这是 schema/数据迁移，只能走现有 `./agent update full`（备份、排空、迁移、失败恢复）；`apply` 会拒绝 schema 变更。**

### 6.4 端口维度的传递与路由

- `Incoming` 增加 **`port` 字段**（显式携带，不靠解析 chat 反推）。
- `Command.chat` 已含端口 → 路由直接读第二段。
- 未注册端口 → `Failed { code: "unknown_port" }`。
- 路由依据是**消息来源**，不是能力匹配：回复必须走它来的那个端口。
- 无 chat 的账号级指令（登录资料、好友/群列表、资料/头像修改、健康探测）必须显式携带目标端口或绑定到来源端口；多实例时禁止静默选第一个。

### 6.5 能力必须按端口查询

全局 `Capabilities` 是错的：

```rust
// 而不是 fn capabilities(&self) -> Capabilities
fn capabilities_for(&self, chat: &str) -> Capabilities;
```

引擎据此决定：这个群能不能 `set_group_card`、能不能读历史、能不能发 @。**能力缺失只在对应端口降级，不影响其它端口。**

### 6.6 必须改成「每端口」的全局单例（实测清单）

| 现状 | 位置 | 必须变成 |
|---|---|---|
| `identity_state` 单行账号级 | `persona/mod.rs:67` `CHECK(id=1)` | 按端口（每账号一份冷却与备份） |
| `activity_rhythm` 单行全局 | `store/schema.sql:32-34` `CHECK(id=1)` | 按端口（不同人群/时区） |
| `calls` 无模型来源维度 | `store/schema.sql:14` | 按**模型端口/实例**隔离预算，不用通讯端口名冒充模型键 |
| 单连接任务 | `main.rs` 的 OneBot 连接任务 | 一组任务，独立重连，**故障不连坐** |
| 单一 `self_id` | `transport.state().self_id` | 每端口各自账号 |
| 扁平 `allowedGroups` | `agent.allowed_groups` | 端口前缀化后天然唯一（`group:qq:123`） |
| 扁平 `allowedUsers`、主人/忽略名单与账号级命令路由 | 配置、控制端及 `engine` 的账号判断 | 显式端口与账号作用域；旧值迁移时绑定默认 `qq`，不得跨端口授权 |
| 账号缓存与资料刷新 | `engine/self_identity.rs`、`persona/mod.rs` | 缓存键包括端口与账号；失联/重连不污染其他端口 |
| 回填任务、控制端收发与调试目标 | `engine/backfill.rs`、`control.rs` | 各任务/命令携带端口，按来源路由并隔离可见状态 |
| 链接共享及媒体跨 chat 关系 | `topic/relay/links.rs`、`store/media.sql` 等 | 源/目的 chat 均带端口，默认不跨协议共享 |

### 6.7 新增红线：跨协议不共享

> **群级记忆共享不得跨端口发生。**

QQ 群与 TG 群是不同人群；把 QQ 群的 `traits`/`long_term` 注入 TG 群，等价于把 A 平台的用户画像搬到 B 平台 —— 这是 §15.4 红线的**跨协议延伸**。

实现：跨群共享评估里**先比端口**，端口不同即 `overlap = 0`。本条要有目标测试。

### 6.8 顺序与身份

- **跨端口不存在全局顺序**（每端口一个 mpsc）：不得假设跨端口事件有序。
- `version` / `last_human` / `due` 仍是**每 chat**，天然安全。
- 同一人同时在两个平台 → 视为**两个独立 `subject`**，不合并身份（除非将来单独做身份解析）。

### 6.9 配置形态

`onebot.url` / `onebot.selfId` 是单实例的，需变成端口数组：

```jsonc
"agent": { "ports": [
  { "name": "qq", "kind": "onebot",   "enabled": true,  "config": { /* … */ } },
  { "name": "tg", "kind": "telegram", "enabled": false, "config": { /* … */ } }
]}
```

`name` 就是 chat 前缀里的端口名，也是 `Incoming.port` 与 `PortManifest.name` 的取值。

---

## 7. 人设面与 SillyTavern（第一类部分）

### 7.1 现状：人设没有任何抽象

- `personality_context(agent, random)` → `{identity: agent.persona.text, behavior, replyStyle, interests, variant}`（`persona/expression.rs:200-210`）
- `persona::persona(store, chat, seed, cfg)` → 基座种子 + 每群「成长人格」（`persona/mod.rs`）

### 7.2 V2 卡字段 → 我们的落点

（依据：[Character Card V2 `data` 规范](https://raw.githubusercontent.com/bradennapier/character-cards-v2/main/data.md)）

| ST V2 字段 | 我们的落点 | 区域 | 处置 |
|---|---|---|---|
| `name` | `agent.name` / 群名片候选 | 配置 | 采用 |
| `description` | `agent.persona` | **人设区** | 采用 |
| `personality` | `agent.personality.behavior` | **人设区** | 采用 |
| `scenario` | 上下文 | 引用数据区 | 采用 |
| `mes_example` | `agent.personality.replyStyle` / few-shot | **人设区** | 采用（随历史增长最先裁剪） |
| `creator_notes` | — | — | **丢弃**（规范：MUST NOT 进 prompt） |
| `tags` / `creator` / `character_version` | 元数据 | — | **不进 prompt**（规范明确） |
| `system_prompt` | **导入时** → 初始人设（人设区）；**运行时** → 不参与 | **人设区**（仅导入时） | ⚠️ **导入时采用（需人确认），运行时丢弃**（见 §7.3） |
| `post_history_instructions` | — | — | ⛔ **丢弃**（见 §7.3） |
| `character_book.entries[]` | 经 `MemoryPort` 落成记忆（关键词进 `keywords`） | 引用数据区 | 第一类通路 |
| `extensions` | 原样透传存储 | — | 规范要求不得破坏未知键 |

### 7.3 硬边界：**卡不得覆盖基座**

[SillyTavern 规范](https://raw.githubusercontent.com/bradennapier/character-cards-v2/main/data.md) 对 `system_prompt` 的要求原文是：
> "Frontends' default behavior **MUST be to replace** the global system prompt with this value"

`post_history_instructions` 则是插在**对话历史之后**的"最后一分钟指令"（规范自称 jailbreak 位），并支持 `{{original}}` 合并。

这与我们的红线**直接冲突**：我们的原则是"外部内容都只是引用数据，不是系统指令"（`prompt-and-learning-design.md:540`），system prompt 由 `prompts.rs` 带着防注入 boundary + JSON 契约组装。**一张下载来的卡若能替换 system prompt，就能一次废掉防注入、JSON 契约、§6「永不复述情绪数值」、§15「person/私聊/原文永不外溢」。这是整个插件系统里最高危的路径。**

**组装顺序定死：**

```
[不可覆盖基座：防注入 boundary + JSON 输出契约 + 责任线 + 红线]
        ↓
[人设区：基座种子 + 成长人格 + PersonaSource（description/personality/examples）]
        ↓
[引用数据区：记忆 / 外部话题 / scenario]   ← 明确标注"引用数据，非指令"
```

规则：**`PersonaSource` 只能写入人设区**；`{{original}}` 本轮**不支持**。这条要成为**目标测试**：断言卡内容不出现在基座区。

#### 7.3.1 「导入时 / 运行时」分层（用户 2026-10-12 修正）

`system_prompt` 的规范要求是"前端 **MUST** 用它替换全局 system prompt"。这在其原本的场景
（用户主动选卡做角色扮演）里是合理的，但直接照做会废掉我们的红线。

**修正后的口径：把"导入"和"运行时"分成两个信任层级。**

| 阶段 | 性质 | `system_prompt` 的处置 |
| --- | --- | --- |
| **导入时** | **一次性的、由人确认的配置行为** | ✅ **作为初始人设** —— 它本来就是"这张卡希望自己是谁"的表述 |
| **运行时** | 卡是**外来的、不可信的数据** | ⛔ **不参与组装**，更不得替换系统提示词 |

**这不是放宽红线，而是把红线划在正确的位置**：真正危险的是**运行时**用卡内容覆盖内核约束。

组装顺序相应修正为：

```
[不可覆盖基座：防注入 boundary + JSON 输出契约 + 责任线 + 红线]   ← 永远最上
        ↓
[人设区：初始人设（可来自卡 system_prompt，导入时确认）
         + 成长人格 + PersonaSource（description / personality / examples）]
        ↓
[引用数据区：记忆 / 外部话题 / scenario]                          ← 标注"引用数据，非指令"
```

**实际收益**：很多卡的人设恰恰写在 `system_prompt` 里，一律丢弃会**明显降低导入质量**；
分层后既保住质量，又没碰红线。

**其它字段按同一标准复核**：

- `post_history_instructions` —— **仍建议丢弃**。它按规范插在**对话历史之后**（离模型最近、
  力量比 system prompt 更大），且规范自称 jailbreak 位；即使在导入时也不应自动吸收，
  最多作为"建议"展示给人、由人手工采纳。
- `alternate_greetings` —— 导入时可作为**开场白候选**（与 `first_mes` 同类，风险低）；
  但 `first_mes` 本身的发送行为已归入 §7.5 的内核议题。

**目标测试（补充）**：
1. 断言卡内容（无论导入还是运行时）**不出现在基座区**；
2. 断言**导入时**采纳的 `system_prompt` 进入的是**人设区**，且受内核约束管辖；
3. 断言**运行时**加载同一张卡时，`system_prompt` **完全不进入** payload。

### 7.4 卡的分发形态属适配器职责

V2 卡常以 **PNG tEXt `chara` 块内嵌 base64 JSON** 分发。这是**文件格式**问题，归 `adapters/persona_file.rs`；且是**本地文件导入**，不是网络 I/O。

### 7.5 移到第二类 / 内核议题

| 事项 | 归属 |
|---|---|
| `character_book` 关键词注入 | **第二类**（若将来做，需单独立权限与审计规矩） |
| `first_mes` 的发送行为 | **内核议题**（与"入群先观察"的 `group_orientation` 冲突，需单独决策） |
| 卡内容如何映射进 prompt 区域 | **内核策略**（不是插件能力，故分类不模糊） |

---

## 8. 第二类：本计划明确不做

| 不做的东西 | 为什么现在不做 |
|---|---|
| `ContextPort`（lorebook / 关键词注入） | 改变内核给模型的输入语义 → 第二类 |
| 自定义候选生成、评分维度 | 同上 |
| 人格行为片段 / 提示词片段注入 | 同上 |
| 插件热加载、沙箱、进程发现 | P-3：连"插件怎么被发现"都还不该假设 |
| 任何具体后端的适配器（Ollama / A_memorix / Telegram / ST 卡运行时） | P-2：例子是包络，不是路线图 |

---

## 9. 插件安全边界

外部后端是 sidecar / 远程服务，适配器需要发 HTTP 或起子进程。**若适配器能拿到完整 `Io`，它就能绕过内核策略直接发消息、直接写记忆** —— §16 审核、§15 红线、配额全失效。

```rust
/// 插件唯一能拿到的：只够说话，不够决策。
pub trait HostIo: Send + Sync {
    fn http<'a>(&'a self, req: HttpRequest) -> BoxFuture<'a, Result<HttpResponse, IoError>>;
    fn spawn<'a>(&'a self, argv: &'a [String], stdin: &'a [u8]) -> BoxFuture<'a, Result<Vec<u8>, IoError>>;
    fn read_file<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<Vec<u8>, IoError>>;
    fn log(&self, event: &str, data: serde_json::Value);
}
```

适配器**不持有** `Io`，只持有 `HostIo`；出站消息只能通过 `Command` 回流内核（由内核决定发不发）；权限按 `PortManifest.host_permissions` 授权（P-3：只给能力面，不定通道实现）。**权限判定放在宿主实现，不信任适配器自报**；注册时验证 manifest 与宿主授予集合，逐次调用再验证目标地址/文件路径/可执行程序及资源上限。`HostIo::log` 也须脱敏和限量。P9 必须证明未授权能力无法通过侧门取得，不能只证明 manifest 能被解析。

---

## 10. 迁移步骤

按 [`docs/DEVELOPMENT.md`](../DEVELOPMENT.md)：**每步先写"可用性 + 目标"两层测试方案再动手。**

| 步 | 动作 | 风险 | 验收 |
|---|---|---|---|
| **P0** | 定契约（§12 清单）+ 两层测试方案 + P-5 的 CI 断言 | **高（规格错误会传播全阶段）** | 静态契约表与假协议的编译期/行为拒绝项均明确 |
| **P1** | 新增 `io/`（类型 + trait + registry 骨架 + `null.rs`），**不接线** | 零 | 编译过，旧测试全绿 |
| **P2** | 通讯面搬家：`transport/mod.rs` → `adapters/onebot.rs`，旧路径留 **re-export shim** | 低 | `onebot_mock` 逐字等价 |
| **P2b** | **通讯面多实例的纯结构准备**：`ChatRouter` + `capabilities_for(chat)`（§6.1/6.5）+ `Incoming.port`；P2c 前只注册/运行一个通讯端口 | **高：若提前接第二端口会污染地址与状态** | 两假端口离线测来源路由；生产单端口等价 |
| **P2c** | **地址端口化**：`chat` 改为 `kind:<port>:<id>`（§6.3）+ 一次性 schema/数据迁移；31 处字面前缀调用逐项分类、至少 18 处解析/构造/协议 ID 点逐项复核；33 张直接含 `chat` 的表 + 2 张双地址媒体表逐表登记 | **高** | `full` 迁移/回滚；身份、时间与多端口隔离目标测试 |
| **P3** | **能力动态化**：`Capabilities` 消费点接进内核（回填 / 身份外显 / @引用 / 转发 / id 校验） | 中 | 测试替身：`history=false` 降级不报错 |
| **P4** | 模型面搬家 + 修 M-1…M-4 | 中 | 10 项传输测试 + parity |
| **P5** | 数据面拆分：`DbPort`（SQL 原地）+ `MemoryPort` + SQLite 语义实现；接入 P2c 已迁好的确定性 `external_id`，核对 `message_key` 证据及跨端口隔离 | **高：记忆往返、删除、共享可能不一致** | Mem-1 / Mem-2 + 证据键往返、跨端口隔离 |
| **P5b** | **每端口单例**：§6.6 全清单（身份、节律、预算、账号缓存、名单、回填、链接共享、控制路由）逐项加端口维度；连接任务每端口一个 | **高：授权或账号状态串味** | 双端口：身份冷却、作息、控制与名单互不影响 |
| **P6** | 外部内容面搬家 + 统一跳转策略 + IO-07 展开有界/不阻塞主循环 + IO-08 下载移出事务和 `core` 锁 | **高：阻塞与锁范围是独立缺陷** | 展开次数上限、主循环继续处理、网络期间事务与 `core` 锁均未持有 |
| **P7** | 入口收敛：删 `transport/`，8 个裸 `call("…")` → 类型化 `Command`；`main.rs` / `control.rs`、账号级命令与调试收发切到 `Io` | **高：控制命令可能选错账号** | clippy `-D warnings` + 全量 + 双端口控制路由 |
| **P8** | 人设面：`PersonaPort` + 组装顺序硬边界（§7.3） | 中 | 断言：卡内容不出现在基座区 |
| **P9** | 一致性测试套件 + `HostIo` 权限面 + 注册表（**不加载任何真实插件**） | **高：宿主权限直接决定插件可触及的副作用** | §11 + 未授权 HTTP/文件/进程/发送的拒绝测试 |

**排序理由**：
- P3 必须**早于**任何第二协议实现 —— 先有抽象再生降级，等于用没被验证的抽象去接第二个协议。
- **P2b/P2c 必须先于第二个通讯适配器**：不先把"地址带端口"和"能力按端口查"做掉，第二个协议一接进来就会污染第一个。这也是"多协议并列"能成立的前提。
- P2c 是高风险迁移（33 张直接含 `chat` 的表、2 张双地址媒体表），但**不是唯一最高风险**；只能在 `full` 的备份/排空/回滚链路执行，不能使用 `apply`。确定性 `external_id` 在 P2c 同一迁移中完成，P5 验证和消费，避免按旧键写入后再次重写。

**危险点及排序修正**：P2b 若先于 P2c 放行第二端口，同名地址、共享状态与权限先被污染，事后迁移无法可靠区分来源，故两者之间设生产接线闸。P5 的记忆单写者/读合并/删除及 `Evidence` 键空间不一致，会造成跨群泄漏或不可逆遗忘，列高风险。P6 承诺顺带修 IO-07/08，但仅“搬家”不能界定转发展开次数、主循环等待及下载时的事务/`core` 锁边界，列高风险并加显式验收。P9 的 `HostIo` 若权限校验在适配器一侧或注册表漏校验，插件可绕过内核发送、读文件、起进程，列高风险。P7 的账号级控制路由同样不能仅以全量测试通过视为安全。

---

## 11. 验收方式：用异形假协议证伪抽象

1. **基线等价**：OneBot 与 SQLite 搬到端口后行为**逐字不变**（现有套件全绿）。
2. **契约测试（conformance）** —— 与实现无关，任何端口实现都得过。以下替身只验证局部分支，**不能单独证明六面可替换**：

| 替身 | 断言 |
|---|---|
| `FakeChatPort{ history=false }` | 内核关回填、学习门控改为只看实时，**降级不报错** |
| `FakeChatPort{ chat_id: "-1001234567890" }` | 负数 id 全程可用 → 证明协议约束已下沉进端口 |
| `FakeChatPort` 返回 `Unsupported{ profile }` | 身份外显跳过，不报错 |
| `FakeChatPort` 发送返回 `uncertain` | 只记账、不自动重发 |
| `FakeMemoryPort` 对 `Group` 查询返回 `person:` 级行 | 内核**回读过滤**守住 §15.4 |
| `FakeMemoryPort` 返回 `Degraded` | 内核降级不崩溃 |
| `FakeMemoryPort` 往返 | Mem-1 / Mem-2 成立 |
| `FakeModelPort` 无凭据 | P4 完成后请求照发（验证 M-1） |
| `FakePersonaPort` 的源文件带 `system_prompt` 字段 | 运行时 `PersonaSource` 不承载该字段；导入时的人为采纳只进人设区，不进基座区（§7.3） |
| **两个 `FakeChatPort` 同时注册** | 回复走**消息来源**的端口；未注册端口 → `unknown_port` |
| **两个 `FakeChatPort` 能力不同**（一个有 `group_card`、一个 `history=false`） | 能力**互不串味**：各自按自己的 `capabilities_for(chat)` 降级 |
| **两个端口的同名 id**（`group:qq:123` 与 `group:tg:123`） | 在 `messages` 里互不覆盖 |
| **两个端口的群**（QQ 群 + TG 群） | 跨群共享 `overlap = 0`（§6.7 红线，**必须有目标测试**） |
| **一个端口断线** | 其它端口照常收发（故障不连坐） |

3. **必须再做一个贯穿六面的“异形假协议”目标测试**，以同一未修改的引擎决策路径完成“入站 → 记忆读取/写入 → 模型请求 → 外部内容/人设读取 → 出站 → 持久化/回读”。它不是 Telegram/Ollama 等真实适配器，也不得用 OneBot JSON 伪装。其最小性质与反证条件如下：

| 异形性质 | 必须观察到；否则抽象失败 |
|---|---|
| 地址采用 `room:…` 一类**非数字、含分隔符**的原始 ID；中立信封仍用 `group:odd:<opaque>`，另端口有相同原始 ID | 无 `u64`/QQ 首位规则泄入内核；来源端口路由正确，同名 ID 在库及回填中隔离 |
| 入站原文不是 OneBot 的 `post_type/message/data` 形状；段含未知类型、非 QQ 提及和惰性转发 | 内核只消费中立段；未知段保留且不执行；展开按能力和上限受控，不阻塞主循环 |
| 两条事件复用相同协议 `message_id`，服务器 `ts` 与本机 `received_at` 不同且先后可逆 | `message_key` 唯一且单调；协议 ID 原样留存；`ts` 和 `received_at` 不互相代替；证据/引用以 `message_key` 定位 |
| 协议缺历史、群名片和头像，运行中能力又降级一次；发送返回一次 `uncertain` | 仅对应功能退化，其他端口继续；不重发不确定发送；账本状态与现有语义一致 |
| 模型端无凭据、端点非 `/chat/completions`/`/messages`；记忆端用非 SQLite 自增键，回传乱序/跨 scope 命中 | 模型可请求且预算隔离；记忆 key 往返和按 key 删除成立；内核过滤越界命中，跨端口不共享 |
| 外部端返回跳转/慢响应；人设端提供不可信卡内容；`HostIo` 拒绝未声明权限 | 跳转和锁边界一致；卡不覆盖基座；未授权 HTTP、文件、进程及绕过内核发送均被拒绝 |

测试必须记录每一面是否真正被替换、被内核消费的中立值、最后的发送目标及持久化行。**任一面只能靠改 `Engine`、解析协议原文或新增 QQ 特判才能通过，即为六面可替换主张的反例**。P1 的空实现、P2b 的双假端口和单面 conformance 仍保留，完整目标测试在 P9 收口；在此之前不能宣称总体抽象已验证。

4. **能力包络清单** —— 把例子转成"架构必须能表达"的核对表，逐条证明契约能表达，但不实现（P-2）：

| 包络条目 | 由什么承载 | 状态 |
|---|---|---|
| 无凭据的本地模型 | `ModelPort` 凭据可缺省 | 待 P4 |
| 与云端隔离的模型预算 | 预算按端口隔离 | 待 P4/P5b |
| 能力缺失的协议（无历史/名片/签名） | `Capabilities` + `Unsupported` + 动态 `Degraded` | 待 P3 |
| 负数/非数字 chat id | id 校验下沉进端口 | 待 P2c/P7 |
| **同面多协议并列**（QQ + 微信 + TG + Discord） | `ChatRouter` + 端口化地址 + `capabilities_for(chat)` | 待 P2b/P2c |
| **跨协议不共享** | §6.7 红线（先比端口再算 overlap） | 待 P5b |
| **每账号独立身份/节律** | §6.6 单例拆分；模型预算另按模型实例隔离 | 待 P5b |
| 远程记忆后端 | `MemoryPort` + `HostIo` + 可序列化类型 | 待 P5/P9 |
| 键空间往返无损 | Mem-1 | 待 P5 |
| 人设来自外部卡 | `PersonaPort.load` | 待 P8 |
| 卡不得覆盖基座 | 组装顺序硬边界 | 待 P8 |
| 关键词触发的上下文 | **第二类**，本计划不承载 | 不做 |

---

## 12. P0–P2 可执行清单（建议批准范围）

### P0 —— 契约与纪律（规格风险高，尚无运行时改动）

- [ ] `types.rs` 落地：`Incoming` / `Sender` / `Segment` / `MediaSrc` / `Command` / `CommandResult` / `Capabilities` / `PortState` / `PortManifest` / `PortReply` / `MemoryKey` / `MemorySubject` / `MemoryLayer` / `MemoryEntry` / `MemoryHit` / `Evidence` / `PersonaSource` / `IoError`
  - 硬要求：中立数据可用 `serde` 序列化；异步签名可用 `futures`，数据内不放闭包、锁守卫、连接句柄或 trait object。`DbPort::store()` 是明确的**进程内过渡例外**，不宣称可跨进程序列化。
- [ ] `ports.rs` 落地 6 个 trait 签名（§5）
- [ ] **P-5 CI 断言**：`engine/io/**` 不得出现 `use crate::engine::{Engine, policy, cycle}` 等
- [ ] **两层测试方案**（先写方案再写代码）：
  - 可用性层：每个端口的输入/输出正确、边界、失败路径
  - 目标层：**每步的目标测试都要注明设计依据**（哪份文档哪一行），使设计变更时测试会变红

#### P0 必须冻结的类型和 trait 语义

| 类型 / 方法 | 必填契约；实现者不得自行猜测 |
|---|---|
| `ChatAddress` | 中立信封为 `kind:<port>:<opaque-id>`，`kind ∈ {group,private}`；只拆前两个分隔符，余下原样作为协议 ID（允许冒号、前导零、负号、非数字）。`port` 为注册表唯一实例名，空段/未知端口在路由层拒绝；是否允许某种协议 ID 由对应适配器决定。P2c 之前旧 `kind:id` 只在迁移适配边界读取，不成为永久双格式。 |
| `Incoming` | `port: String`、`chat: ChatAddress`、`message_id: String`（原始协议值）、`ts: f64`（服务器时间）、`received_at: f64`（本机接收时刻）、`self_id`、`sender`、`segments`、`self_message`、`raw`。适配器不得分配 `message_key`；存储层在接收/回填时分配内部单调键并返回。服务器时间缺失/无效时沿用现有拒收与异常记录语义，不能悄然令 `ts=received_at`。`raw` 仅供端口/审计，不供决策解析。 |
| `Evidence` | `chat: ChatAddress`、`message_key: String` 为内部定位键；`message_id: Option<String>` 只作协议溯源，`ts: f64` 为服务器时间，`received_at: Option<f64>` 另列。无可解析内部键的旧证据需通过现有解析/审计路径处理，**不得**用原始 ID 冒充 `message_key`。`MemoryEntry.sources`、`MemoryHit.sources`、回填游标和引用解析必须按此往返。 |
| `Command` 与查询结果 | 有 chat 的指令按 `ChatAddress` 路由；`LoginInfo` / `Health` / `FetchFriendList` / `FetchGroupList` / `SetProfile` / `SetAvatar` 等无 chat 的账号级指令必须显式带 `port`。`FetchHistory` 返回中立 `Incoming`/历史条目，成员、群资料、好友、群列表、账号资料各有类型化结果；不得以 `Items(Value)` 暗中透传 OneBot 原文。`Sent { message_id }` 仅是协议回执，内部 `message_key` 仍由存储层分配。 |
| `CommandResult` / `PortReply<T>` / `IoError` | `CommandResult` 仅有成功的通讯结果（`Sent`、`History`、`Members`、`GroupInfo`、`Friends`、`Groups`、`LoginInfo`、`Health`、`Ok` 等）；泛型 `PortReply<T>` 独占 `Unsupported`、`Degraded`、`Failed { code, uncertain }`，其 `T` 是各方法的成功值。静态能力不足与运行时不支持都能走 `Unsupported`；运行时部分成功走 `Degraded`。错误码保留原值，`uncertain=true` 不自动重试/故障转移。内部失败用 `IoError`，在端口边界映射为保留错误码的 `Failed`。 |
| `ChatPort` | `manifest` 的 `name` 与 `Incoming.port`、`ChatAddress.port` 相同；`attach` 入站后必须尊重背压和断线状态。`deliver` 接受类型化 `Command`，返回 `PortReply<CommandResult>`；查询可经 `deliver`，但类型化结果必须匹配命令。路由器先核对目标端口再调用适配器，不让适配器自行决定别的账号。 |
| `ModelRequest` / `ModelReply` | 请求保留 `system`、结构化 `payload`、`format`、`max_tokens`，并显式指定模型实例/配置；`ModelReply.text` 保持原始文本。无凭据可请求本地端点，预算按模型实例记账；截断、空正文、非法 JSON、重试与错误码均沿用现有语义。`complete` 的 `ModelRequest<'a>` 生命周期与返回的 `BoxFuture<'a, …>` 对齐。 |
| `RetrievalQuery` / `MemoryScope` | 查询至少包含 `chat: ChatAddress`、`subject: MemorySubject`、`layers: Vec<MemoryLayer>`、`limit`；scope 至少包含同一 `chat`、`subject` 与待重置层。缺省不得扩大到所有 chat/端口；无跨群授权时禁止跨 chat 命中。`MemoryKey` 四元组 `chat/subject/layer/slot` 往返、删除与幂等写入必须一致。 |
| `FetchRequest` / `MediaSrc` / `PersonaSource` | 请求显式给定来源、超时/大小上限及允许的跳转策略，响应只含中立内容；媒体来源可为 URL/本地路径/不透明句柄，由适配器解析。人设只返回声明式 `name/description/personality/scenario/examples`；卡原文的 `system_prompt` 不成为运行时 `PersonaSource` 字段。 |
| `PortManifest` / `HostPermission` / `PortState` | `name` 是唯一端口实例键，`kind` 属六面之一，`transport` 是运行方式声明，`core_api_version` 是兼容范围；`host_permissions` 默认空且按 HTTP 目标、可执行程序、文件路径及日志限额进一步收窄。`PortState` 至少区分可用、降级、断开；每次响应的能力状态可覆盖启动时声明。 |
| 其余五面 | `DbPort` 仅统一当前 SQLite 句柄与事务边界；`MemoryPort` 保持按完整 key 寻址与回读过滤；`ExternalPort` 实施大小/跳转/超时上限；`PersonaPort` 不返回可覆盖系统基座的指令。路由/组合按 §6.2，任何未注册端口明确失败。 |

**签名收敛规则**：§5 中的示意 Rust 签名若与本表冲突，以本表为 P0 最终语义，并在 P0 把签名改成同一套可编译定义；不得把冲突留到 P2。具体包括 `ModelRequest<'a>` 与现有 `BoxFuture` 所需的借用生命周期、`MemoryPort` 返回的降级信息、`DbPort` 的 `MutexGuard<Store>` 进程内例外，以及 `CommandResult`/`PortReply` 的重复失败态。P0 结束条件是这些类型和六个 trait 可编译、序列化边界明确、路由器能拒绝错误端口；**不是仅把名称列在清单上**。

### P1 —— 纯新增骨架（零行为变更）

- [ ] `engine/io/{mod,types,ports,registry,router,host}.rs` + `adapters/null.rs`
- [ ] `Io` 只做聚合，**不被 `Engine` 引用**
- [ ] `router.rs` 落地 6 个端口组的组合策略（§6.2），先只支持"单实例"路径，多实例留桩
- [ ] `null.rs` 实现全部端口：`deliver` 返回 `Unsupported`、`retrieve` 返回空、`complete` 返回 `Failed` —— 作为 dry-run 与测试基线

### P2 —— 通讯面搬家（本阶段行为敏感点）

- [ ] `transport/mod.rs` → `engine/io/adapters/onebot.rs`，实现 `ChatPort`
- [ ] `Incoming ↔ OneBot 事件`、`Command ↔ OneBot 动作` 双向映射
- [ ] 旧路径保留 **re-export shim**（`crate::transport::OneBot`）直到 P7，保证每步可上线
- [ ] **等价性验收**：`onebot_mock` 全套逐字通过；`engine_parity` 不受影响
- [ ] `Cargo.toml` / `lib.rs` 的模块声明不破坏 `--all-targets` 构建

### P2b —— 通讯面多实例（**新增，多协议并列的前提**）

- [ ] `ChatRouter`：按 `Command.chat` 的端口段路由；无 chat 的账号级命令必须显式指定端口；未注册端口 → `unknown_port`
- [ ] `capabilities_for(chat)` 取代全局 `capabilities()`（§6.5）
- [ ] `Incoming` 增加 `port` 字段（§6.4）
- [ ] 双假端口契约测试：来源路由、能力互不串味（§11）
- [ ] **单端口配置下行为逐字不变**（可先只注册一个端口）
- [ ] **生产接线闸**：P2c 数据迁移与状态隔离验收完成前，运行配置必须拒绝第二个通讯端口；双假端口仅用于离线契约测试

### P2c —— 地址端口化（**新增，高风险 schema/数据迁移**）

- [ ] `chat` 由 `kind:<id>` 改为 `kind:<port>:<id>`（§6.3）
- [ ] 建立逐表迁移矩阵：33 张直接含 `chat` 字段的持久表、2 张双地址媒体表；逐列记录主键/索引/外键/源与目的地址、JSON/缓存/审计/回填游标，并复核 11 张 `media_*` 表的实际覆盖范围；`outbound_message_facts`、`message_identity_audit` 必列
- [ ] 把 31 处字面 `group:`/`private:` 前缀调用分为仅判类型与解析/构造两类；至少 18 处解析/构造/协议 ID 点逐处记录输入、输出、错误码，不能把 31 处一概视为无需修改
- [ ] 同次迁移完成 §5.5 的确定性 `external_id` / 旧键映射，P5 不再二次改键；迁移保持 `message_key`、协议 `message_id`、服务器 `ts`、本机 `received_at` 各自含义及数值，审计表保留可追溯性
- [ ] **只用 `./agent update full`**：备份 → UPDATE-2 排空 → schema/数据迁移 → 构建/启动；失败恢复备份和旧服务。`apply` 对 schema 变更必须拒绝，不列为可用部署方式
- [ ] **验收**：迁移前后单端口决策/投递等价；同名 ID 双端口不覆盖；双地址媒体与链接共享不串味；迁移失败可回滚；P2b 的第二端口闸仅在这些测试通过后移除

---

## 13. 未定项

1. **`Capabilities` 的形态**：P0 已定注册时静态声明 + 响应内动态 `Unsupported`/`Degraded`；实施不得只保留其中之一。
2. **P9 注册表留到什么程度**：只留 trait + 空注册表，还是连发现/沙箱都留到真做插件时再定？按 P-3 我倾向**后者**。
3. **`DbPort` 是否永久非插件点**（不换 PostgreSQL 等）？本计划按"永久非插件点"推进。
4. **`first_mes`**（内核议题，§7.5）：群聊要不要为卡破例先发言？
5. **P2 的 shim 保留到哪一步**：我在 P7 删除；如果你想更早收敛可提前。
6. **地址格式**：P0 已定 `kind:<port>:<id>`；若改变需重新审计 31 处前缀调用与至少 18 处解析/构造/协议 ID 点，不可在实施中临时切换。
7. **记忆面多后端是否允许"多写者"**：我按**单写者 + 只读镜像**设计（§6.2）；如果需要真正的多写，`forget` 的语义会分叉（一个删、一个墓碑），要重新定。
8. **是否合并跨平台身份**：我按"同一人在两个平台 = 两个独立 subject"设计（§6.8）；若将来要做身份解析，`MemorySubject::Person` 需要加端口维度。
9. **P2c 排期**：允许 P2b 先做路由骨架与离线双假端口；P2c 未完成前生产只跑单通讯端口，第二端口必须被配置校验拒绝。

---

## 14. W（utils 归一化）与端口层的职责边界

按 X1 的约束，**统一后的 I/O 类型、trait、注册表、路由全部集中在 `rust/src/engine/io/`；插件作者只需要知道这一个位置。**`utils` 只提供跨模块同义、无状态、无协议判断的纯原语，不能成为第二套端口 API 或让插件作者再去找别的目录。

| 去向 | 可做什么 | 不可借机做什么 |
|---|---|---|
| **W / `rust/src/utils/`** | 已核实同义的 JS `trim` 空白集合、UUID v4、集合重叠评分、基础取值、按 Unicode 标量截断；必要时提供 ASCII 数字、正 `u64`、地址组件拆分等**纯原语**，保持调用点原错误码与约束 | 不推出全仓通用 `valid_chat_id`，不决定 `group/private`、QQ 号/face ID、消息身份或路由 |
| **端口层 / `engine/io/`** | 中立地址信封解析、端口名校验、协议 ID 的适配器级校验/编码、能力与错误映射、入站/出站中立类型、路由；逐一收敛至少 18 个地址/协议 ID 使用点 | 不把 OneBot 的正整数/首位非零规则当跨协议规则；不改既有业务错误码或 `message_key` 语义 |
| **保持在业务调用点** | 具体授权、允许群/用户、主人/忽略名单、学习/发送闸门、记忆红线、是否允许空值及具体错误码 | 不因使用共同原语而抹平不同业务含义 |
| **暂不归并** | JSON 业务校验、`message_key` 回退、face/QQ ID 特殊规则、UTF-16 计数、段解析、迁移决策、URL 规则、tokenizer、跨运行时原子写入、模型预算 | 不在 W 中“先统一语义”，再在 P2c/P5 中重做一次 |

**顺序**：W 只收敛已证明同义的原语并保持各调用点语义；P0 冻结端口中立契约；P2c 逐点迁移地址/协议 ID；P5 处理证据键与记忆。对同名不同义的会话 ID 校验，**端口层负责定义新信封和各协议规则，W 不预先统一**。若某规则既不是纯原语也不属 I/O（如授权/记忆策略），继续留业务层。

---

## 15. 与既有文档的关系

- [`docs/PLUGIN-PORTS.md`](../PLUGIN-PORTS.md)：§3.1–3.4 是中立模型的早期分析，**最终身份/时间/地址/查询结果/错误层次以本文 P0 为准**；**§4 迁移路径由本文取代**；其 §5 待定问题中 #2（Forward 展开归属）、#3（`Fetch*` 返回形状）、#4（id 校验归属）本文已定：**Forward 协议展开归端口且受 IO-07 上限/调度约束、id 的协议校验归适配器、`Fetch*` 用中立结构**。
- [`docs/working/audit-io.md`](audit-io.md)：本文顺带修 IO-07（转发展开无上限 + 阻塞主循环）、IO-08（下载跨事务/锁）、S1（跳转策略不一致）。
- `TODO.md` F1：以本文为落地清单。
