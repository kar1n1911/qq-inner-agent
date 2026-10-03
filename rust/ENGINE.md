# P6a Engine 核心

`engine::Engine::new` 接收已校验 `Config`、共享 `Arc<Mutex<Store>>`、
`OrientationProvider`（真实 `Provider` 已实现）、`EngineTransport`（真实 `OneBot` 已实现）
与可注入 `Options`。不加载配置文件、不访问真实 data、不启动网络连接。
`ingest` 只入库和标记；调用者驱动 `tick`，`wait_idle` 等待本轮任务，`stop` 取消模型等待并等待已提交投递完成。
日志回调在同步段调用，不得重入 Engine 或获取同一 Store 锁。

`cycle` 中的数字注释对应 SURVEY §2.2 的 1–48 步。复用 policy、sending、orientation、
activity、memory、expression、store（含 learning）、prompts、provider 和 onebot；
没有引入依赖，没有改动 main、提示词、数据库 schema 或素材路径。

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

- P6a 明确不做长度分档接线，故没有调用 `pick_length_target`、没有传 `lengthTarget` 或记录其日志键。
  当前 JS 已有此调用，golden 明确不比较该字段且 mock 正文不依赖它；测试断言 Rust 未接线。
  提示词保持原样，真实模型因此可能产生不同长度，留给 P6c。也未实现只发表情、多气泡/打字延迟、
  按群表情频率、三层决策、零模型初筛或素材选择。
- 候选文本复用既有 `policy::clip_chars`，按 Unicode 标量字符截断 300；JS 按 UTF-16 码元截断。
  非 BMP 边界有显式测试，普通中文/ASCII golden 对齐；不生成孤立代理项。
- Rust 取消丢弃 future，不能停止阻塞传输；引擎模型等待取消错误码为 `aborted`，
  JS 底层 AbortSignal 的错误码可能由 fetch/Node 版本决定。取消后的预算与持久化不变量按上述测试。
- 随机源分别注入发送、候选选择、表达与活动；JS select 原本用全局 Math.random，oracle 单独固定。
  golden 验证确定输入下的调度/决策，不声称真实模型输出或时区数据库版本完全相同。
