# P6f 剩余设计实现(提示词类:分层 + 责任线)

设计权威:`docs/working/prompt-and-learning-design.md` **第二、十四节**。

## 定位

两处都改**提示词文本**,源头在 `src/prompts.mjs`(JS),经 `rust/tools/gen-prompts.mjs`
生成到 `rust/src/prompts.rs`。**改完后必须重跑生成器并让 `prompts_parity` 逐字通过。**

## 任务 1:提示词分层(第二节)

把提示词在**设计/生成/测试上**分成三层,但发送时仍随请求(模型只有 system/user 两段):

| 层 | 内容 | 回答的问题 |
| --- | --- | --- |
| ① 身份背景 | `persona` + `personality(behavior/replyStyle/interests/variants)` + 学到的 `traits` | 我是谁、我怎么说话 |
| ② 任务契约 | TASK 标识 + 输入字段说明 + 输出 JSON schema + 失败即拒绝 | 这次产出什么形状 |
| ③ 行为准则 | 防注入、不可信数据、表情用法、反 AI 腔、语言选择 | 什么不该做 |

实现要求:

- `src/prompts.mjs` 把 per-task 常量**只含契约、不再复述人设**;人设只在 ① 出现一次;
- ③ 拆成**若干命名规则片段**(`boundary` 等),可按场景组合、单独测试、单独关闭;
- **不改变任何输出 JSON 契约与错误码**(否则行为漂移);这是纯结构重构。

## 任务 2:虚构责任线(第十四节)

`boundary` 文本把"不得编造亲身经历"改为**责任线**:

| 类别 | 口径 |
| --- | --- |
| 无害的经历虚构("刚看到一只猫趴在键盘上") | ✅ 放开 |
| 情绪/状态表达("我今天有点困") | ✅ 放开 |
| 涉及对方决策或利益("我帮你问过了"、"这个药能吃") | ❌ 禁止 |
| 被直接问是不是 AI | ❌ 不主动冒充(可不正面回答) |
| 涉及第三方的具体事实("XX 说他不来了") | ❌ 禁止(会真实误导人) |

落地:`boundary`(JS)→ 生成 `prompts.rs` → **同步更新 `prompts_parity` 里"不编造经历"的断言**
(改为断言"涉及对方决策/利益/第三方事实仍禁止",且"无害经历虚构被允许")。

## 验收

1. `cargo build --release`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 全过;
2. `prompts_parity` 逐字通过(生成产物与 JS 一致);
3. 分层重构**不改变**任何 JSON 契约与错误码(用现有 golden/parity 证明);
4. 责任线的 parity 断言更新后仍精确。

## 交付

`git add -A && git commit`(英文),一段话回报:分了三层各自含什么、责任线改了什么、parity 怎么过的。
