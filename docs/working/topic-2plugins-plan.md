# 主动话题两插件：边界与接口规划（2026-10-17；只读规划）

## 核实的基线

`rust/src/engine/io/types.rs:297-305` 的 `PortKind` 只有 Chat/Model/Db/Memory/External/Persona；`plugin_list.rs:64-98` 和 `src/dashboard.mjs:49-56` 只识别这六类，`factory.rs:183+` 是六面静态构造器。`plugins/plugins.json` 当前不存在。`rust/src/engine/behavior.rs:265-300` 有 `B.formation → K.validate_candidates → B.learning → K.commit_learning → B.evaluation → K.validate_ratings → B.selection` 的图，但 `behavior.rs:466-480` 的执行器仍把整轮委托给 `Engine::cycle`；编图器仅容许添加节点，不能替换已有 B 槽。因此行为槽是设计上正确的分类，尚非可运行能力。`TODO.md:555-615` 把“至少惠及三人”限定为研究档，当前档不要求；未来实验环境可有成员简历，当前仓库未见明确的简历采集/同意/可见性接口。`docs/working/topic-gen-select-plan-v2.md` 指出无稳定话题身份、来源及候选级生命周期的缺口。文献索引 #4/#23/#27/#28/#31 支持待检验的互动机制；#30/#32/#34 不支持把读过简历当成真人背书或擅自公开背景；观察研究不提供效果量或 QQ 阈值。

## 插件类型及分工

采用两类插件体系中的 **Behavior**，不是第七个 I/O `PolicyPort`，不塞进 `PersonaPort`。共同安装目录与发现/UI 可复用，行为契约、运行时与图编译独立于 `engine/io/` 的六端口。若只把现有算法包在 Rust 结构体内，改动小但并非真正插件；要从清单激活两项，需中等规模的行为 manifest 识别、静态行为构造器、B 槽绑定/逐槽执行、快照与结果校验、图 generation 发布。新建第七 IO 端口会扩散到六面注册/路由/工厂且仍解决不了行为图执行，改动更大且概念错误。完全语言无关的 Sidecar 是更大的后续阶段。

② **前置背景判断**在候选生成并经 `K.validate_candidates` 后、选择前运行；“前置”是相对选择，不是每次 FORM 前读取所有成员简历。宿主给其只读、同群、同一 turn 的 `TurnSnapshot`、候选 ID/来源/话题状态、可见当前上下文、成员背景的被声明允许字段及其来源/用途/公开范围/同意标记。它按候选产生 `BackgroundAssessment`：新人是否可能受益、可参与成员集合的下/上界、证据强度/出处/时间/可见性、侵入性、信息缺口、允许的表达边界。不给原始 Store、跨群/私聊资料、发送句柄或完整 Io；私密背景原则上留在该插件，输出给①的是经最小化的判断与证据句柄。②不能判最终发言，也不能把简历文字送进 prompt。

① **主动话题选择器**在评估及②完成后、表达前运行。输入为经过宿主验证的候选与评分、候选来源和稳定话题 ID/最后活动/阶段、②的逐候选判断、群当前作息与参与/配额可用状态。输出 tagged union `Select{candidate_id, action, opening_form, confidence, evidence_refs, direction}` 或 `Wait{reason, until_event_or_time}` 或 `Withhold{reason}`。`Select` 只选一个已经存在且合格的 ID，不写文本、不发送、不改账本。旧话题重提、引用目标、最终发送仍由宿主 `B.topic_lifecycle/K.final_admission` 复核。③“倾向”变成候选排序/回避规则：当前共享对象优先、旧话题/私人背景惩罚、安静可选；“方向”变成允许的开口形式和互动目标：概括当下、群体短评、有限的开放机会、仅在有直接证据时定向提问；不能绕过最后闸门。`Wait/Withhold` 是成功决策，不是失败。

## 置信标准：证据优先级、分档、开口形式

“确定”是对**可提供参与机会**的审慎估计，不是“新人最终会喜欢/被接纳”的已校准概率。用可审计的证据分档和硬门槛，暂不造任意加权和：

