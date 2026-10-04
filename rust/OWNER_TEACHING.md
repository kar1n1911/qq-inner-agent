# 主人私聊教学（P6e）

仅 Rust 支持，默认关闭；不修改 JS defaults 快照。配置示例：

```json
{
  "agent": {
    "ownerTeaching": {
      "enabled": true,
      "ownerUin": "1950202917"
    }
  }
}
```

`ownerUin` 为 QQ ID 字符串，省略时使用 `1950202917`。仍受现有消息准入规则（如 `allowedUsers`、`ignoredUsers`）约束。群消息及非主人消息不解析，关闭时全部作为普通聊天。

- `/黑话 A=B`：写入 expressions，kind 为 jargon；词限 40、意思限 160 个 UTF-16 字符。再次教学同一词会更新意思。
- `/记住 内容`：写入 long_term，长度上限为 500 与 memory.longChars 中较小值。
- `/忘记 关键词`：在主人私聊中按字面子串删除匹配的长期记忆、traits、旧式 learned_memories 和黑话/表达；同时清理对应记忆修订。关键词限 500 个 UTF-16 字符，空值不执行。

subject 仅为 `person:<ownerUin>`，来源精确为 `["owner-teaching"]`，不引用真实消息 ID。已有 memory/expressions 接口负责持久化与保留期管理，结果可用 `learning.list` 查看。教学黑话允许单个特殊来源召回，普通学习的多人/多来源要求不变。

教学绕过常规学习的 8 条/300 秒门控，但保留 subject、类型和长度校验。当前仓库没有 §16 自我审核实现，因此按阶段任务使用形式校验兜底，未新增模型审核器。

命中的指令在消息去重后执行，通过独立异步队列回复“记住了”“忘记了”或“没看懂：原因”，不进入 capture、普通学习及普通回复流程。队列使用现有 delivery 记录；发送失败/结果不确定不自动重发。dryRun 时不发送确认。教学内容使用原始消息校验，避免聊天截断使超长内容被误接受。
