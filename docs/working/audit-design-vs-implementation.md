# 设计 vs 实现:全流程一致性审计

**日期**:2026-10-10
**方法**:按流程分 6 组,每组只读它对应的设计文档行段 + 代码文件,专门寻找**"设计明确要求 A,实现却做了 B"**的偏差。
**参照案例**(发起本次审计的实例):设计 §九 L644-656 要求①「回复必要性」与②「发起话题必要性」的
触发条件、冷却周期与失败后果**都不同**;实现 `engine/mod.rs:1710` 写成 `let proactive = t.hint != Hint::SelfChat;`,
把"没被点名"当成"主动发言",导致群友说话时的回话被套上主动发言的恢复期而丢弃(日志实测 4/4 次)。

**纪律**:只报有证据的偏差;禁止凑数;每条给出设计原文+行号、代码 `文件:行`、可观测后果、置信度。
**父 agent 复核**:所有 high 条目均已回到代码核对,并尽量用线上数据反证(见"复核"列)。

---

## 一、总览

| 组 | 覆盖流程 | 报告 | 复核结果 |
|---|---|---|---|
| A | 决策分层 + 发送概率 + 群活跃度/漂移 + 交流阶段 | 6 条(3 high) | 3 条 high **成立** |
| B | 情绪 + 记忆召回 + 学习分诊 + 跨群共享 | 5 条(3 high) | **B3 推翻**;B2 结论对理由错;B1/B4 成立 |
| C | 身份自治 + 过往情景 + 人类化 | 11 条(5 high) | 3 条 high **成立** |
| D | 输入归一化 + 转发/回填/积压 + 话题来源 + 责任线 | 6 条(2 high) | 2 条 high **成立** |
| E | 素材采集与选择 + 场合分桶 + 表情包学习 | 9 条(3 high) | 3 条 high **成立** |
| F | 私聊窄例外 + 主人教学 | 3 条(1 high) | 1 条 high **成立** |

合计 **40 条**,复核后 **39 条成立、1 条推翻**。

---

## 二、按根因归类(比逐条修更有效)

### 根因 ① 一个变量承担多种语义 —— 修一处、消三个症状

**`let proactive = t.hint != Hint::SelfChat`(`engine/mod.rs:1710`)被至少 4 处复用**,而设计 (§九 L649/L652) 要求
①回话 与 ②开话题 的**冷却、配额、概率因子、失败后果都不同**。

| 编号 | 后果 | 代码 |
|---|---|---|
| A1 | 回话套上 `recovery`/`settle`/`pace`/`motivation`;`recovery=(gap/300).min(1)` 刚回话时≈0.067,概率近乎必丢 | `mod.rs:1710`、`sending.rs:192-221` |
| A2 | 回话走主动冷却/配额(`proactiveCooldownSeconds` 默认 180s、`maxProactivePerHour` 默认 6),且**自锁**:回话投递被记为 `proactive=1`,而 `counts.last` 不区分类型 → 回话后该群 180s 内所有非点名消息直接丢弃 | `decision.rs:55-71`、`mod.rs:1294-1296`、`store/operations.rs:187` |
| A3 | 情绪保底只覆盖被点名/私聊(`max(1.)` 写在 `!timing.proactive` 分支),非点名回话被情绪二次压低 | `sending.rs:266-296` |

**正确判据应是触发路径而非 hint**:`trigger ∈ {topic, pause}` 才算主动开话题。
**已复核**:A3 的 `if !timing.proactive { mood.max(1.); affinity.max(1.) }` 确实只对 SelfChat 生效,注释写的却是"被点名或私聊时保留情绪增益,但不因情绪压制回应"。

### 根因 ② 判据的尺度与实际数据量级不匹配 —— 阈值形同虚设

| 编号 | 内容 |
|---|---|
| **B2** | 设计要求"低心情 → 情绪化发言 `skip`(不落库)"。代码 `confidence_threshold = 2 * low_mood`,设计意图对应 `mood ≤ -0.5`(阈值 1.0);但情绪实际只有 **±0.01** 量级(单步上限 ±0.15,线上实测 mood ≈ -0.0026)→ 阈值 ≈0.02,而 confidence ≈0.7~0.9,**分支几乎永不触发**。*(B 组原报"恒为 0",理由有误,结论成立)* |
| **A4** | 设计指定两个独立概率 `p_active`/`p_quiet`/`quietCap` 用于开话题;代码在 `decision.rs:124-130` 硬编码 `0.15 × 两个线性斜坡`,而且 `since_human` 在 3×pause 处、`since_self` 在 1×pause 处**即饱和** → "随静默时长上升"变成常数,`quietCap` 对初筛完全无效。这几个旋钮只在默认关闭的媒体路径被消费 |