1. **硬门槛**：候选有同群、当前可见或经审核的外部新来源；话题身份和结束状态可核验；没有禁止披露的个人信息；侵入性不高；②可用；宿主参与/作息/配额闸门仍准许。任一失败，不主动说。
2. **证据排序（不能重复计同一人/同一来源）**：新人在本群明确提出问题或兴趣、主动接续当前话题为最强；当前群内共同可见的消息/图片/链接且新人已加入该互动为次强；老成员在当前群主动邀请新人参与给开口机会，但**不能单独证明新人会受益**；经同意用于选题的简历可支持“潜在参与人数”估计，但不能单独升级为对新人的高置信定向发言；模型推断、跨群、私聊、过期/不明来源只列未知，不作许可证据。当前共享对象优先于遥远的旧共同经历（文献 #23/#26，仅机制假设）。
3. **高**：新人当前明确表达/请求或自发接续，候选对应同一可见对象，且无披露/侵入风险；允许简短、与该对象直接相关的回答或一次有依据的定向邀请。**中**：群中有当下共享对象，新人可见，但其兴趣不明；仅允许不点名、不暴露背景的群体短评或一条有据概括，避免要求新人回应与连续追问。**低/未知**：只有简历交集、旧话题、推断兴趣、不可见对象或证据不足；不主动发言。②服务本身不可用时不能退到“中”的安全话术，必须停主动链。
4. `confidence={tier: high|medium|low|unknown, numeric: null, calibrated:false, basis_refs:[...]}`；若后续要展示 [0,1] 分数，应先用人工标注/线上反馈校准，并独立验证“开口/持续回应/新人接纳”，不能拿模型自报 0.8 当真概率。初始配置使用 `min_confidence_tier=medium`，但“中”只能用低侵入的非定向形式；定向需 `high`。高置信也不自动获得引用私人背景的权限。

**高确定性与低侵入的同时实现**：将“能拿来选题”和“能说给群听”分开。②可处理被允许的简历字段，仅向①传成员匿名句柄、证据等级与交集计数，不传原文；①选择当下公开共享对象。表达器只能看到公开素材和 `opening_form`，对卡片/简历事实做负面泄漏检查。要公开个人背景必须有该成员针对内容与受众的确认；任何点名/定向问题还需其当前可见参与证据。高侵入候选即使匹配多人也硬拒绝，不用“更多证据”抵消。记录证据 ID、可见性和配置代，但不记敏感原文。避免根据更多隐私资料把分数越算越高、同时使发言越私人。

## “至少惠及三人”与新来者：两种口径

`potential_beneficiaries` 是有证据的可参与人数估计，不是实际受益人数。②给 `lower_bound/upper_bound`、去重的人数和各人证据等级；研究档只用保守下界，资料缺失不能按“可能”凑数。两人群研究条件为 `insufficient_group_size`，不偷偷放宽；当前档不以三人为硬门槛。

- **同一目标的两种表述**：新人必须在估计的三人之内，`newcomer_benefit >= high` 且 `lower_bound >=3`，其中三人包括新人；选择一个群体共同话题。
- **两个独立判据**：新人受益证据单独达标，群体潜在参与下界另外达三人；若“三人”意为新人以外三人则要求四名不同人，必须由产品口径明确。技术上用两个字段与两个拒绝原因 `newcomer_evidence_insufficient`/`group_coverage_insufficient`，不要隐式合成一个分数。

推荐暂采用“独立字段、研究档 AND、三人含新人”的定义，便于分别观察两个目标；当前档只执行同群新话题/侵入性/新人中档门槛，不把研究条件塞进现网。这里的“惠及”只可验“有证据的参与机会”，实际接纳须另收集人类反馈；文献没有给它的概率阈值。

## 数据契约、图与失败

