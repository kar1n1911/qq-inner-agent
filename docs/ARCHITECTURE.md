# 架构(当前实现)

> 本文描述**代码现在的样子**,不是设计意图。凡"文档说已实现、实际没接线"的地方都会显式标注。
> 配套:`DEVELOPMENT.md`(按系统的功能 → 文件索引)、`TODO.md`(待办与已知失效)。

---

## 0. 一句话

整套 agent 是**输入 → 决策 → 输出**的三段式;**人格与记忆不是独立系统,而是决策层的输入上下文**。

```
        ┌───────────── 输入 ─────────────┐
QQ ──▶ OneBot 事件 ──▶ 归一化/去重 ──▶ SQLite(唯一状态)
                          │
                          └─ 同时:短期记忆 · 情绪状态 · 图片OCR · 观察期计数
                                     │
        ┌───────────── 决策 ─────────────▼────────────┐
        │  上下文组装 = 人格(persona/身份)             │
        │             + 记忆(三层/召回)               │
        │             + 情绪(affect) + 入群风格        │
        │             + 当前 history                   │
        │                    │                         │
        │  模型管线:FORMATION → EVALUATION → select    │
        │            → FORECAST/发送评估 → ARTICULATE  │
        │                    │                         │
        │  门禁:可用性(作息×节律)· 触发 · 冷却        │
        │  概率:开启 sending 时六因子+情绪修正、veto  │
        └────────────────────┬────────────────────────┘
                             │
        ┌────────────────────▼────── 输出 ─────────────┐
        │  Engine::send_chat · 多气泡 · 打字延迟        │
        │  落库 deliveries / messages(self=1)           │
        └──────────────────────────────────────────────┘
```

| 层 | 职责 | 主要文件 |
|---|---|---|
| **输入** | 事件接入、归一化、入库、感知(记忆/情绪/媒体/观察) | `main.rs`(事件入口)`engine/mod.rs::ingest` `store/` |
| **决策** | 组装上下文、模型管线、门禁、概率 | `engine/mod.rs::cycle` `engine/{policy,sending,activity,orientation}` `persona/` `memory/` `prompts.rs` |
| **输出** | 发送、多气泡、落库 | `engine/mod.rs`(发送段)`transport/mod.rs` |

---

## 1. 输入层

```
OneBot 事件
  → main.rs::Runtime::notice() → control.observe()
      ├─ P2 显式启用: adapter.map_event() → Engine::ingest_incoming()
      └─ 旧路径: policy::resolve_forwards() → Engine::ingest()
          ├─ policy::normalize()       校验身份/权限/时间格式；过期消息仍可入库
          ├─ 主人命令分支             去重后执行，不进入普通回复
          ├─ db.record_message()       去重入库(唯一真相)
          ├─ OCR/媒体采集、观察、记忆捕获
          ├─ 实时消息: db.observe()    观察 expectation，再置 pending/version/hint
          └─ 过期/回填: 只作感知，不创建活跃会话或 pending
  → Engine::tick()                     回复/话题初筛；旧模式另有 media/pause 路由
```

**感知与值班解耦**(2026-10-08):离岗不再丢消息,只影响是否发言。

四个后台循环:

| 循环 | 周期 | 位置 | 作用 |
|---|---|---|---|
| `tick` | 1s | `engine/mod.rs::tick` | 身份刷新与群资料采集 → 发言可用性 → 命令处理/逐聊天调度 |
| `backfill` | 连接时 + 60s | `engine/backfill.rs` | 拉群历史补录(默认开) |
| OCR worker | 入队即跑 | `media/ocr.rs` | 图片 → 文字 + 置信度 |
| 话题抓取 | 按 `topicSource` | `topic/mod.rs` | 外部内容(**当前无来源配置**) |

---

## 2. 决策层

### 2.1 上下文 = 人格 + 记忆 + 情绪(都在这一层,不分离)