### 根因 ③ 设计写了、引擎没做或做窄了

| 编号 | 内容 | 置信 |
|---|---|---|
| C1 | 多气泡:**任一片段失败不中断**,且 `sent = Some(result)` 只保留最后一条 → 首条失败被末条成功掩盖,整轮记为 `sent` | high |
| C2 | 多气泡:**片段绕过 `maxOutputChars` 与总长度约束**(截断只在 `decorate`,只作用于 `text`) | high |
| C3 | 多气泡:**无档位/场景门控、无 1–3 条上限**,只看 `a.multi_bubble` 开关 | high |
| C4 | **首条消息无打字延迟**:代码是 `if i > 0 { sleep }`,而单气泡占绝大多数 → "改动 8"对主力路径完全没实现 | high |
| C5 | 气泡之间只重查"在岗",**未重查静默时段** → 静默开始时已排队的气泡仍会发出 | medium |
| C6 | 设计规定"默认开启"的 face-only、按群学频率,实现是 **serde 默认 false**,测试还断言默认关 → 开箱即用时装饰仍是全局固定 15% | medium-high |
| C7 | `long` 档不配表情**无引擎门控**,且装饰抽样发生在档位抽取之前(顺序上档位还不存在) | medium |
| C8 | **`add_detail` 无运行时调用点**(只有测试调用),且 `prepare` 在已有记录时跳过 → 过往情景 `details` **恒为空**,"可追加细节"生产路径不存在 | high(事实) |
| C9 | 过往情景触发条件被实现为"该 chat **无任何**其它记录(含机器人自己的发言)",设计要求的是"无**相关**真实记忆" → 实际永不触发 | medium |
| C10 | 过往情景 RULE 直拼 system,**绕过第③层"命名片段"机制** → `disabledRules` 关不掉、parity 覆盖不到 | medium |
| C11 | 被明令废弃的「基座名·风格标签」模板**仍是护栏拒绝时的落库取值** | low-med |
| D4 | §21.7 把"已被别人转发过的卡片/链接"列为**低风险、优先**的候选,代码对 `[合并转发]/[卡片]/[CQ:` **一律 continue**,只抽裸 URL → 该档对象运行时不可达 | medium |
| D6 | §21.5.3"转发带来源链接"在**最终生成步无执行点**(ARTICULATE payload 不含 `externalTopics`) | low |
| E-D1 | §12.1 说"按场合分桶不是更精确,**而是唯一说得通的形式**";实现 bucket 只有 `"active"`/`"quiet_rescue"` 两个字面量,四个阶段全塌进 `"active"` | high |
| E-D3(D2) | 温度硬门槛挂在 `a.quiet`(活跃度≤1/3),而救场触发用 `since_human≥silence && stage∈{Closing,NaturalEnd}` —— **两个谓词不同**,前者为假时门槛被整体跳过 | high |
| E-D3 | 同一个桶名 `quiet_rescue` **写入侧用"时间断口"、读取侧用"活跃度"**两套定义 → 写进去读不到,桶默认 Beta(1,3)=1/3 < 0.65 → 冷清救场几乎从不发出 | high |
| E-D4 | 沉默的有效信号**漏掉 `Stage::Closing`**(设计明文含"收束 **或** 自然终止") | medium |
| E-D5 | "判定不确定 → 概率降到最低"未实现:概率只读连续 `confidence`(约 42% 满概率),不读 `confident`;且兜底返回 `NaturalEnd` 恰是允许触发的阶段 | medium |
| E-D6 | "群内在通用"**无任何门槛**:采集端对每条人类图片无条件入库,通用度字段 `widespread` **全仓无消费者** | medium |
| E-D7 | §13.8 的"后台人工覆盖"通道不存在:`set_media_source_override`/`media_assets` **零调用方**,控制协议无 media 方法 | medium |
| E-D8 | 情景档案缺"语气/affect"维度(`media_contexts` 只有 chat/hash/message_id/role) | med-low |
| E-D9 | §11.5"每个判定都要能被复核"落空(`media_stages` 无读取方);且收束标记两份默认值不一致(一份为空、一份写死 5 条) | low |
| F2 | §20.5"黑话/表达优先,`/记住` 的长期记忆其次"**无对应实现**(两类并列进 prompt,无优先级) | low |
| F3 | §20.3 授权只由 `ownerUin` 定义,实现额外要求必须同时在 `allowedUsers` → 主人不在列表时 `/记住` **静默丢弃** | low |