宿主顺序：`B.formation → K.validate_candidates → B.learning/K.commit_learning → B.topic_background(②) → K.validate_background → B.evaluation → K.validate_ratings → B.selection(①) → K.validate_selection → ... → B.topic_lifecycle → K.final_admission`。是否把②放在 EVALUATE 前后可按模型预算调整，但必须先有合法候选、先于①，且最终话题闸门不消失。宿主在无 Core/Store guard 时构造快照/异步调用，回来核对 turn/version/epoch、证据 chat/scope/时间与候选 ID。插件结果不得给新消息 ID、伪造 `message_key` 或虚构服务器 ts。若当前稳定话题身份/候选来源还未完成，使用 `unknown`，不拿 `thought.created` 充当话题最后活动。

建议版本化 JSON 形态：
```
BackgroundInput{schema_version,turn_token,chat_scope,candidates:[{id,source_ref,topic_ref,stage}],current_context,member_evidence_scopes}
BackgroundAssessment{candidate_id,newcomer:{tier,evidence_refs},potential_beneficiaries:{lower_bound,upper_bound,unique_subject_refs},intrusion:low|medium|high,visibility:group|private|unknown,missing:[],evidence:[{ref,source_type,ts,scope,consent_scope}]}
SelectionInput{turn_token,valid_candidates,ratings,background_by_candidate,topic_lifecycle,host_eligibility,direction_policy}
SelectionDecision=Select{candidate_id,action,opening_form,confidence,evidence_refs,direction}|Wait{reason,until}|Withhold{reason}
```
宿主只接受候选集中已存在的 ID、同群证据、唯一有效生产者和已声明开口形式；原始资料或自造 `message_key` 被拒。`PortReply<T>` 四态可借鉴，但它属于基础设施端口且仅描述调用状态，不能替代 `Select/Wait/Withhold`、置信度和证据；使用独立 `BehaviorReply<T>` 同构的 ok/unsupported/degraded/failed 包装。`degraded` 只有当携带可验证且足以满足本档硬条件的结果才可使用，不能把背景服务不可用写成“低置信成功”。

依赖：宿主快照/同群身份与可见性 → ②；②可声明可选 MemoryPort 与 ModelPort（若做语义归纳）；①必需②、候选/评分/话题状态，可选 ModelPort（若解释/排序）；持久化/事务由宿主掌管，不给原始 DbPort；选择器不得直接调用 ChatPort。`② unavailable/timeout/invalid/unknown essential scope` → `proactive_blocked_background_unavailable` 且可查询；`① unavailable` → `proactive_blocked_selector_unavailable`；两者不回退到既有主动发言链。仅在整个新组合**未配置/未启用**时保留旧路径。回复路径照旧。配置加载/热切换失败则保留旧 generation 的资源并标示 `target != applied`、逐插件错误，但该组合的主动发言先阻断直到状态可判定，不能暗中由旧选择器照常发。每轮留审计：turn、chat、候选 ID、图代、插件 ID、证据 ref、置信档、人数上下界、隐私拒绝原因、选择/等待/阻断，不记简历原文。

## 清单、配置、敏感数据、热重载

提议两包 `plugins/topic_background/manifest.json` 和 `plugins/topic_selector/manifest.json`，共享安装发现，但 `kind:"Behavior"`, `behavior_api_version`, `slots`, `consumes/produces`, `after/before`, `dependencies`, `config_schema`, `host_permissions`, `data_access`, `hot_reload` 属未来行为清单扩展，**当前代码不能识别**。不伪装成 `Model` 或 `Persona`。`plugins/plugins.json` 仍用现有 `{schema_version:1,mode:"manual",plugins:[...]}`，背景项在前，选择器项在后并以 `bindings.background_assessment="topic-background"` 指向前者；均默认 `enabled:false`，AB3 识别只读、草稿暂存、显式启用；启用时原子编图/构造/发布，不能半启动。例示配置而不落盘：背景 `allow_profile_for_selection=false`, `allowed_profile_fields=[]`, `max_evidence_age_seconds`, `require_subject_consent=true`；选择器 `policy=current|research`, `min_confidence_tier=medium`, `targeted_min_tier=high`, `direction_policy=shared_current_first`, `allow_resume_only=false`, `research_min_beneficiaries=3`, `max_intrusion=low`。字段 `number` 用于数值，避免现有 `integer`/`number` 类型分歧。方向是选择器参数，不第三包。

