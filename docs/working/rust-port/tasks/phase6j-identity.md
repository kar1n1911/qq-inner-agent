# P6j 身份自治(§22)

设计权威:`docs/working/prompt-and-learning-design.md` 第二十二节。

## 定位
全新功能,只做 Rust、开关化默认关闭。改账号身份(昵称/群名片/头像)属高风险,需 `ownerUin` 确认。

## 任务

### 1. onebot.rs:三个 set_* 原语
- `set_group_card(group_id: &str, card: &str) -> Reply`:调 `set_group_card`,params `{group_id, user_id: self_id, card}`
- `set_qq_profile(nickname: &str) -> Reply`:调 `set_qq_profile`,params `{nickname}`
- `set_qq_avatar(file: &str) -> Reply`:调 `set_qq_avatar`,params `{file}`
(NapCat 支持三者;代码只发 action,与桥无关。)

### 2. config.rs:`agent.identity`
```
Identity { enabled: bool(默认 false), min_traits: usize(默认 3), min_age_days: f64(默认 7),
           allow_nickname: bool(默认 false), allow_group_card: bool(默认 false), allow_avatar: bool(默认 false) }
```

### 3. identity.rs:判断 + 提议 + 状态
- `enough(store, chat, now, cfg) -> Result<bool>`:该群群龄 >= min_age_days 且 group traits 条数 >= min_traits。
- `propose(store, chat) -> Result<Proposal>`:从群 traits + persona 归纳 nickname / group_card(简单规则:persona + 最高频 trait)。
- 一张小表 `identity_proposal` 存 pending 提案 + `applied` 标记(避免重复改名)。

### 4. 引擎接线:周期检查 + owner 确认
- 周期 tick(如每小时)里:若 `identity.enabled && enough && 无 pending && 未 applied` → 生成提案、存 pending、
  给 owner 私聊发"我学够了,建议改名 X / 群名片 Y,回复 /同意改名 或 /忽略"。
- 扩展 owner 指令(复用 `owner_teaching.rs` 的 private+ownerUin 授权边界):
  `/同意改名` → 读 pending,按 allow_nickname/allow_group_card/allow_avatar 逐个 set_group_card/set_qq_profile/set_qq_avatar,清 pending、记 applied;
  `/忽略` → 清 pending。

## 硬约束
1. 全部开关化默认关闭;不改 `prompts.rs`(生成产物);不引新依赖;
2. 中文注释,标注高风险 + owner 确认边界。

## 验收
`cargo check` + `cargo clippy --all-targets -- -D warnings` 通过;默认关闭时行为与现在逐字一致(parity 全过)。

## 交付
`git add -A && git commit`(英文),一段话回报。