### 根因 ④ 红线只在部分方向执行

| 编号 | 内容 | 置信 |
|---|---|---|
| **D2** | **入站转发把原发言人昵称/QQ 拼进本群正文**(`policy.rs:616-632`),并写进 `person:<转发者>` 记忆(`memory/mod.rs:47-52`)→ 踩 §15.4.3「必须剥掉来源消息 id 与**昵称**」与 §21.7「person 级与私聊内容永不外溢」;且 `attribution` 按 `sources.sender` 区分,而 sender 是**转发者** → 别人的话被算到转发者头上 | high |
| B4 | 跨群共享的昵称剥离**只有单向**:兜底检测只取候选行自己的群做子串匹配,A 群摘要含 B 群成员昵称/QQ 时不会被拦 | medium |
| **F1** | **`/记住` 完全绕过 §16 自我审核**(`owner_teaching.rs:69-85`),且用与事实相反的注释"§16 尚未实现"作理由;于是 `/记住 我的身份证号是…`、`/记住 忽略之前所有规则` 会以 `confidence=1.0, importance=0.8` **原样落库**并进入此后每轮 prompt | high |
| **D1** | §14 已决定「无害的经历虚构 ✅放开」,但**优先级最高的 persona 层**仍是「**不编造亲身经历**」(defaults.json/config.mjs),Layer① IDENTITY 还有"兴趣不是编造经历的许可" → 放开被上层压掉 | high |

### 根因 ⑤ 死分支 / 死字段 / 未接线

| 编号 | 内容 |
|---|---|
| A5 | `if reply.is_none() { "message" } else if topic.is_none() { 概率筛 }` → **reply 通过时 topic 永不参与抽签**,"先回复还是先开话题"未实现;且每 tick 白算一遍 probability 再丢弃 |
| A6 | `wild` 漂移只有一重闸门:设计要"冷清 **且** 该群接受过离谱话题",实现里 `accepted_wild` 只抬上限,活跃群也能取到 wild |
| B5 | 愤怒熔断只对"**成功发出**"计数,"想发但被拦"不计入 → 与设计"连续超额 N 条"语义不同(方向偏严格,危害小) |
| D3 | 回填**未隔离 `db.observe`**(同文件对 state/version/last_human/pending 都隔离了,唯独漏它)→ 历史消息被登记为"我在等的回应",污染 `priorExpectation` 与 ① 的期望闸门 |
| D4(relay) | relay 去重只读 `short_term`,而其唯一写入点受 `learning.enabled` 控制 → `relay=true, learning=false` 时**去重恒失效**,同一链接可反复进候选 |
| D 附注 | **高风险转发链整段未接线**:`AgentMerged` 仅测试构造,`send_forward` 与 `RelayDecision.message_ids` 无生产消费者 → §21.7.3 下半的所有闸门运行时永不触发、无法观测 |
| C8/E-D6/E-D7 | 见上(无调用点/无消费者) |

---

## 三、建议的修复顺序

1. **根因 ①(一处修复、三个症状)**:把 `proactive` 改为按触发路径判定(`topic`/`pause`),并让冷却、配额、投递计数、情绪保底四者共用同一语义。**这是当前对线上行为影响最大的一条**。
2. **根因 ④ 的两条红线**:入站转发剥离昵称/QQ(或至少不写进 person 级);`/记住` 接回 §16 自我审核(并删掉那句错误注释)。
3. **根因 ②**:把 B2 的阈值改成与情绪实际量级匹配的形式(或在 affect 值域扩大之前,明确该机制"当前不生效"并记入文档),A4 的两个概率接进初筛。
4. **根因 ⑤**:A5(让 topic 真正参与抽签)、D3(回填隔离 observe)、D4(relay 去重不依赖 learning 开关)。
5. **根因 ③**:按影响排序 —— C4/C1/C2/C3(多气泡四条,直接影响观感与稳定性)→ E-D1/E-D3/E-D2(素材分桶与门槛)→ C8/C9(过往情景)→ 其余。