`host_permissions` 现有四种机械活动 Http/Spawn/ReadFile/Log，**没有读成员背景**；不能把 `ReadFile` 当成数据语义授权。单列 `data_access`/`input_scopes` 声明 `source=member_profile`, `purpose=topic_selection`, `scope=same_chat`, `fields`, `public_disclosure=false`, retention 与对象确认状态；AB2 显示作用域和数据流向，审计实际调用。若来源是文件/远程接口，`read_file`/`http` 另行如实列宿主机械活动。遵循 `behavior-plugin-design.md` §7.6/§9 的较晚裁决：主人所选插件按“声明、可见、可审计”处理，不把空权限当安全封堵；但聊天隔离、数据最小化、不能未经本人确认对群披露是正确性/隐私边界。文档 §1/§4 和 `plugins/README.md` 尚有旧的“默认拒绝/逐次授权”语句，需在实施前统一口径，规划本轮不改。

初版两包均 `transport:"InProcess"`, `hot_reload:false`：当前仅有 Rust 静态构造器/单体运行，且替换图代和在途 turn 排空尚未作为这两个槽的运行能力验收。AB3 对 false/依赖拓扑变化可走整框架重启；重启前后显示 target/applied/reason。以后若实现原子图代切换、在途排空、证据快照失效与边界测试，再可将纯参数调整声明热重载。3.12 的统一进程外协议目前仍待实施；要 Sidecar/跨语言，需版本化 `BehaviorInput/Reply`、握手、deadline、身份/证据数据最小化与进程生命周期。不能用六端口 gRPC 契约代替行为槽契约。

## 顺序、最小路径与验收

1. **先补可核验事实**：候选稳定 ID/来源、话题身份/结束/最后活动、当前共享对象与新人当前参与标记；无事实先标 unknown。12:03“47 分钟旧汉堡、无当下证据/引用”必须被现有最终闸门阻断，不等待插件。无需先开启按群学习。
2. **先做②**：定义受控快照、同群数据范围、`BackgroundAssessment` 和可查询失败状态；可先包现有内核上下文/只用群内公开消息，不读简历，输出保守判断。此时是行为逻辑验证，不声称清单插件已可用。未来实验背景通过明确的数据声明和成员确认接入。
3. **再做①**：先包候选评分/一次选择及 wait，把②的判断加入候选资格；同群当前共享对象中档可群体短评，定向高档；选中后仍走 topic_lifecycle/最终闸门。不可用即阻断主动，不退旧选择器。保留原响应路线。
4. **最后接清单行为插件**：添加行为 manifest 的发现/校验、静态行为构造器、B 槽替换执行、图代原子发布、AB2/AB3 状态；这是不能仅靠包装解决的框架扩展。3.12 Sidecar 后续独立进行。不得改现有六端口中立契约语义。

目标测试：识别只读不构造；②失败/超时/无同群证据时主动链阻断且逐插件状态可查；中档只产不点名短评，高档定向需新人当前明确证据；简历只影响排序，不进入最终 prompt/群消息，未确认披露被拒；研究档下界 2 拒绝/3 且含新人可进入最终闸门，两人群原因可见；已结束 47 分钟旧话题无可靠新证据被拦；选择器输出伪候选或跨群证据被宿主拒；两包未启用时旧路径逐字行为不变；失败发布不出现半生效图；到最后发送前仍复核参与/静默/配额/引用。评估指标分“开口”“互相回应”“新人接纳”，后者需要人类反馈，不能把潜在人数当真实收益。

需产品裁决：研究档三人是否包括新人，还是三名其他人；当前档是否要求新人至少中档证据（建议仅有实际新人在场时要求）；成员背景的来源、字段、本人对“仅选题/可对群披露”的分别确认和撤回机制；是否允许没有当前公开共享对象但有经审资料的新话题（建议只在显式模式中）。本轮不改任何仓库文件、不跑测试、不提交。
