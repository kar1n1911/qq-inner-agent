# 插件与端口(设计分析)

> 范围:**只做输入/输出端口**,不做决策层插件。
> 目标:把内核的**输入/输出序列化成中立形式**,再由插件适配不同外部协议
> (OneBot / 微信机器人 / Discord / Telegram …)。
> 本文是**分析**,不含实现。落地条目见 `TODO.md` F1。

---

## 1. 为什么先做端口,而不是决策插件

决策插件(自定义候选、评分维度)要改的是**内核对"说什么"的判断**,与协议无关,风险在内核语义;
端口插件要解决的是**"怎么收、怎么发"**,而这部分**现在完全被 OneBot 的形状占满**(见 §2)。
先把 I/O 中立化,收益立刻可见:换协议不动引擎,同时压掉大量协议专有判断。

---

## 2. 现状:OneBot 已经渗到哪一层(实测清单)

| 位置 | 现状 | 问题 |
|---|---|---|
| **入站入口** | `Engine::ingest(event: &Value)` 直接吃 **OneBot 原始事件 JSON** | 事件形状是内核的输入契约 |
| **入站字段** | 内核解读:`post_type` `message_type` `notice_type` `message` `message_id` `self_id` `sender` `time` `user_id` `group_id` | 10 个 OneBot 字段散在 `engine/policy.rs::normalize_core` 等处 |
| **消息段** | 处理 `text` `at` `reply` `image` `face` `forward` `json` `node` | 段类型即 OneBot 段类型 |
| **合并转发** | `resolve_forwards` 调 `get_forward_msg` 展开 | 协议专有的惰性展开逻辑嵌在策略层 |
| **出站抽象** | `trait OrientationTransport { fn call(action: &str, params: Value) }` | **trait 本身是 OneBot 形状**;`action` 就是 OneBot 动作名 |
| **出站动作** | 21 个:`get_login_info` `get_status` `get_friend_list` `get_group_list` `get_group_info` `get_group_member_info` `get_group_member_list` `get_stranger_info` `get_group_msg_history` `_get_group_notice` `get_forward_msg` `send_group_msg` `send_private_msg` `set_group_card` `set_qq_profile` `set_qq_avatar` `set_self_longnick` … | 内核按协议动作名发指令 |
| **媒体发送** | `send_media(chat, segment: Value)`,内部按 `segment["type"]` 校验 | 最深的耦合:内核在拼 OneBot 段 |
| **已中立的部分** | chat 标识 `group:<id>` / `private:<id>`;`TransportState`;通知模型 | ✅ 可复用,是端口抽象的现成地基 |

**一句话**:现在换协议 = 让新协议**假装自己是 OneBot**。这正是要消除的。

---

## 3. 中立模型:先序列化输入/输出

### 3.1 入站(端口 → 内核)

```rust
/// 端口把外部事件翻译成这个信封;kernel 只认它。
pub struct Incoming {
    pub chat: String,                 // 已有约定:"group:<id>" / "private:<id>"
    pub message_id: String,
    pub self_id: String,
    pub timestamp: f64,
    pub sender: Sender,
    pub segments: Vec<Segment>,
    pub self_message: bool,
    /// 端口私有原文;内核不得解读,仅用于回放/审计。
    pub raw: Option<serde_json::Value>,
}

pub struct Sender {
    pub id: String,
    pub name: String,          // 显示名(nickname / card 的优先结果)
    pub card: Option<String>,  // 群名片
    pub role: Option<String>,  // owner / admin / member
    pub title: Option<String>, // 群头衔(见 TODO J0)
    pub is_robot: Option<bool>,
}

pub enum Segment {
    Text(String),
    Mention { id: String },                 // @某人(含自己)
    Reply { id: String },                   // 引用某条
    Image { src: MediaSrc, summary: Option<String> },
    Face { id: String },                    // 协议表情;端口负责解释
    Forward { id: Option<String>, nodes: Vec<ForwardNode> }, // 惰性:只有 id 时由端口展开
    Card { title: Option<String>, desc: Option<String>, url: Option<String> },
    Unknown { kind: String },               // 保留未知段,内核忽略但不报错
}

/// 媒体来源:内核不关心它是 HTTP、本地文件还是 base64。
pub enum MediaSrc { Url(String), LocalPath(String), Opaque(String) }
```

要点:
- **段是"语义"而不是"协议码"**:`Mention` 不区分 QQ 的 `at`/Discord 的 `<@id>`/TG 的 `text_mention`;
- **未知段必须可携带**:否则新协议的新段会直接报错;
- **`raw` 只给端口用**:内核不得依赖它(否则又耦合了)。

### 3.2 出站(内核 → 端口)

把 21 个字符串动作换成**类型化指令**:

```rust
pub enum Command {
    Send       { chat: String, segments: Vec<Segment>, reply_to: Option<String> },
    SendMedia  { chat: String, media: MediaRef },        // 不透明句柄,由端口解析
    FetchHistory { chat: String, count: usize, before: Option<String> },
    FetchForward { id: String },
    FetchMembers { chat: String },
    FetchGroupInfo { chat: String },
    FetchFriendList,
    FetchGroupList,
    LoginInfo,
    Health,
    SetGroupCard { chat: String, card: String },
    SetProfile   { nickname: Option<String>, signature: Option<String> },
    SetAvatar    { media: MediaRef },
}

pub enum CommandResult {
    Sent { message_id: String },
    Items(serde_json::Value),      // 结构化返回,但**形状由中立类型定义**,不是协议原文
    Ok,
    Unsupported { capability: Capability },
    Failed { code: String, uncertain: bool },   // uncertain 保留现有语义
}
```

