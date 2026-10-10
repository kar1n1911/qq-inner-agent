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

`Engine` 字段从 `transport + provider + store` 收敛为 `io: Arc<Io>`。

**每个面是「端口组 + 路由」，不是一个端口** —— 通讯面会同时跑 OneBot / 微信 / TG / Discord，详见 §6。

**关于"减少模块数量"的诚实说明**：文件数不会减少（20 → 约 15），减少的是**入口数量与跨目录散布** —— 从 6 个目录里各自为政的 ~30 个原始 I/O 调用，收敛成 1 个聚合入口 + 6 个端口组。这是本计划真正交付的东西。

---

## 5. 第一类插件：端口规格

### 5.0 聚合入口

```rust
pub struct Io {
    chat:     Arc<dyn ChatPort>,
    model:    Arc<dyn ModelPort>,
    db:       Arc<dyn DbPort>,
    memory:   Arc<dyn MemoryPort>,
    external: Arc<dyn ExternalPort>,
    persona:  Arc<dyn PersonaPort>,
}
impl Io {
    pub fn chat(&self) -> &dyn ChatPort { … }
    // … 其余同形
}
```

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

pub enum PortReply {
    Ok(CommandResult),
    Unsupported { capability: Capability },   // 一等公民：内核必须降级
    Degraded { result: CommandResult, missing: Vec<Capability> },
    Failed { code: String, uncertain: bool }, // uncertain 保留"不确定不重试"语义
}
```

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
    fn deliver<'a>(&'a self, cmd: Command) -> BoxFuture<'a, PortReply>;   // 出站
    fn attach(&self, sink: mpsc::Sender<Incoming>);                       // 入站
}

pub struct Capabilities {
    pub mention: bool, pub reply: bool, pub media: bool, pub forward: bool,
    pub group_card: bool, pub profile: bool, pub avatar: bool, pub signature: bool,
    pub member_title: bool, pub member_role: bool, pub is_robot: bool,
    pub history: bool, pub group_info: bool, pub members: bool,
}
```

指令集与段模型直接采用 [`PLUGIN-PORTS.md`](../PLUGIN-PORTS.md) §3.1–3.2（`Incoming` / `Sender` / `Segment` / `MediaSrc` / `Command` / `CommandResult`），**不重造**。

**不变量（可测）**：

| # | 不变量 | 现在违反在哪 |
|---|---|---|
| C-1 | **chat / message id 是不透明字符串**：负数、超长均由适配器判定，内核不解析 | `send_media` 要求 `parse::<u64>()`、`send_targeted` 要求首字符 `1-9`（`transport/mod.rs:194-200, 230-236`） |
| C-2 | **段顺序由内核给定，适配器不得重排** | 现在固定 `[reply][at][text][face]` 写在端口层，需保持 |
| C-3 | **模型输出永不成为段**：适配器只接受类型化 `Segment` | 现已满足，搬家时不得退化 |
| C-4 | `Unsupported` 必须可返回，且内核每个使用点都必须能降级 | 现在无此概念，`resolve_forwards` 等功能缺失即整段不可用 |
| C-5 | `uncertain` 语义在适配器与内核两端都不得丢 | 现已满足 |

**契约测试**：`FakeChatPort` 分别构造 `history=false`（断言内核关回填、学习门控改只看实时）、负数 id、`Unsupported{profile}`、`uncertain` 发送 —— 四条都必须**降级而非报错**。

### 5.3 `ModelPort` —— 模型面

