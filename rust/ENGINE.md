# P6a Engine 核心与 P6c 人类化行为

`engine::Engine::new` 接收已校验 `Config`、共享 `Arc<Mutex<Store>>`、
`OrientationProvider`（真实 `Provider` 已实现）、`EngineTransport`（真实 `OneBot` 已实现）
与可注入 `Options`。不加载配置文件、不访问真实 data、不启动网络连接。
`ingest` 只入库和标记；调用者驱动 `tick`，`wait_idle` 等待本轮任务，`stop` 取消模型等待并等待已提交投递完成。
日志回调在同步段调用，不得重入 Engine 或获取同一 Store 锁。

`cycle` 中的数字注释对应 SURVEY §2.2 的 1–48 步。复用 policy、sending、orientation、
activity、memory、expression、store（含 learning）、prompts、provider 和 onebot；
P6c 没有引入依赖或修改生成提示词；仅开启表情新功能时创建辅助表。

## 并发与五项 obsolete 条件

core 锁串行化 chat 状态和每段同步决策；锁顺序为 core → store，网络 await 不持锁。
chat 按 JS Map 插入顺序遍历。准入时在同一锁内设置 busy、计入并发名额、捕获 version/id/now，
避免 Tokio 延迟首次 poll 吞掉下一条消息的 debounce。不同 chat 可以同时等待 I/O；同 chat 单飞。
已有 assessment 时直接 finish；finish 仅在原 version 仍匹配时清 pending、重置 hint、
按 pause/已发送设置 pauseDone，并 mark_handled。取消不会清掉更新版本的新消息。

| obsolete 条件 | Rust 中的触发源 |
| --- | --- |
| aborted | `stop()` 将 watch 信号置 true；热重载调用者先 stop 旧 Engine，再构造新实例 |
| version 改变 | 成功去重入库的新消息；新 epoch 的自己入群通知；不可用时的 tick。重复消息/重复入群通知不递增 |
| activity.started 改变 | 已有 ActivityRhythm 在块到期、时钟回拨、配置签名改变时重抽；即使相邻两块都 active 也作废 |
| learning epoch 改变 | 同一个 Store 上 `reset_learning`（包括单 subject 重置）；普通 learn 不改变 epoch |
| orientation epoch 改变 | GroupOrientation 的更新入群时间触发 `orientation_joined`；直接调用 joined 同样被检查，不依赖 version |

检查位置与 JS 相同：观察闸门后检查 version/aborted/available；formation 校验后、
候选入库后、评分回写后、forecast 后、articulation 装饰后检查全部五项。
因此**作废不是回滚所有旧状态**：例如 JS 在 evaluation 作废检查前已经写回评分，
此前合法提交的候选/学习也保留；Rust 保持这个顺序。未通过 freshness 的新学习、assessment、
投递不能提交。活动窗口、available、连接、静默与重复检查仍保持各自位置。

模型等待使用 `tokio::select!` + watch。真实 Provider 的 `spawn_blocking` 无法中断：
已启动请求甚至可能在取消后才实际发送，已准入预算不退还，服务端仍可能继续生成。
旧任务结果不能写入新引擎。测试用可控阻塞线程模拟该边界，不访问网络。
停止后可在新实例上 `inherit_chats(old.chats())` 保留允许 chat 并重置 busy/lastThink；
调用者只应在新实例启动前调用一次。不在本阶段实现 revision watcher。
`restore` 只恢复最近人类消息位置，保留记忆但不重放或因旧历史主动发言。

投递在调用 transport 前写 pending、消费候选并 finish。传输一旦开始，不因模型取消信号
抛弃结果；stop 等待投递结束。成功/确定失败/不确定分别落库，后二者均不自动重发。
崩溃后的 pending 由现有 `recover_deliveries` 标 uncertain，恢复也不重发。

## 验证与明确差异

运行（在 rust 目录）：

```sh
cargo build --release
cargo clippy --all-targets -- -D warnings
cargo test
```