| 来源 | 内容 | 代码 |
|---|---|---|
| 人格 | `agent.persona` + `personality` + 身份自治的成长人格 | `persona/mod.rs::persona` |
| 记忆 | 三层(short_term / long_term / traits)+ 召回排序 | `memory/mod.rs` `memory/ranking.rs` |
| 情绪 | mood / affinity / disposition → 影响动机与长度档位 | `persona/affect.rs` |
| 入群风格 | 观察期选出的初始 style / topics | `engine/orientation.rs` |
| 对话 | history(带 `speaker` / `sender` / `self`) | `engine/mod.rs::cycle` |

### 2.2 模型管线

```
FORMATION   → candidates(≤3)+ allocation + learning
EVALUATION  → ratings(8 维:relevance/information_gap/expected_impact/urgency/coherence/originality/balance/dynamics)
select()    → 选中候选(含沉默补偿 factor)
FORECAST    → shouldSend / outcomes / responseMode / plan (仅 sending.enabled=true)
概率/评估    → 六因子、veto、抽签并记录 assessment；关闭 sending 时整段跳过
ARTICULATE  → 最终 text / emoji / faceId / bubbles(+ lengthTarget 档位)
```

### 2.3 门禁

| 门禁 | 位置 | 说明 |
|---|---|---|
| 可用性 `available()` | `tick` 调度 + `cycle` 入口 | 作息表 × 活动节律；被点名（含私聊）的 pending 回复可唤醒，发送前仍复核安全闸门 |
| 触发 | `message` / `topic` / `pause` / `media` | |
| 冷却 | `proactiveCooldown` / `minThinkInterval` / `maxProactivePerHour` | `Hint::SelfChat` 可豁免部分 |

---

## 3. 输出层

```
Engine::send_chat(chat, text, face)
  ├─ 多气泡:配置/场景允许且 bubbles 为 2–3 条、通过长度/内容校验 → 逐条发送
  │    首条也延迟；条间另加 300–800ms；face 只挂最后一条
  └─ 逐条记录 delivery；收到 ACK 后各自记 messages(self=1)
```

---

## 4. API 面(四类)

内核对外只有四个接口面。**每个方法 / action / 端点"现在做什么"的逐条说明,
放在 [`DEVELOPMENT.md`](DEVELOPMENT.md) 各系统自己的章节里**:

| 接口 | 两端 | 传输 | 逐条说明在哪 |
|---|---|---|---|
| **控制套接字** | 内核 ↔ 仪表盘/CLI | Unix socket `data/control.sock`,NDJSON | [`DEVELOPMENT.md` §10.1](DEVELOPMENT.md#101-控制套接字接口内核--仪表盘cli) |
| **OneBot** | 内核 ↔ NapCat(QQ 桥) | WebSocket `ws://127.0.0.1:3001/` | [`DEVELOPMENT.md` §8.1](DEVELOPMENT.md#81-onebot-接口内核--napcat) |
| **模型 API** | 内核 ↔ LLM | `/chat/completions`(OpenAI 兼容,含 DeepSeek)· `/messages`(Anthropic)· `/models` | [`DEVELOPMENT.md` §8.2](DEVELOPMENT.md#82-模型-api内核--llm) |
| **仪表盘 HTTP** | 浏览器 ↔ Node | HTTP `127.0.0.1:5097` · HTTPS `0.0.0.0:5098` | [`DEVELOPMENT.md` §11.1](DEVELOPMENT.md#111-仪表盘-http-接口浏览器--node) |

在数据流上的位置:

- **OneBot** 是**输入层与输出层的边界**(事件进来、消息发出都走它);
- **模型 API** 在**决策层**被六个任务共用:`ORIENTATION`(入群定风格)、`FORMATION`(出候选)、
  `EVALUATION`(评分)、`ARTICULATION`(成文)、`FORECAST`(发不发)、`LEARNING_REVIEW`(复核学习项);
- **控制套接字**只服务运维,不参与聊天链路;
- **仪表盘 HTTP** 是给浏览器的一层封装(它再经控制套接字或直读 SQLite 取数)。

---

## 5. 配置地图(`config.json`)

```
ui.language
provider.{kind,baseUrl,model,maxTokens,tokenParameter,timeoutSeconds,retries,requestsPerHour,anthropicAuth,workspaceId,thinking}
onebot.{url,selfId,heartbeatSeconds,requestTimeoutSeconds,reconnectMaxSeconds,forwardEnabled}
agent
  ├─ name / persona / aliases / replyLanguage
  ├─ personality.{behavior,replyStyle,interests,variants,variantProbability}
  ├─ expression / emoji / rhythm / schedule / observation / memory / learning
  ├─ sending / proactive / dryRun / threshold / interruptThreshold
  ├─ affect / identity / backstory / ownerTeaching / relay / topicSource
  ├─ memoryRecall / threeLayerDecision / multiBubble / backfill / ocr
  └─ allowedGroups / allowedUsers / ignoredUsers
storage.{directory,retentionDays,maxMessagesPerChat}
```

键白名单由 Rust `config-defaults` 导出,仪表盘动态同步。

---

## 6. 模块接线状态(审计结论)

| 模块 | 配置 | 实际 | 说明 |
|---|---|---|---|
| `store` / `config` / `control` | — | ✅ | |
| `transport`(OneBot + provider) | — | ✅ | |
| `engine`(tick/cycle/ingest/policy/sending) | — | ✅ | |
| `engine::orientation` | `observation.enabled` | ✅ | 2 群已 ready |
| `engine::backfill` | `backfill.enabled` | ✅ | 默认开 |
| `engine::backlog` | `observation.backlogDigest` | ⚪ 关 | 默认关(符合设计) |
| `persona`(身份自治 §22) | 4 个 allow 全开 | ⚠️ **卡住** | `enough()` 需 traits ≥ 3,现有 1 → 从未尝试 |
| `persona::affect` | `affect.enabled` | ✅ | 但 disposition 落到 `Withdrawn`(动机 ×0.2) |
| `persona::expression` | `expression.learn` | ⚠️ **0 条** | 模型从不产出 |
| `persona::humanize` | `emoji.*` | ✅ | `humanize_faces` 715 行 |
| `persona::owner_teaching` | `ownerTeaching.enabled` | ✅ 待用 | 尚无使用 |
| `persona::backstory` | `backstory.enabled` | ⚪ 关 | 默认关 |
| `memory` | `learning` / `memoryRecall` | ⚠️ 部分 | `short_term` 660+;`traits`/`long_term` 近乎空 |
| `media`(素材语料) | 未配 | ⚪ | 表全为 0 |
| `media::ocr` | `ocr.enabled` | ✅ | rapidocr,14 条 |
| `topic`(外部来源) | `topicSource.enabled` | ⚠️ **不抓取** | `github`/`feeds` 为空 |
| **`topic::relay`(群间转发)** | `relay.enabled` | ✅ 有条件接线 | 主动话题且群活动合格时收集、审核后注入候选上下文（`engine/mod.rs::cycle`） |
| `onebot.forwardEnabled` | true | ✅ | 合并转发解析 + 标注 |

图例:✅ 正常 · ⚠️ 部分/受阻 · ⚪ 有意关闭 · ❌ 失效。

---

## 7. 已知失效(详见 `TODO.md`)

以下保留原条目编号；1、4、5 已按当前代码更正，不再列为失效。

1. **`relay` 已接线** —— 主动话题路径满足活动资格且 `relay.enabled` 时收集与审核候选链接（`engine/mod.rs::cycle`）。
2. **`topicSource` 无来源** —— 管线已接好,只差配置。
3. **`traits` 学不到是枢纽** —— 同时卡住 identity、expression 和"群内说话方式"。
4. **`multiBubble` 已接线** —— 模型契约包含 `bubbles`；实际拆分仍受配置、场景及长度校验约束。
5. **点名可唤醒回复** —— `tick` 与 `cycle` 对 `Hint::SelfChat` 保留可用性例外。
6. **自我身份缺失** —— 自己发的话在上下文里署名人格名,群里看到的是 QQ 昵称,导致认不出"被讨论的机器人就是自己"。