```rust
pub trait ModelPort: Send + Sync {
    fn manifest(&self) -> &PortManifest;
    fn capabilities(&self) -> ModelCapabilities;   // json_mode / thinking_field / truncation_signal / list_models
    fn complete<'a>(&'a self, req: ModelRequest) -> BoxFuture<'a, Result<ModelReply, ModelError>>;
    fn list_models<'a>(&'a self) -> BoxFuture<'a, Result<Vec<String>, ModelError>>;
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

覆盖表：`messages` `deliveries` `send_assessments` `expectations` `handled` `decisions` `calls` `group_hours` `group_orientation` `activity_rhythm` `decoration_usage` `media_*`(9) `humanize_*` `identity_*`。

**为什么只统一句柄、不统一 SQL**：这 ~120 个点是**我方私有的事务型数据**，没有第二个实现者；把它们搬进一个"仓库"只会得到一个巨大的 god object，并破坏 `store/` 现有分层。**只有出现第二个真实后端时才值得抽象。**

**不变量**：
- `Store` **不是 `Sync`**（`RefCell`/`Cell`：`group_sharing` / `ocr_enabled` / `memory_pending`，`store.rs:7-15`），必须继续用 `Arc<Mutex<Store>>` 串行化；抽象不得破坏这一点。
- 事务边界由调用方持有（现有 `immediate()` 模式不变）。
- 启动 `recover_deliveries` 只标 `uncertain`、不重发，语义不变。

### 5.5 `MemoryPort` —— 记忆面（**真正的插件点**）

```rust
pub trait MemoryPort: Send + Sync {
    fn manifest(&self) -> &PortManifest;
    fn upsert<'a>(&'a self, entry: MemoryEntry) -> BoxFuture<'a, Result<(), IoError>>;
    fn forget<'a>(&'a self, key: &'a MemoryKey) -> BoxFuture<'a, Result<(), IoError>>;
    fn retrieve<'a>(&'a self, q: RetrievalQuery) -> BoxFuture<'a, Result<Vec<MemoryHit>, IoError>>;
    fn reset<'a>(&'a self, scope: MemoryScope) -> BoxFuture<'a, Result<(), IoError>>;
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
    pub sources: Vec<Evidence>,     // 真实消息 id + ts
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
    fn fetch<'a>(&'a self, req: FetchRequest) -> BoxFuture<'a, Result<Vec<FeedItem>, IoError>>;
    fn read_bytes<'a>(&'a self, src: MediaSrc, limit: u64) -> BoxFuture<'a, Result<Vec<u8>, IoError>>;
    fn ocr<'a>(&'a self, bytes: Vec<u8>) -> BoxFuture<'a, Result<String, IoError>>;
}
```

**不变量**：
- **跳转策略统一**：现在 provider 显式 `.redirects(0)`（`provider_transport.rs:169-177`），而 `media::read_image`（`media/mod.rs:442-446`）与 `topic::fetch`（`topic/mod.rs:321-324`）沿用 ureq 默认 5 跳 → 三种客户端三种答案（[`audit-io.md`](audit-io.md) S1）。
- **外部抓取/下载不得跨事务、跨 `core` 锁**：现在媒体下载发生在 `store.immediate()` 事务**与** `core` 锁内（[`audit-io.md`](audit-io.md) IO-08）—— 而同一仓库对 OCR 已明文要求"绝不持有数据库互斥锁"（`MEDIA.md:125`）。搬家时按 OCR 的正确写法统一。
- 预算独立于模型面。

### 5.7 `PersonaPort` —— 人设面（**只读来源，第一类**）

```rust
pub trait PersonaPort: Send + Sync {
    fn manifest(&self) -> &PortManifest;
    /// 只产出【声明式人设数据】。
    fn load<'a>(&'a self, chat: &'a str) -> BoxFuture<'a, Result<PersonaSource, IoError>>;
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
| **地址歧义** | QQ 群 `123` 与 TG 会话 `123` 在 `PRIMARY KEY(chat,id)` 上**互相覆盖** | `store/schema.sql:3` |
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

**为什么端口放第二段，而不是最前或最后**：全仓有 **35 处**非测试代码用 `starts_with("group:")` / `strip_prefix("group:")` 判类型（`memory/sharing.rs:28`、`persona/humanize.rs:31`、`persona/affect.rs:309`、`engine/orientation.rs:208` …）。端口放中间，**这 35 处类型判断全部继续有效**；只有需要"原始协议 id"的地方要再切一刀（如 `transport/mod.rs:231` 的 `parse::<u64>()`），这类约 **10 处**。放最前或最后都会一次性打断全部 35 处。

**不采用"只有一个端口时不加前缀"**：条件格式会制造两种数据形状，是歧义 bug 的温床。要么全加，要么全不加。

**迁移成本**：一次性把既有行的 `chat` 由 `group:X` 改写为 `group:qq:X`（`qq` 为配置的默认端口名），涉及约 15 张表的主键/索引。机械 `UPDATE`，但**必须与 §5.5 的 `external_id` 确定性一起做**，否则键空间中途不一致。

### 6.4 端口维度的传递与路由

- `Incoming` 增加 **`port` 字段**（显式携带，不靠解析 chat 反推）。
- `Command.chat` 已含端口 → 路由直接读第二段。
- 未注册端口 → `Failed { code: "unknown_port" }`。
- 路由依据是**消息来源**，不是能力匹配：回复必须走它来的那个端口。

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
| `calls` 无端口维度 | `store/schema.sql:8` | 加端口维度（模型预算隔离） |
| 单连接任务 | `main.rs:224` | 一组任务，独立重连，**故障不连坐** |
| 单一 `self_id` | `transport.state().self_id` | 每端口各自账号 |
| 扁平 `allowedGroups` | `agent.allowed_groups` | 端口前缀化后天然唯一（`group:qq:123`） |

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
| `system_prompt` | — | — | ⛔ **丢弃**（见 §7.3） |
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

适配器**不持有** `Io`，只持有 `HostIo`；出站消息只能通过 `Command` 回流内核（由内核决定发不发）；权限按 `PortManifest.host_permissions` 授权（P-3：只给能力面，不定通道实现）。

---

## 10. 迁移步骤

按 [`docs/DEVELOPMENT.md`](../DEVELOPMENT.md)：**每步先写"可用性 + 目标"两层测试方案再动手。**

| 步 | 动作 | 风险 | 验收 |
|---|---|---|---|
| **P0** | 定契约（§12 清单）+ 两层测试方案 + P-5 的 CI 断言 | 零 | 断言能拦住反向依赖 |
| **P1** | 新增 `io/`（类型 + trait + registry 骨架 + `null.rs`），**不接线** | 零 | 编译过，旧测试全绿 |
| **P2** | 通讯面搬家：`transport/mod.rs` → `adapters/onebot.rs`，旧路径留 **re-export shim** | 低 | `onebot_mock` 逐字等价 |
| **P2b** | **通讯面多实例**：`ChatRouter` + `capabilities_for(chat)`（§6.1/6.5）+ `Incoming.port`；单端口时不改变行为 | 中 | 双假端口：来源路由、能力互不串味 |
| **P2c** | **地址端口化**：`chat` 改为 `kind:<port>:<id>`（§6.3）+ 一次性数据迁移；约 35 处类型判断不改、约 10 处取原始 id 的地方要切第二刀 | **高** | 迁移前后同一端口行为逐字等价 |
| **P3** | **能力动态化**：`Capabilities` 消费点接进内核（回填 / 身份外显 / @引用 / 转发 / id 校验） | 中 | 测试替身：`history=false` 降级不报错 |
| **P4** | 模型面搬家 + 修 M-1…M-4 | 中 | 10 项传输测试 + parity |
| **P5** | 数据面拆分：`DbPort`（SQL 原地）+ `MemoryPort` + SQLite 语义实现；**先修 `external_id` 确定性** | 高 | Mem-1 / Mem-2 两条契约测试 |
| **P5b** | **每端口单例**：`identity_state` / `activity_rhythm` / `calls` 加端口维度；连接任务改为每端口一个、故障不连坐（§6.6） | 中 | 双端口：身份冷却与作息互不影响 |
| **P6** | 外部内容面搬家 + 统一跳转策略 + 下载移出事务/锁 | 中 | topic / media / OCR 套件 |
| **P7** | 入口收敛：删 `transport/`，8 个裸 `call("…")` → 类型化 `Command`；`main.rs` / `control.rs` 切到 `Io` | 中 | clippy `-D warnings` + 全量 |
| **P8** | 人设面：`PersonaPort` + 组装顺序硬边界（§7.3） | 中 | 断言：卡内容不出现在基座区 |
| **P9** | 一致性测试套件 + `HostIo` 权限面 + 注册表（**不加载任何真实插件**） | 中 | §11 |

**排序理由**：
- P3 必须**早于**任何第二协议实现 —— 先有抽象再生降级，等于用没被验证的抽象去接第二个协议。
- **P2b/P2c 必须先于第二个通讯适配器**：不先把"地址带端口"和"能力按端口查"做掉，第二个协议一接进来就会污染第一个。这也是"多协议并列"能成立的前提。
- P2c 是**全计划最高风险的迁移**（动约 15 张表的键空间）；它必须与 P5 的 `external_id` 确定性**协调**，否则键空间中途不一致。建议 P2c 单独排期、独立回滚。
- P5 的"先修 `external_id`"是前置项，不是细节。

---

## 11. 验收方式：不做例子，怎么证明抽象成立

1. **基线等价**：OneBot 与 SQLite 搬到端口后行为**逐字不变**（现有套件全绿）。
2. **契约测试（conformance）** —— 与实现无关，任何端口实现都得过。用测试替身压分歧路径：

| 替身 | 断言 |
|---|---|
| `FakeChatPort{ history=false }` | 内核关回填、学习门控改为只看实时，**降级不报错** |
| `FakeChatPort{ chat_id: "-1001234567890" }` | 负数 id 全程可用 → 证明协议约束已下沉进端口 |
| `FakeChatPort` 返回 `Unsupported{ profile }` | 身份外显跳过，不报错 |
| `FakeChatPort` 发送返回 `uncertain` | 只记账、不自动重发 |
| `FakeMemoryPort` 对 `Group` 查询返回 `person:` 级行 | 内核**回读过滤**守住 §15.4 |
| `FakeMemoryPort` 返回 `Degraded` | 内核降级不崩溃 |
| `FakeMemoryPort` 往返 | Mem-1 / Mem-2 成立 |
| `FakeModelPort` 无凭据 | 请求照发（M-1 已修） |
| `FakePersonaPort` 带 `system_prompt` 字段 | 该字段**不进入任何 prompt 区**（§7.3） |
| **两个 `FakeChatPort` 同时注册** | 回复走**消息来源**的端口；未注册端口 → `unknown_port` |
| **两个 `FakeChatPort` 能力不同**（一个有 `group_card`、一个 `history=false`） | 能力**互不串味**：各自按自己的 `capabilities_for(chat)` 降级 |
| **两个端口的同名 id**（`group:qq:123` 与 `group:tg:123`） | 在 `messages` 里互不覆盖 |
| **两个端口的群**（QQ 群 + TG 群） | 跨群共享 `overlap = 0`（§6.7 红线，**必须有目标测试**） |
| **一个端口断线** | 其它端口照常收发（故障不连坐） |

3. **能力包络清单** —— 把例子转成"架构必须能表达"的核对表，逐条证明契约能表达，但不实现（P-2）：

| 包络条目 | 由什么承载 | 状态 |
|---|---|---|
| 无凭据的本地模型 | `ModelPort` 凭据可缺省 | 待 P4 |
| 与云端隔离的模型预算 | 预算按端口隔离 | 待 P4/P5b |
| 能力缺失的协议（无历史/名片/签名） | `Capabilities` + `Unsupported` + 动态 `Degraded` | 待 P3 |
| 负数/非数字 chat id | id 校验下沉进端口 | 待 P2c/P7 |
| **同面多协议并列**（QQ + 微信 + TG + Discord） | `ChatRouter` + 端口化地址 + `capabilities_for(chat)` | 待 P2b/P2c |
| **跨协议不共享** | §6.7 红线（先比端口再算 overlap） | 待 P5b |
| **每账号独立身份/节律** | §6.6 单例拆分 | 待 P5b |
| 远程记忆后端 | `MemoryPort` + `HostIo` + 可序列化类型 | 待 P5/P9 |
| 键空间往返无损 | Mem-1 | 待 P5 |
| 人设来自外部卡 | `PersonaPort.load` | 待 P8 |
| 卡不得覆盖基座 | 组装顺序硬边界 | 待 P8 |
| 关键词触发的上下文 | **第二类**，本计划不承载 | 不做 |

---

## 12. P0–P2 可执行清单（建议批准范围）

### P0 —— 契约与纪律（零代码风险）

- [ ] `types.rs` 落地：`Incoming` / `Sender` / `Segment` / `MediaSrc` / `Command` / `CommandResult` / `Capabilities` / `PortState` / `PortManifest` / `PortReply` / `MemoryKey` / `MemorySubject` / `MemoryLayer` / `MemoryEntry` / `MemoryHit` / `Evidence` / `PersonaSource` / `IoError`
  - 硬要求：**只用 `serde` / `futures`，不放闭包与 trait object 进数据**（为跨进程序列化留路）
- [ ] `ports.rs` 落地 6 个 trait 签名（§5）
- [ ] **P-5 CI 断言**：`engine/io/**` 不得出现 `use crate::engine::{Engine, policy, cycle}` 等
- [ ] **两层测试方案**（先写方案再写代码）：
  - 可用性层：每个端口的输入/输出正确、边界、失败路径
  - 目标层：**每步的目标测试都要注明设计依据**（哪份文档哪一行），使设计变更时测试会变红

### P1 —— 纯新增骨架（零行为变更）

- [ ] `engine/io/{mod,types,ports,registry,router,host}.rs` + `adapters/null.rs`
- [ ] `Io` 只做聚合，**不被 `Engine` 引用**
- [ ] `router.rs` 落地 6 个端口组的组合策略（§6.2），先只支持"单实例"路径，多实例留桩
- [ ] `null.rs` 实现全部端口：`deliver` 返回 `Unsupported`、`retrieve` 返回空、`complete` 返回 `Failed` —— 作为 dry-run 与测试基线

### P2 —— 通讯面搬家（唯一行为敏感点）

- [ ] `transport/mod.rs` → `engine/io/adapters/onebot.rs`，实现 `ChatPort`
- [ ] `Incoming ↔ OneBot 事件`、`Command ↔ OneBot 动作` 双向映射
- [ ] 旧路径保留 **re-export shim**（`crate::transport::OneBot`）直到 P7，保证每步可上线
- [ ] **等价性验收**：`onebot_mock` 全套逐字通过；`engine_parity` 不受影响
- [ ] `Cargo.toml` / `lib.rs` 的模块声明不破坏 `--all-targets` 构建

### P2b —— 通讯面多实例（**新增，多协议并列的前提**）

- [ ] `ChatRouter`：按 `Command.chat` 的端口段路由；未注册端口 → `unknown_port`
- [ ] `capabilities_for(chat)` 取代全局 `capabilities()`（§6.5）
- [ ] `Incoming` 增加 `port` 字段（§6.4）
- [ ] 双假端口契约测试：来源路由、能力互不串味（§11）
- [ ] **单端口配置下行为逐字不变**（可先只注册一个端口）

### P2c —— 地址端口化（**新增，全计划最高风险**）

- [ ] `chat` 由 `kind:<id>` 改为 `kind:<port>:<id>`（§6.3）
- [ ] 一次性数据迁移（约 15 张表）；与 P5 的 `external_id` 确定性**协调排期**
- [ ] **验收：迁移前后同一端口行为逐字等价**；约 10 处取原始协议 id 的地方改切第二刀，其余约 35 处类型判断不动
- [ ] 建议**单独排期、独立回滚**

---

## 13. 未定项

1. **`Capabilities` 的形态**：注册时静态声明 + 响应内动态 `Degraded`，还是只有其一？我倾向**两者都要**。
2. **P9 注册表留到什么程度**：只留 trait + 空注册表，还是连发现/沙箱都留到真做插件时再定？按 P-3 我倾向**后者**。
3. **`DbPort` 是否永久非插件点**（不换 PostgreSQL 等）？本计划按"永久非插件点"推进。
4. **`first_mes`**（内核议题，§7.5）：群聊要不要为卡破例先发言？
5. **P2 的 shim 保留到哪一步**：我在 P7 删除；如果你想更早收敛可提前。
6. **地址格式**：`kind:<port>:<id>`（我推荐，代价最小）还是 `port:kind:<id>`（更直读但打断 35 处类型判断）？
7. **记忆面多后端是否允许"多写者"**：我按**单写者 + 只读镜像**设计（§6.2）；如果需要真正的多写，`forget` 的语义会分叉（一个删、一个墓碑），要重新定。
8. **是否合并跨平台身份**：我按"同一人在两个平台 = 两个独立 subject"设计（§6.8）；若将来要做身份解析，`MemorySubject::Person` 需要加端口维度。
9. **P2c 是否本轮就做**：它是多协议成立的前提，但也是最高风险项；也可以先做 P2b（路由就绪）+ 只跑单端口，把 P2c 推迟到真的接第二个协议之前。

---

## 14. 与既有文档的关系

- [`docs/PLUGIN-PORTS.md`](../PLUGIN-PORTS.md)：§3.1–3.4 的中立模型照用；**§4 迁移路径由本文取代**；其 §5 待定问题中 #2（Forward 展开归属）、#3（`Fetch*` 返回形状）、#4（id 校验归属）本文已给倾向：**Forward 归端口、id 校验归端口、`Fetch*` 用中立结构**。
- [`docs/working/audit-io.md`](audit-io.md)：本文顺带修 IO-07（转发展开无上限 + 阻塞主循环）、IO-08（下载跨事务/锁）、S1（跳转策略不一致）。
- `TODO.md` F1：以本文为落地清单。
