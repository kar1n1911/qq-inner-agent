# 群内装饰素材采集（P5b）

此阶段只有 Rust 实现，没有新增依赖，也没有修改 JS 或默认 store schema。

## 接入

当前 Rust CLI 只有配置、自检、数据库结构和 OneBot 连接校验，没有常驻 Engine。
运行时收到 `Notification::Event(event)` 后，在单个阻塞工作线程上调用：

```rust,ignore
let collector = media::Collector::new(data_dir, media::Config {
    enabled: true,
    classification: conversation::Config {
        closing_markers: vec!["好的".into(), "收到".into(), "那先这样".into(),
                              "睡了".into(), "明天见".into()],
        ..Default::default()
    },
    ..Default::default()
});
let report = collector.ingest(&store, &event, &bot.state().self_id, &agent, now)?;
// 调用方将 report.failures 写入自己的日志；错误不含临时 URL/凭据。
```

`Config` 可通过 serde 从独立配置反序列化；`enabled` 默认 `false`，不会自动加入
既有配置规范化流程。关闭时不建表、不写消息、不读取文件、不访问网络。
启用时复用 `policy::normalize` 的身份、白名单、忽略用户和消息时效检查，并调用
`Store::message` 保存上下文源消息（该调用幂等）。只采 array/CQ 中的 image 和 face。
不要并发调度多个采集者操作同一媒体目录；下载是同步且限时的，事务串行处理素材写入。

## 存储与边界

- `Store::enable_media()` 幂等创建四张新表，已有表不变。`media_assets` 是群/私聊
  隔离的索引，`media_contexts` 只含消息 id 和角色（usage/before/after）。正文只能关联
  `messages` 读取；历史被清理后引用可能不再解析，不保留正文副本。
- `media_receipts` 按消息与段序号避免重放加计数；一条消息中重复的两段仍计两次。
  失败段不写 receipt，可重试；淘汰素材后保留 receipt，旧事件不再复活它。
- 图片 SHA-256 复用 settings 中已有实现；内置 face 使用 `SHA256("face:" + id)`，
  不需要图片文件。fitness 初始 `{}`，不更新学习评分。
- 本地 `file`（亦支持简单 `file://` 路径）读取并复制；HTTP(S) `file` 当场下载。
  **未确认任何 OneBot 实现专有的取文件 API，因此没有调用或猜测该 API**。
  opaque file id、base64、仅有 `url` 字段均不猜测，作为采集失败返回。
- 默认单文件 2 MiB、全库文件字节总量 100 MiB、请求超时 15 秒，可配置。
  超限单文件跳过；总量不足按 occurrences、last_seen 淘汰最低频/最旧图片。
  扩展名按字节签名确定，未知格式用 `.bin`，避免同内容因 URL 后缀不同落两份。
- 先将完整字节写临时文件、sync、rename，再提交索引；普通数据库失败回滚并清理新文件。
  进程崩溃在文件落盘与数据库提交之间可能留下无索引文件；此阶段不提供崩溃孤儿清理器。
  淘汰的旧文件在提交后删除，删除失败作为失败报告返回，可能需运维清理残留。

## 两轴分类

`conversation::classify(messages, target, observed_at, config)` 是纯函数；输入显式包含
观察时间及目标后的消息。词元相似度复用 `text::similarity`，记录相邻相似度、交替次数、
观察间隔和标记命中，供日志复核。不根据沉默生成任何学习信号。

收束标记集默认空，必须由配置提供。有标记也要短消息、足够后续观察且无延续；
展开需要同话题短间隔交替；单发需要前后无重叠。自然终止、无法确认的转向、
缺少前后证据均 `confident: false`。词汇交叠不足以证明“由上文触发”，故转向不声称确定。
`media_stages` 保存分类及结构证据，不保存正文。入站更新最近已有分类；静默期间
运行时可显式调用 `Store::record_media_stage` 重新观察，本阶段没有新增计时引擎。

不实现检索发送、学习分诊、affect、作息学习、注意力漂移、跨群共享或自我审核。

## 验证

`tests/media_collect.rs` 用临时目录、临时 SQLite 和 loopback HTTP 服务测试；
SHA-256 使用公开 abc 向量，计数/文件数/隔离/回滚采用设计推导的精确断言。
分类采用合理类别和趋势约束，模糊输入的 `confident: false` 为精确不变量。
完整验收在 `rust/` 下运行：

```sh
cargo build --release
cargo clippy --all-targets -- -D warnings
cargo test
```