`tests/engine_parity.rs` 的脚本同时喂给 Rust Engine 与 `fixtures/engine-oracle.mjs` 调用的
**真实 JS Engine**。两边使用真实内存 Store、mock provider/transport、注入时钟/随机，
比较每个模型阶段、决策序列、日志、候选池、投递/assessment/handled 和 chat 状态。
涵盖点名/开放/显式 other、突发合并、冷却/静默/配额、各模型阶段作废、候选保留、
非法输出、学习、API 预算、不确定送达与 dry-run。UUID 不比较；分数跨语言仅用浮点容差。
Node 不在 PATH 时只跳过 JS oracle，Rust 不变量测试仍执行。发送入口精确断言已有 pending
投递和 handled；并发/取消测试用屏障而非真实时间推进业务时钟。
启发式断言用阈值方向和分数区间，不断言“正确质量分数”；发送次数、作用域、阶段顺序精确断言。

明确差异与边界：

- P6c 已无条件接入已有 `pick_length_target`：沿用 JS 的 expressionRandom，在装饰候选之后抽样，
  articulation payload 与 `message_sent` 都包含 `lengthTarget`；self 不会得到 tiny。
  engine golden 比较该字段、日志及档位边界，默认关闭新功能时保持 JS 行为。
- P6c **未实现第 4 项多气泡/打字延迟**；也未扩展三层决策或零模型初筛。
- 候选文本复用既有 `policy::clip_chars`，按 Unicode 标量字符截断 300；JS 按 UTF-16 码元截断。
  非 BMP 边界有显式测试，普通中文/ASCII golden 对齐；不生成孤立代理项。
- Rust 取消丢弃 future，不能停止阻塞传输；引擎模型等待取消错误码为 `aborted`，
  JS 底层 AbortSignal 的错误码可能由 fetch/Node 版本决定。取消后的预算与持久化不变量按上述测试。
- 随机源分别注入发送、候选选择、表达与活动；JS select 原本用全局 Math.random，oracle 单独固定。
  golden 验证确定输入下的调度/决策，不声称真实模型输出或时区数据库版本完全相同。


## P6c 门控新功能

配置 `agent.emoji.learnFrequency`、`agent.emoji.faceOnly` 均缺省 **false**，必须为布尔值。
false 在强类型序列化中省略，保留既有 defaults/config parity；可直接在 config.json 的 emoji 对象中设为 true。
不修改 `prompts.rs` 或 `prompts_parity` 中“不得含空文本”的断言。

- **频率学习**：只有 learnFrequency 开启才从入站事件采集真实 face 段（数组或未转义 CQ），
  去重后在 `humanize_faces` 保存消息证据。查询同群 `messages` 的非自己消息，30 天窗口、7 天半衰期，
  加权样本量 <5 时为 0.08，否则 `clamp(rate × 0.8, 0, 0.35)`，再取 emoji.probability 上限。
  不改变私聊固定概率。原装饰 enabled、冷却仍生效；原有单次装饰抽签同时控制候选中的 symbols/faceIds。
  证据持久化并随 messages 删除清理。旧 messages 没有原始段证据，不从可能伪造的 `[QQface:...]`
  文本回填，因此新开关从启用后采集的样本冷启动。
- **只发表情**：只在 faceOnly 开启时向 articulation user JSON 加 `runtimeInstructions` 和
  `faceOnlyAllowed`；它是运行时片段，不进入生成产物。允许空文本和白名单内单 face、禁止 Unicode 混用。
  硬门槛是 open/other、原始 motivation ∈[1,3]、非 long，距上次自身消息至少 max(30 秒, emoji 冷却)。
  求助/难过尚无可靠语义分类器，因此只放行“哈哈”“确实”“同感”等纯附和白名单，未回答的人类消息
  全部必须在白名单内；未知/混合内容拒绝。此范围比设计的泛化场景识别更保守，不声称能识别所有情绪。
  `humanize_reply_state` 在发送前置位，成功正文清除，确定失败撤销，不确定或崩溃保留；
  即使聊天历史清理或引擎重启，同群也不能连续尝试第二次单 face。其它空正文素材保守视作无实质回答。
  单 face 仍经过原有配额、quiet、dry-run、freshness 与投递账本，真实 OneBot 只发送一个 face 段。
