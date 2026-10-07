# P6g 剩余设计实现(学习管线:分诊 + 自我审核)

设计权威:`docs/working/prompt-and-learning-design.md` **第四、十六节**。

## 定位

两处都改**学习管线**:先改 FORM 输出契约(提示词,JS→生成→Rust),再在 Rust 侧执行判定。
按"只做 Rust"规则:**行为逻辑只在 Rust 实现**,但 FORM 契约与审核提示词走
`src/prompts.mjs` → `gen-prompts.mjs` → `prompts.rs`,且**保持 JSON 契约向后兼容**。

## 任务 1:学习分诊(第四节)

`FORM` 的 `learning.layers[]` 增加 `verdict` 字段:

```json
{ "operation":"upsert|forget", "verdict":"learn|partial|skip", ... }
```

| verdict | 落库方式 |
| --- | --- |
| `learn` | 正常 upsert,confidence 高 |
| `skip` | **不落库**,决策日志留 `skipped` 记录 + `reason` |
| `partial` | 落库但 `confidence ≤ 0.5` + `pending` 标记;获得第 N 条独立新证据后升格 `learn`(N 可配,默认 2) |

实现要求:

- `verdict` 缺省视为 `learn`(向后兼容现有输出);非法 verdict **整批拒绝**(沿用 `invalid_*` 风格);
- 判定依据(来源充分性/稳定性/归属/敏感性/可复用性)写进**任务契约**,不写进人设;
- 敏感性(身份/健康/财务/位置/亲密关系)→ 一律 `skip`;
- `partial` 升格规则**由代码执行**(不依赖模型自觉),可配置 N。

## 任务 2:落库前自我审核(第十六节)

在**写入前**加一道审核,只在学习时调用(已受 8 条/300 秒门控,成本可接受):

- 输入:候选条目 + 其**来源消息**(只给被引用的那几条);
- 输出:`keep` / `drop` / `rewrite`(+改写文本)+ 一句理由;
- `drop` 与 `rewrite` 都进决策日志;
- 审核提示词用**"找出不该记住的理由"**框架,并明确 drop 理由清单(一次性情绪、转述、重复、敏感推断);
- 避免变成橡皮图章:同模型审自己产出倾向批准,靠"找理由 drop"框架对抗。

## 验收

1. `cargo build --release`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 全过;
2. 分诊:skip 不落库但留日志、partial 低置信+pending、升格规则按 N 执行、非法 verdict 整批拒绝;
3. 自我审核:keep/drop/rewrite 三条路各测到;drop 与 rewrite 进日志;来源只给被引用的消息;
4. 默认路径向后兼容(无 verdict 视为 learn,与既有 parity 一致);
5. 测试强度按第十八节:不变量精确,启发式区间/方向。

## 交付

`git add -A && git commit`(英文),一段话回报:verdict 三条路怎么落库、升格与审核怎么测的、与设计的偏差。
