# 外部话题来源（Rust）

`agent.topicSource` 缺省或来源列表为空时完全关闭，不改变模型输入。

```json
{
  "agent": {
    "topicSource": {
      "github": ["esp32", "topic:sdr", "diy"],
      "feeds": ["https://example.org/forum.rss"],
      "intervalHours": 1,
      "maxPerHour": 1,
      "maxRequests": 4,
      "maxItems": 3,
      "maxChars": 600,
      "maxTotalChars": 1800,
      "cacheHours": 1,
      "threshold": 0.15
    }
  }
}
```

GitHub 使用 repositories search（按 updated 降序）；topic 精确查询写为
`topic:名称`。论坛提供 RSS 订阅 URL；支持 RSS item/title/description/category/link、
CDATA 和常见 XML 实体，不抓取任意论坛 HTML，不支持 Atom 或 DTD。
每次请求超时 10 秒、响应最多 256 KiB，不增加依赖。

仅在主动话题轮次、群作息内且近期有人类活动时抓取。群 traits 与近期人类消息中
至少出现两次的前 32 个词元组成兴趣集合；全角 ASCII 转半角、小写、字母数字分词，
中文补二元词，排除常见停用词。得分 = 交集词数 / sqrt(兴趣词数 × 条目词数)，
零交集永不入选。日志 `topic_source` 包含来源、得分与命中的兴趣词。

`intervalHours` 限制两次尝试的间隔，`maxPerHour` 为每群滑动小时次数上限。
`maxRequests` 限制一次尝试的网络请求；`maxItems` 同时限制每源解析条数与最终候选数。
`maxChars` 包含标题、描述、tags 和完整 URL 的 Unicode 字符数；先保留 URL，
再按标题、描述、tags 顺序截断，URL 自身放不下则丢弃。
`maxTotalChars` 限制一次匹配处理的总字符数（无关条目也消耗预算）。
来源缓存跨群共享、同源去重；失败同样缓存，避免坏源反复请求。
节流与缓存为进程内状态，重启/热重载后重置；抓取预算独立于聊天模型额度。

安全闸门比“感兴趣”更靠前：相关度筛选后的条目必须先通过责任线审核才能交给形成模型；
失败或不确定则不注入。外部条目作为不可信引用数据，要求保留来源 URL；
候选继续走原有评估/发送流程。审核模型调用仍受现有模型配额约束。
