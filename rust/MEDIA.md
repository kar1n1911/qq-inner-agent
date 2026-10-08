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

- `Store::enable_media()` 幂等创建五张新表，已有表不变。`media_assets` 是群/私聊
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

## 来源可公开性（13.8 补充要求）

`media_assets.source_tier` 默认 **unknown**，与 private 一样禁止出群，群内仍可使用。
新增 `distinct_senders`（默认 0）、`source_override`（默认 NULL）、
`public_corpus_match`（默认 0）。`media_senders` 以 `(chat, hash, sender)` 唯一键准确
计不同人类数，重复消息/段不增加人数；人数、素材计数和来源判定在同一事务内提交。
旧库在 `enable_media()` 中通过 `PRAGMA table_info` 探测后 ALTER；只从仍存在的 usage
引用及人类消息回填人数。已删除历史无法恢复，所以旧库人数是可证明的下界，不从
occurrences 猜人数。淘汰素材同时删除发送者索引。

已核对 `onebot.rs::receive`：通知携带原始 JSON，未筛除扩展字段；采集器数组段也原样
保留。现有仓库事件样例可验证的只有 image 的 `file/url`、face 的 `id` 等，没有实际
桥返回的 `sub_type`、收藏表情/sticker 或 emoji 标记样本及其公开性语义。配置中的
`agent.emoji` 是发送装饰设置，不是入站来源证据。因此不臆造字段映射；即使收到
未验证的 sticker/emoji 标志，也不能把群友照片升级为公开。新增测试中的这些标记
仅为防误放行的合成负例，不代表已确认协议。

`media_source::classify(Evidence, widespread_threshold)` 是纯函数：人工覆盖优先，
其次是本地公开语料 SHA-256 完全匹配；其余 unknown。人数越多越可能是群内常用素材，
但无法证明公开可搜，故传播度仅产生辅助 `widespread` 信号，**永不单独放行**。
`media_source::can_use(tier, source_chat, target_chat)` 是纯准入函数：同一 chat 的三个档
都允许；不同群仅 public 允许；私聊不参与跨群共享。此阶段不实现跨群检索或发送。

`media::Config.public_corpus_dir` 可指定运维已确认公开的本地语料目录（默认 None）。
构造启用的 Collector 时递归散列普通文件，忽略符号链接及超限文件；缺失/不可读视为空，
不凭文件名推断，不下载任何语料。目录变更后重建 Collector 才刷新快照。已命中的
正面证据在素材存续期保留；若语料标错，使用人工 private 覆盖撤销放行。
来源判定没有网络调用，更不会上传图片或反向图搜；原有临时 URL 的入站下载仍只用于落盘。

后台 API：`Store::set_media_source_override(chat, hash, Some(media_source::Override::Public))`
或 `Private`，`None` 清除覆盖并按已保存证据重算。覆盖按 chat/hash 隔离，重复采集及
数据库重开均保留，人工 private 优先于语料匹配；素材被容量策略淘汰时覆盖随记录删除。

测试精确断言默认未知、人工覆盖优先级/撤销/持久化、不同发送者数、旧库迁移和出群闸门；
传播信号只断言随人数增加的方向。本地语料哈希使用公开 abc 向量，不从实现反推期望值。
当前工作区设计文档尚无 13.8 正文，本节实现依据本次任务给出的新增要求。

## Optional image OCR

`agent.ocr` is disabled by default. Enable it with
`{"enabled":true,"engine":"tesseract","languages":"chi_sim+eng","binary":"tesseract","timeoutSeconds":20,"maxChars":800,"maxBytes":4194304}`.
Install Tesseract and the requested language data separately. No Rust dependency is added.

`media/ocr.rs::build` dispatches the `Engine::recognize(&[u8])` implementation;
add an engine branch there to integrate PaddleOCR or a vision API. The built
Tesseract adapter clips output by Unicode characters. Image downloads reuse
`media::read_image` and enforce `maxBytes` before recognition.

Accepted live and backfill image messages enter a bounded background queue.
Download/recognition never holds the database mutex; errors or queue saturation
only emit `ocr_failed`. A stopped/replaced engine discards queued OCR work.
The `media_ocr` table is created only when enabled, keyed by chat/message ID;
message deletion also removes its OCR row. Multiple images are combined in
segment order within the per-message character budget.

History, backlog history and backlog samples enhance the first `[image]` or
`[图片]` placeholder to `[image: text]` when the result is ready. Original message
text is unchanged. A response built before OCR finishes retains its placeholder;
later contexts include the text. Disabling OCR also disables enhancement of
previously stored results, and starts no worker or image download.
