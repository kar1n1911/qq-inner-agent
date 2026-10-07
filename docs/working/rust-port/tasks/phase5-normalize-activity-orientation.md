你是 qq-inner-agent 的 Rust 移植工程师。当前工作区是**隔离的 git worktree**,基线是 P4 之后的
`main`。改动不影响主分支。

## 前置

`rust/src/` 下已有 `config.rs`、`settings.rs`、`text.rs`、`policy.rs`、`sending.rs`、
`prompts.rs`、`onebot.rs`、`store.rs`。先读它们再动手。

**注意**:`policy.rs` 已经实现了 `allowed` / `quiet` / `active_at` / `select` /
`pick_length_target` / `repeated`,以及一个两步解析时区的 `Zone`(**先用固定偏移
`+08:00` 这类,再回退 IANA 区域名**)。本阶段**不要重复实现**这些,直接复用。

## 必读

- `docs/working/rust-port/ARCHITECTURE.md`(不可变更的契约)
- `docs/working/rust-port/SURVEY.md` —— **第 8.5、8.6 节**(活动节奏与入群观察),第 3 节(表结构)
- `src/activity.mjs`(44 行)、`src/orientation.mjs`、`src/policy.mjs` 的 `normalize`

## 任务:P5 —— 事件解析、活动节奏、入群观察

### 1. `normalize`(补在 `policy.rs` 里)

这是 P4 之前**有意没做**的一块,因为它与 OneBot 事件形状耦合。请逐行对齐
`src/policy.mjs` 的 `normalize(event, selfId, a, now)`:

- 只接受 `post_type === "message"` 且 `message_type ∈ {group, private}`
- 过滤自身消息(含 `self_id` 校验)、`ignoredUsers`、未启用的 chat
- 时间窗口:`!Number.isFinite(ts)`、`now - ts > activeWindowSeconds`、`ts > now + 60` 都要拒绝
- 数组消息与 CQ 字符串消息**两种**都要支持;`at` / `face` / `reply` / 其他附件标记的处理要一致
- `cqDecode` 的四个实体替换(**顺序**是 `&#44;` → `&#91;` → `&#93;` → `&amp;`)
- `text.trim().slice(0, maxInputChars)` —— JS 的 `.slice` 按 **UTF-16 码元**截断,
  可能切断代理对。请明确选择:要么复刻该行为,要么按 `char` 截断并**在注释与测试里写明差异**
- `named` 别名判定(`alias + ':'`、`alias + '：'`、`'@' + alias + ' '`,大小写不敏感,只在**开头**匹配)
- `hint`:`addressed ? "self" : atOther ? "other" : "open"`

返回一个强类型 `Message { chat, id, sender, name, text, ts, self, hint }`。

### 2. `activity.rs`

- `activity_probability(now, schedule, rhythm)` —— 复刻高斯形**静默**曲线。
  注意 JS 是 `edge - (edge - center) * clamp((gaussian - edge) / (1 - edge), 0, 1)`,
  其中 `gaussian = exp(-0.5 * ((x - 0.5) / sigma)^2)`、`edge = exp(-0.5 * (0.5 / sigma)^2)`;
  `x` 是静默区间内的进度(0..1,跨午夜回绕)。
- `ActivityRhythm::snapshot(now)`:
  - `signature = JSON([schedule, rhythm])`;**signature 变化即重抽**
  - `now < started`(时钟回拨)也重抽
  - 时长在 `[min, max]` 内均匀抽样;相邻块可选到同一状态,因此连续活跃可以超过单块上限
  - 状态与到期时间持久化在 `activity_rhythm` 单行(`id = 1`)
  - `enabled === false` 时直接回落到 `active_at(schedule)`

### 3. `orientation.rs`

复刻 `GroupOrientation` 的状态机:`ensure` / `get` / `joined` / `observe` / `profile` / `beforeSpeak`。

- 三处来源采集:`get_group_info`、`_get_group_notice`、`get_group_msg_history`;
  任一失败/不支持/格式错误都记为 **unavailable,绝不编造**
- 阈值判定 `observationSatisfied`:默认**同时**满足 `minSeconds` 与 `minMessages`;
  `thresholdMode` 为 `either` 时满足其一即可;`enabled === false` 时直接放行
- 阈值满足后发一次中文 `ORIENT` 请求(用 `prompts` 里已有的边界文本 + SURVEY 里记录的
  ORIENT 提示词;若 `prompts.rs` 里没有 ORIENT,请**用生成器脚本补上**,不要手写)
- 输出无效或 provider 失败:**闸门保持关闭**,`retry_at = now + 60`
- `epoch` 在重新入群时递增,使在途分析失效;**重复入群通知不得重复递增**
- `profile(chat)` 只暴露可供发言阶段参考的 style/overview/topics

## 硬性约束

1. **不得引入新依赖。**
2. **不得修改 `rust/` 以外的任何文件。**
3. 中文注释,重点标注:退避与重抽触发条件、时区/DST、UTF-16 vs `char` 截断、
   `epoch` 失效时机、以及"失败即不可用而非编造"的边界。

## 验收标准

1. `cargo build --release` 成功;`cargo clippy --all-targets -- -D warnings` 零警告;`cargo test` 全过。
2. **必须与 JS 交叉验证**(Node 不可用时跳过,而不是失败):
   - `normalize` 的用例矩阵:数组与 CQ 字符串两种格式、`at` 自己/他人/`all`、
     face、reply、附件、空文本、超长文本(含代理对)、非白名单、被忽略用户、
     自身消息、时间过早/过晚、别名三种写法
   - `activity_probability` 在多个 `sigma` 与 `x` 上与 JS 逐值比对(**用二进制位传值**,
     因为 serde_json 解析十进制浮点可能差 1 ULP —— 参见 `tests/sending_parity.rs` 的做法)
   - `observationSatisfied` 的 both/either/disabled 三态与边界
3. 时间与随机都必须可注入,测试里不要依赖真实时钟。

## 交付

1. `git add -A && git commit`,英文 commit message。
2. 用**一段话**回报:实现了什么、测试怎么跑、与 JS 的已知差异或不确定点。

---

## ⛔ 本阶段**不要**实现的设计(重要)

`docs/working/prompt-and-learning-design.md` 里记录了几项**已设计但尚未实施**的改动。
本项目的铁律是**与现行 JS 行为逐字对齐**,所以在本阶段:

- **不要**实现提示词分层重构(把身份/背景与任务契约、行为准则拆开);
- **不要**实现学习分诊(`learn` / `partial` / `skip` 三档与自动升格);
- **不要**实现 affect 指标(心情值 / 好感度 / 认同度,二维心情与四象限 disposition);
- **不要**实现记忆召回下钻(`recall` 字段、回查原始历史与语境)。

**一律以 `src/*.mjs` 的现行实现为准**;遇到"设计文档说要改、但 JS 还没改"的地方,
按 JS 现在的行为移植,并在回报里提一句你注意到了这处设计。

这些改动会在 JS 侧先行落地后,再单独派发移植任务。

