# P6c 人类化行为

你是 qq-inner-agent 的 Rust 移植工程师。工作区是隔离的 git worktree。

## 定位:混合 —— 一项 parity,三项门控新功能

- **长度分档接线是 parity 项**:JS 的 `src/engine.mjs` 已经调用 `pickLengthTarget` 并把 `lengthTarget` 传进
  articulation payload;Rust 引擎(P6a)当时有意没做,**现在要补上对齐 JS**。它不是新功能,不要加开关。
- 其余三项是**新功能,只做 Rust,且必须开关化默认关闭**(见 `docs/working/prompt-and-learning-design.md` 状态总表)。

## 必读

- `docs/working/prompt-and-learning-design.md` 第九节(三层决策里"说什么"这一层的现状)、状态总表;
- `docs/working/human-like-replies.md`(改动 1~8);
- `src/engine.mjs`(看 JS 怎么接 `lengthTarget`)、`src/policy.mjs` 的 `pickLengthTarget`;
- 已合并的 `rust/src/engine.rs`、`rust/src/policy.rs`(已有 `pick_length_target`)、`rust/src/prompts.rs`(生成器产物,勿手改)。

## 任务

### 1. 长度分档接线(parity,对齐 JS)

引擎组装 articulation payload 时,调用 `policy::pick_length_target(hint, random)`,把结果作为 `lengthTarget` 传入,
并在 `message_sent` 日志里带出 `lengthTarget`(与 JS 一致)。**被直接点名(`hint=self`)时不得给 `tiny`** —— `pick_length_target` 已保证,接线即可。

### 2. 按群表情/face 频率(新,门控)

`p(chat) = clamp(该群人类 face 使用率 × 0.8, 0, 0.35)`,冷启动保守 0.08。
该群人类 face 使用率从 `messages` 里统计(该群、非自己、含 face 段的比例,带时间衰减)。
开关名建议 `agent.emoji.learnFrequency`(默认 false);**不得引入新依赖**。

### 3. 只发表情(新,门控)

模型返回**空文本 + 一个 face** 时,发送**只有 face 段**的消息。

- 硬性限制:仅 `open`/`other` 且动机中低时允许;**被直接点名、对方求助或表达难过时禁止**;每 chat 连续不超过 1 次;
- **提示词改动必须用运行时片段,不要改 `prompts.rs`**(它是生成器产物,改了会破坏 `prompts_parity`):
  当该开关启用时,在 articulation 的 user 内容里**追加一段指令**(允许空文本 + 单 face),
  默认关闭时不追加 → 生成的 `prompts.rs` 与 JS 保持一致;
- ⚠️ 现有 `prompts_parity` 有一条断言"提示词不得含'空文本'"—— 那是针对**生成产物**的;
  运行时片段不进入生成产物,所以那条断言**不应被改**。要分清"生成产物"与"运行时追加"。

### 4. 多气泡 + 打字延迟(新,门控)

ARTICULATE 返回多条时按间隔依次发送;发送前加与长度成比例的短延迟(含抖动)。
若实现不便,可**明确说明未做**,不要静默省略。

## 硬性约束

1. 不得引入新依赖;不得改 `rust/` 以外文件;
2. 不得手改 `prompts.rs`(要用生成器 + 运行时片段);
3. 新功能一律**开关化默认关闭**,默认路径与 JS 逐字一致(既有 parity 测试照旧全过);
4. 中文注释,重点标注:哪些是 parity、哪些是门控新功能、以及"运行时片段 vs 生成产物"的区分。

## 验收

1. `cargo build --release`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 全过;
2. 长度接线与 JS 交叉验证(用 engine_parity 的 golden 方式);`message_sent` 带 `lengthTarget`;
3. face 频率:冷启动 0.08、按使用率 clamp 到 [0,0.35]、开关默认关闭;
4. 只发表情:默认关闭时行为不变;开启时"空文本+face"能发、被点名/求助/难过时禁止、连续不超过 1 次;
5. 多气泡/延迟:若实现,验证条数与间隔;若未实现,在回报里明确说;
6. 测试强度按第十八节:不变量精确,启发式用区间/方向。

## 交付

1. `git add -A && git commit`,英文。
2. 一段话回报:哪些是 parity、哪些是门控新功能、第 4 项做没做、与 JS/设计的已知差异。
