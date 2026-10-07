# 开发文档:系统与实现位置

> 本文件是给**开发者**看的系统地图:按系统拆分,标注每个功能实现在哪个文件。
> 它是"在哪改"的索引,不是设计讨论。设计与阶段的临时工作文档见
> [`docs/working/prompt-and-learning-design.md`](prompt-and-learning-design.md) 与 [`docs/working/rust-port/`](rust-port/)。

## 系统总览

| 系统 | 职责 | 核心文件 |
| --- | --- | --- |
| [人格系统](#1-人格系统) | persona / 身份自治 / 过往情景 / 人类化 | `config.rs` `persona/expression.rs` `persona/mod.rs` `persona/backstory.rs` `persona/humanize.rs` `prompts.rs` |
| [记忆系统](#2-记忆系统) | 三层记忆 / 召回排序 / 学习 / 词元 | `memory/mod.rs` `memory/ranking.rs` `store/learning.rs` `persona/conversation.rs` `memory/text.rs` |
| [情绪系统](#3-情绪系统) | 心情/认同/好感 + 二维 disposition | `persona/affect.rs` |
| [记忆召回](#4-记忆召回) | recall 下钻 | `persona/recall.rs` |
| [决策系统](#5-决策系统) | 引擎 48 步 / 三层决策 / 策略 / 发送 / 活跃 / 观察 | `engine/mod.rs` `engine/decision.rs` `engine/policy.rs` `engine/sending.rs` `engine/activity.rs` `engine/orientation.rs` |
| [素材系统](#6-素材系统) | 表情包采集 / 选择 / 来源可公开性 | `media.rs` `media_select.rs` `media_source.rs` |
| [话题来源](#7-话题来源-21) | 外部内容抓取 + 群间转发 | `topic_source.rs` `relay.rs` |
| [传输系统](#8-传输系统) | OneBot v11 + LLM provider | `onebot.rs` `provider.rs` `provider_transport.rs` |
| [存储与配置](#9-存储与配置) | SQLite / 观察期持久化 / 配置协议 / 文件协议 | `store.rs` `store/*.rs` `config.rs` `settings.rs` |
| [控制与运行时](#10-控制与运行时) | 控制套接字 / 主循环 / 主人教学 | `control.rs` `main.rs` `persona/owner_teaching.rs` |

全部源码位于 `rust/src/`。Node 侧(仪表盘/CLI)见[最后一节](#11-node-侧)。

---

## 1. 人格系统

agent "是谁、怎么说话"这一整条链。

| 功能 | 实现位置 |
| --- | --- |
| 基座人格 `agent.persona`(种子文本) | `config.rs` — `Agent.persona`(`RuntimeText`) |
| 行为/回复风格/兴趣/变体 `agent.personality` | `config.rs` — `Agent.personality`;`persona/expression.rs` — `personality_context()` 组装成 payload |
| 提示词里的 persona 注入 | `prompts.rs`(生成物,勿手改,见[提示词](#提示词));`engine/mod.rs` — formation/articulation payload 的 `persona` 字段 |
| **身份自治**(§22:昵称/群名片/头像/签名自动外显 + 人格成长) | `persona/mod.rs` — `enough`/`propose`/`automate`/`grow`/`persona`/`backup`/`restore`;`onebot.rs` — `set_group_card`/`set_qq_profile`/`set_qq_avatar`/`set_signature` |
| **模型自拟昵称**(贴吧式) | `persona/mod.rs` — `NAME_PROMPT`/`member_names`/`model_nickname`;引擎 tick 里调模型生成 |
| **过往情景**(§23:不可变虚构自身过往) | `persona/backstory.rs` — `create`(只 INSERT)/`add_detail`(只追加)/`recall`;`engine/mod.rs` — articulation 前的 prepare/recall |
| 人类化运行时片段(只发表情 / 多气泡指令) | `persona/humanize.rs` — `FACE_ONLY_INSTRUCTIONS`/`MULTI_BUBBLE_INSTRUCTIONS` |
| 多气泡 + 打字延迟发送 | `engine/mod.rs` — articulation 后的发送循环(解析 `bubbles`、条间延迟随心情抖动) |

### 提示词

- **源**:`src/prompts.mjs`(仓库根,JS);**生成物**:`rust/src/prompts.rs`。
- 改动提示词:`src/prompts.mjs` → `node rust/tools/gen-prompts.mjs` → `cargo test --test prompts_parity`。
- 运行时片段(不进入生成物):`persona/humanize.rs` / `persona/recall.rs` 的常量,由 engine 追加到 payload。

---

## 2. 记忆系统

| 功能 | 实现位置 |
| --- | --- |
| 三层记忆(short/long/notebook + traits) | `memory/mod.rs` — `LayeredMemory` 等;表结构在 `store.rs` 的建表 |
| 召回排序(词汇重叠 + 中文双字 + 时效) | `memory/ranking.rs` — 稀疏召回,调用方先限 chat/subject |
| 学习事务(落库前 self-review) | `store/learning.rs`;引擎调用在 `engine/mod.rs` |
| 结构判断(沉默不转学习信号) | `persona/conversation.rs` |
| 词元化 / 相似度 | `memory/text.rs` — `terms()`/`similarity()`(对应旧 `store.mjs`) |
| 中文检索(unicode 归一) | `memory/memory_unicode.rs` |

---

## 3. 情绪系统

| 功能 | 实现位置 |
| --- | --- |
| 三指标(mood 消息级 / agreement 消息级 / affinity 人物级) | `persona/affect.rs` — `rate`/`read`/`update` |
| 衰减(affinity 7 天、其余 4h) | `persona/affect.rs` — `decay()` |
| 二维 disposition(四象限) | `persona/affect.rs` — `disposition(valence, rationality)` → Angry/Withdrawn/Scrutinizing/Supportive |
| 单向约束(agreement 不聚合 affinity)+ 有界步长 + 负性非对称 | `persona/affect.rs` — `update()` 内的断言与步长 |
| Angry 熔断(无回应连发 ≤3) | `persona/affect.rs` — `burst_allowed`/`reserve_burst` |
| 接入发送概率(motivation 因子) | `engine/sending.rs` — `sending_probability_with_affect`(定义);`engine/mod.rs` 调用,门控 `agent.affect.enabled` |

---

## 4. 记忆召回

| 功能 | 实现位置 |
| --- | --- |
| recall 下钻(两级召回,预算含 id/时间戳) | `persona/recall.rs` — `RULE`/`CONTRACT`/`Budget`;`engine/mod.rs` — articulation system 里追加 `RULE` |

---

## 5. 决策系统

| 功能 | 实现位置 |
| --- | --- |
| 引擎 48 步 cycle(候选→评估→发送→预期) | `engine/mod.rs` — `cycle()`/`media_cycle()` |
| 三层决策(①②拆分 + 零模型初筛) | `engine/decision.rs`;门控 `agent.threeLayerDecision` |
| 逐消息策略(allowed/normalize/quiet/select/repeated) | `engine/policy.rs` — `normalize`/`quiet`/`active_at`/`allowed`/`pick_length_target` |
| 发送概率(六因子乘积 + 预测校验) | `engine/sending.rs` — `sending_probability`/`forecast_result` |
| 活跃概率(高斯曲线) | `engine/activity.rs` — `activity_probability` |
| 入群观察闸门 | `engine/orientation.rs`(纯逻辑);持久化在 `store/orientation.rs` |
| forward 段收发(接收侧拉取合并聊天记录) | `engine/policy.rs` — `resolve_forwards`;`onebot.rs` — `get_forward_msg`/`send_forward` |

---

## 6. 素材系统

| 功能 | 实现位置 |
| --- | --- |
| 入站表情包采集(哈希去重、即时落盘) | `media.rs` |
| 素材选择(群温度 + 场合适配度,与自身 activity 独立) | `media_select.rs` |
| 来源可公开性(本地处理,不上传/不反向图搜) | `media_source.rs` |

---

## 7. 话题来源(§21)

| 功能 | 实现位置 |
| --- | --- |
| 外部新鲜内容(GitHub/RSS 抓取 + 相关度 + 节流/预算/缓存) | `topic_source.rs` — `fetch`/`parse`/`relevance`/`collect`;配置 `agent.topicSource` |
| 群间转发(低风险转手 / 高风险门控 + 去重 + 责任线/自审) | `relay.rs` — `classify`/`duplicate`/`gate`/`collect`;配置 `agent.relay` |

---

## 8. 传输系统

| 功能 | 实现位置 |
| --- | --- |
| OneBot v11 正向 WS(鉴权/心跳/重连/early 缓冲/echo 关联) | `onebot.rs` — `OneBot`/`send`/`send_media`/`send_forward`/`get_forward_msg`/`set_*` |
| LLM provider(两种 API 格式 + 重试退避 + 预算) | `provider.rs`(纯逻辑 + HTTP);`provider_transport.rs`(阻塞传输在 blocking 池) |

---

## 9. 存储与配置

| 功能 | 实现位置 |
| --- | --- |
| SQLite 主接口(建表/查询/保留期清理/预算) | `store.rs` — `Store` |
| 观察期与活动块持久化(epoch 防旧覆盖) | `store/orientation.rs` |
| 其余操作(学习事务等) | `store/operations.rs` `store/learning.rs` |
| 配置协议(defaults/merge/validate/loadConfig/readiness) | `config.rs` |
| 文件协议(revision 哈希 / atomicJson / 热重载) | `settings.rs` |

---

## 10. 控制与运行时

| 功能 | 实现位置 |
| --- | --- |
| 控制套接字服务端(方法表 + 事件) | `control.rs` |
| 主循环(1s tick / status / revision 监视 / 信号) | `main.rs` |
| 完整配置 schema(`config-defaults` 子命令,dashboard 动态白名单用) | `main.rs` — `Command::ConfigDefaults`(`config::Config::from_value(&defaults())` round-trip) |
| 主人私聊教学(/记住 /黑话 /忘记 /还原) | `persona/owner_teaching.rs` |
| 模块注册 | `lib.rs` |

---

## 11. Node 侧

Node 只承载仪表盘与 CLI(旧 agent 实现已归档到 `js-legacy` 分支):

| 文件 | 职责 |
| --- | --- |
| `src/dashboard.mjs` | Web 控制台(读控制套接字 + 双读回退) |
| `src/cli.mjs` | 命令行(`core-status` 等) |
| `src/control.mjs` | 控制套接字客户端 |
| `src/config.mjs` `src/settings.mjs` `src/provider.mjs` `src/onebot.mjs` `src/store.mjs` | 仪表盘/CLI 的运维依赖(配置读写、状态、直连 SQLite 的少量操作)。`settings.mjs` 的键白名单**动态取自 Rust 的 `config-defaults`**(回退到 `config.mjs` 的静态 defaults),加新配置键无需手改 JS |
| `src/prompts.mjs` | 提示词源(生成 `rust/src/prompts.rs`) |

前端静态资源:`web/`(`index.html` + `app.js` + `i18n.mjs` + 样式)。`index.html` 含三个页面(Overview / Configuration / Activity / **Advanced**);Advanced 页放所有功能开关与细粒度参数。

---

## 测试在哪

- 每个系统的 parity/单测在 `rust/tests/`:`config_parity.rs` `policy_parity.rs` `sending_parity.rs` `prompts_parity.rs` `engine_parity.rs` `memory_parity.rs` `store_parity.rs` `activity_parity.rs` `provider_parity.rs` `phase5_parity.rs` `identity.rs` `backstory.rs` `onebot_mock.rs` 等;
- parity 期望值冻结在 `rust/tests/golden/`(不依赖 node);
- 运行:`cargo test`;`cargo clippy --all-targets -- -D warnings`。