要点:
- **`Unsupported` 是一等公民**:协议缺某个能力时内核必须能**降级**而不是报错
  (文档 §22 已经记录过这种差异:"SnowLuma 无、NapCat 有");
- **`uncertain` 保留**:现有"发送结果不确定不得重试"的约束不能丢;
- `Fetch*` 的返回要定义**中立结构**(群资料、成员、历史条目),不能让协议原文直接回流。

### 3.3 能力声明

```rust
pub struct Capabilities {
    pub mention: bool, pub reply: bool, pub media: bool, pub forward: bool,
    pub group_card: bool, pub profile: bool, pub avatar: bool, pub signature: bool,
    pub member_title: bool, pub member_role: bool, pub is_robot: bool,
    pub history: bool, pub group_info: bool, pub members: bool,
}
impl Port {
    fn capabilities(&self) -> Capabilities;   // 内核据此裁剪行为与提示词
}
```

**这一项是本次分析里最关键的产出**:它把"能不能 @、能不能改名片、有没有头衔"变成**运行时可查询的事实**,
而不是靠内核猜或靠文档记。

### 3.4 端口接口

```rust
pub trait Port: Send + Sync {
    fn name(&self) -> &'static str;                 // "onebot" / "discord" / ...
    fn capabilities(&self) -> Capabilities;
    fn state(&self) -> PortState;                   // 连通/在线
    /// 出站:收指令、给结果。唯一写入口。
    fn deliver<'a>(&'a self, cmd: Command) -> BoxFuture<'a, CommandResult>;
    /// 入站:端口把 Incoming 推进内核(当前实现是 mpsc + Notification)。
    fn attach(&self, sink: mpsc::Sender<Incoming>);
}
```

现有 `OneBot` 结构 + `transport/mod.rs` **整体变成"OneBot 端口"**:它对外只实现 `Port`,
内部的 `call("get_group_info", …)`、段拼接、校验全部**下沉到端口内部**。

---

## 4. 迁移路径(可增量、每步都能上线)

> **⚠️ 本节已被 [`docs/working/io-port-plan.md`](working/io-port-plan.md) 取代。**
> 那份计划把插件分成两类——**第一类 IO 插件**（本计划范围）与**第二类决策层插件**（单独立项）——
> 把端口扩到 6 个(通讯/模型/操作数据/记忆/外部内容/人设)、把 IO 收进 `engine/io/`,
> 并补上了本节缺的:能力动态降级、一致性测试套件、插件受限宿主 `HostIo`、以及人设面的硬边界。
> 本节保留仅作历史参考。

| 步 | 动作 | 风险 |
|---|---|---|
| **P1** | 定义中立类型(`Incoming`/`Segment`/`Command`/`Capabilities`),**不接线** | 零(纯新增) |
| **P2** | 写 **OneBot 端口**:`Incoming ↔ OneBot 事件`、`Command ↔ OneBot 动作` 的双向映射 + 测试 | 低(新代码) |
| **P3** | 入站切换:`ingest(Value)` → `ingest(Incoming)`;`normalize_core` 改为消费中立段 | 中(改引擎入口) |
| **P4** | 出站切换:`call(action, params)` → `deliver(Command)`,逐个替换 21 个调用点 | 中(面广但机械) |
| **P5** | 能力接入:`capabilities()` 驱动降级(无 `group_card` 就跳过身份外显;无 `mention` 就不发 @) | 低 |
| **P6**(以后) | 插件化加载:端口可进程内(Rust trait)或 **sidecar**(把 `Incoming`/`Command` 走 socket 序列化)—— **§3 的中立模型就是为这步准备的** | 高,单独排期 |

**建议**:P1+P2 先做(纯新增、零风险),它同时把"OneBot 细节"从内核里**抽出来但不改行为**;
确认映射逐字等价后,再做 P3/P4 的切换。

---

## 5. 待定问题(需要你拍板)

1. **中立段的粒度**:`Image` 是否要区分"静态图/动图/表情包"?—— 影响素材系统与 OCR 的接法;
2. **`Forward` 的展开由谁做**:端口展开(内核只看到已展开的节点)还是内核发 `FetchForward` 再自己拼?
   —— 现在的实现是后者(`resolve_forwards` 在策略层),我倾向**改成端口展开**(协议差异归端口);
3. **`Fetch*` 的返回形状**:要新定义一套中立结构(群资料/成员/历史),工作量不小;
   可先只覆盖内核真正消费的字段;
4. **id 的类型**:现在一律字符串;`group:<id>` 的数字校验(如 `send_media` 里 `parse::<u64>`)要不要**收进端口**?
   (我倾向收进端口 —— 那是协议约束,不是内核约束);
5. **进程内 vs sidecar 的先后**:先把 trait 做出来(进程内),sidecar 留到 P6;
   但**中立模型现在就要按"可跨进程序列化"设计**(只用手册化的类型,不放闭包/trait object 进数据)。
