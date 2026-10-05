# P6h 剩余设计实现(状态指标 + 二维心情 + 记忆召回)

设计权威:`docs/prompt-and-learning-design.md` **第六、七、八节**。

## 定位

全新功能(JS 无对应物),**只做 Rust** + 行为测试。因接入 `sending_probability` 属于
"改已迁移模块行为",故**必须开关化默认关闭**:默认中性、不影响既有 parity。

## 任务 1:affect 指标(第六节)

三个正交指标,互不推导:

| 维度 | 对象 | 含义 | 范围 |
| --- | --- | --- | --- |
| `mood` | 消息级 | 对这条内容的情绪反应 | −1…+1 |
| `agreement` | 消息级 | 事实/道理上是否同意 | −1…+1 |
| `affinity` | 人物级 | 对说话者的观感 | −1…+1 |

存储(新增两张表):

- `message_ratings(message_id, chat, mood, agreement, confidence, rated_at)`,只评人类消息;
- `affect_state(chat, subject, dimension, value, baseline, confidence, updated, sources)`,
  只放会衰减的状态(affinity、可选的群级/人物级 mood);`value ∈ [-1,1]`。

规则:

- 衰减(读时衰减写回):`value = baseline + (v0 - baseline) * 0.5^((t-updated)/half_life)`;
  affinity 半衰 5–14 天、mood 持续分量 2–6 小时、消息级不衰减;
- 有界步长(单次 ≤±0.15)、负性非对称(负面 −0.20 / 正面 +0.10)、按 confidence 缩放;
- **单向约束**:`agreement` **绝不**聚合进 `affinity`(代码保证 + 测试断言);
- 只影响行为(`sending_probability` 新因子),**绝不进内容**;永不复述数值、不作奖励优化、不施压、可归零。

## 任务 2:二维心情 + 四象限(第七节)

`mood` 是二维:`valence`(愉悦 −1…+1)× `rationality`(理性 +1 ↔ 感性 −1)。

纯函数 `disposition(valence, rationality) -> Angry | Withdrawn | Scrutinizing | Supportive`,映射到行为:

| disposition | 参与动机 | 长度档 | 语气 |
| --- | --- | --- | --- |
| `Angry` | ↑ | 偏短 | 直接、可带个人指向(可下调 affinity、可超常规闸门) |
| `Withdrawn` | ↓↓ | —— | 倾向不发言 |
| `Scrutinizing` | ↓ | 偏长严谨 | 克制、提高自检 |
| `Supportive` | ↑ | 中短 | 温和、不用玩笑黑话 |

- `Angry` 保留一个**熔断**:同 chat 无回应连续超发 ≤ N 条(默认 3),之后回常规闸门;
- 参与动机进 `sending_probability` 因子结构,语气/自检进 articulation 行为准则层,长度档与 `lengthTarget` 同源。

## 任务 3:记忆召回下钻(第八节)

两级召回:第一级 `memory_layers`(大意,常驻);第二级 `messages`(精确,按需)。

- 输出增加可选字段:
  `{"recall":{"needed":true,"why":"…","query":"关键词","aroundMessageId":"123","window":20}}`;
- 缺省 `needed:false`(向后兼容);
- 运行时按此做**一次受控检索**(限条数/字符,预算上限),结果带原始 id+时间戳喂回模型;
- 检索原文仍只是引用数据(防注入);下钻决定与命中数进决策日志,不进回复正文;
- 行为准则加一条:**细节未核实必须表达不确定,禁止用大意补全细节**。

## 硬性约束(全部)

1. 不引入新依赖;不改 `rust/` 以外文件(除提示词契约的 JS 生成器同步);
2. 全部**开关化默认关闭**,默认路径与 JS 逐字一致(parity 照旧全过);
3. 红线(永不复述数值/不作奖励优化/单向约束/跨聊天隔离)写成**代码断言**,不靠提示词自觉;
4. 中文注释,重点标注:三指标的正交性、衰减公式、单向约束、四象限映射、熔断、召回预算。

## 验收

1. `cargo build --release`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 全过;
2. 建表/衰减/更新/四象限纯函数各有单元测试;`agreement→affinity` 单向约束显式断言;
3. 开关关闭时 `sending_probability` 与 JS golden 一致(默认中性);开启时 mood/affinity 作为新因子生效;
4. `disposition` 四象限映射正确;`Angry` 熔断(N 条无回应)生效;
5. `recall` 受控检索:预算上限、结果带 id、缺省 needed:false;
6. 测试强度按第十八节:不变量精确,启发式区间/方向。

## 交付

`git add -A && git commit`(英文),一段话回报:三指标存储/衰减/单向约束、四象限与熔断、召回预算分别怎么测的、与设计的偏差。