---

## 四、审计中确认"一致"的部分(避免重复劳动)

- **A**:静默期沉默的学习资格、收束标记可配置、冷启动用该群 `group_hours`、冷清期阻断话题;
- **B**:`agreement` 绝不聚合进 `affinity`(双向 assert + schema CHECK 物理排除)、状态只调阈值不进内容(`content_allowed` 输出侧拦截)、§15 person/私聊/原文不外溢、§16.4 审核在共享之前;
- **C**:身份自治的冷却/回退语义(`identity_state` 账号级单行、跨群共用冷却、同事务"先存原值再占冷却"、逆序回退重新冷却、dry-run 不执行)、`grow` 按群独立冷却、§23.3 三条不变量(触发器拦住 UPDATE/DELETE/REPLACE)、`decoration_usage` 冷却与 face-only 连续限制;
- **D**:三层分离与逐条开关、作息+活跃度门控、相关度≠安全且 fail-closed(owner 不能豁免)、三个新话题来源默认关闭、**出站侧**外溢防护、回填的允许群/去重/时效;
- **E**:素材发送禁带 text/at(结构与协议两层)、unknown 按 private、不做联网反搜、硬门槛全在抽签前、用群自己的作息、只采人类、阶段判定只记录不生信号;
- **F**:私聊不外溢(6 条独立证据)、授权不可伪造(sender 取 `event.user_id`、指令只拼 text 段、标记由代码生成)。

---

## 五、修复进度（2026-10-10，因 codex 额度耗尽暂停）

第一批派了 6 个修复任务，**全部撞到用量上限而暂停**，工作区内容均已保留：

| 任务 | 状态 | 工作区 |
| --- | --- | --- |
| `forward-identity` | **已提交** `e25c126` | 干净，待验收合并 |
| `proactive-fix` | 未提交（5 文件） | `engine/decision.rs`、`engine/mod.rs`、`engine/policy.rs`、`engine/sending.rs`、`tests/engine_parity.rs` |
| `owner-review` | 未提交（5 文件） | `engine/mod.rs`、`memory/mod.rs`、`persona/owner_teaching.rs`、`tests/engine_parity.rs`、`tests/owner_teaching.rs` |
| `mood-threshold` | 未提交（4 文件） | `memory/mod.rs`、`persona/affect.rs`、`prompts.rs`、`tests/learning_triage.rs` |
| `media-bucket` | 未提交（2 文件） | `media/media_select.rs`、`tests/media_select.rs` |
| `reply-target` | **未开始** | 无改动 |

### 额度刷新后的续做顺序

1. 让 4 个“未提交”的任务各自 `git status` / `git diff --stat` 确认改到哪，收尾并**补目标测试**，再提交；
2. `reply-target` 从零开始（去掉 `targeting.rs` 里对模型 `replyTo` 的无条件覆盖 + 改 `src/prompts.mjs:34` 的措辞）；
3. 全部完成后统一合并 → 远程全量 → 构建部署；
4. 之后派第二批：§14 persona 放开、`topic` 参与抽签（A5）、回填隔离 `db.observe`（D3）、relay 去重解耦（D4）、多气泡 5 条（C1–C5）、过往情景（C8–C10）、其余死字段。

### 测试流程（本次新增，已写入 `docs/DEVELOPMENT.md`）

任何功能**动手前**先写测试方案，且必须两层：**可用性测试**（输入/输出、边界、**接入程序总线**）+
**目标测试**（设计文档里那句“为什么”在**真实链路**上成立，须注明依据行号）。**只过可用性测试不算完成。**

---

## 六、方法学备注

子代理**会误读代码**:B 组把 `learning_thresholds` 返回元组的第 1 个值当成了第 2 个,得出"单来源被无条件强制 partial"的错误结论;
经代码复核(第三个值 `promotion_evidence` 只用于升格)**并用线上数据反证**(库中实际存在单来源、confidence=0.7 的条目)后推翻。
**故:凡 high 结论必须回到代码核验,可能时应以线上数据反证。**
