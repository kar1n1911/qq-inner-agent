# P4 记忆、排序和表达

实现以 `src/memory.mjs`、`memory-ranking.mjs`、`expression.mjs` 和 `store.mjs` 为准，复用既有 schema、配置结构、JSON 行接口及 SQLite 连接。没有新增依赖或修改 `rust/` 外文件。未实现提示词分层、学习分诊、affect、recall 下钻，也未移植死代码 `parseLearning`/`handled()`。

## 调用与维护

- `LayeredMemory::new(&store)` / `ExpressionMemory::new(&store)` 是轻量视图；容量索引和维护时间由 Store 共享，所以 learn/reset 与入站不会使用不同计数。
- 启动、修改记忆配置或外部写入共享库后调用 `configure(now, settings)`；未来 P6 定时循环调用 `maintain(now, settings)`，它最多每小时调用一次相同的全量实现，包含没有新消息的 chat。当前仓库尚无 Rust engine 常驻循环，P4 不添加该阶段代码。
- 普通入站仅对一个/两个 subject 做定点 put，加常数次 HashMap/HashSet 操作。首次接入未初始化的 chat 读取一次人数索引。每 chat 累积 `shortLimit` 次 capture（默认 40）或距上次清理一小时触发全量清理；人数超过 `maxPeople` 立即触发。追加次数是容量上界，重复消息可能提前触发清理，不会漏掉超量追加。
- 批处理前最多暂存约两个 shortLimit 的群短期行；short/context 仍在读取时执行上限及有效期过滤。清理 SQL 批量裁剪 revision，重要性、更新时间、rowid、人数并列 subject 排序保持原语义。
- 当成员超过人数上限后又返回，逐次淘汰与“从未淘汰、最后一次 enforce”数学上并不等价（被淘汰者旧历史不能复活）。人数超限即时清理保留现行 JS 的逐次淘汰语义。测试分别证明：普通 500 条追加与一次最终 enforce 收敛；成员反复进出的序列与 JS 收敛。
- `Store::learn` 与 `reset_learning` 使用 BEGIN IMMEDIATE 和 RAII 回滚。事务失败同时使人数缓存失效。所有 JSON 列读出均为解析结构；learning_state 的 sources 与 JS 原始 TEXT 返回不同，交叉测试显式验证这项 P1b 契约。

## 文字与兼容边界

- 分词属性区间由 `node rust/tools/gen-memory-unicode.mjs` 生成（Node 26.9.0 / Unicode 17.0）。不用 `is_alphanumeric` 近似 L/N，也不把混合汉字词拆成仅汉字部分。token 是 `Vec<u16>`，保留 JS UTF-16 二元组里的孤立代理项，因此扩展汉字仍能逐 token 对齐。Node 或 Rust Unicode 数据升级需重跑交叉验证。
- 长度校验、记忆预算按 UTF-16 单元；decorate 按 Unicode 码点。复合 emoji 不保证只占一个码点。短期文本切在代理对中间时替换为 U+FFFD，与 JS 写入 SQLite 后的实际结果一致，已有交叉测试；输入 JSON 自身含孤立代理项时仍受 serde_json 的字符串表示限制。
- 同分时 Rust 使用字符串顺序，JS 使用环境相关的 `localeCompare`。系统生成的小写 UUID 顺序一致；人为混合大小写、重音或其他依赖 locale 的 ID（含表达 term 构成的 ID）可能顺序不同。测试明确记录 `a` / `A` 的差异，未声称 ICU 排序逐字等价。
- 现行 JS 对 jargon 要求 term 出现在**每条**证据中，example 仅需出现在**至少一条**；expression 的 example 才要求每条命中。遵循源码而非调研文档中的简化描述。
- 延迟清理是本阶段要求的行为差异：物理行数不会逐消息与 JS 相同。生产入站应沿用 `store.message` 的消息去重门控；手工重新 capture 已应被容量淘汰的旧 ID，不属于只追加新消息的收敛保证。

## 验证与性能复现

所有新增测试使用内存库或 `rust/target/` 下临时库，不访问真实 data/。JS oracle 直接加载生产源码；覆盖校验整批拒绝、隔离、短期去重、修订不延寿/不降级、revision 清理、长期/特征独立保留、容量和配置缩减、人数淘汰、学习 epoch/回滚、共库双向读写、表达证据/冷却、装饰白名单/码点预算、BM25/RRF/去重及 token 调用次数。

在 `rust/` 运行：

```sh
cargo build --release
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --release --test memory_parity measured_performance -- --ignored --nocapture
```

性能测试默认忽略，须用最后一条命令显式运行。三轮中位数，不计进程启动、建库和 200 人预置数据；SQLite 在内存中。入站测 400 条新消息（包含最后 configure）；排序测同 subject 的 80 个候选。JS 排序源码只注入 token 计数，算法不变；两边最终记忆行及排序结果均断言一致。分词计数为 JS 332,137 次、Rust 161 次（查询 1 次，每候选词频文本和重叠文本各 1 次）；贪心循环不分词，只更新缓存的最大重叠惩罚。时间是本机基准，不代表网络/模型调用主导的端到端提速。

本机最终实测（2026-10-02，rustc 1.98.1 / Node 26.9.0，release，三轮中位数）：

| 工作负载 | JS | Rust | 比值 |
| --- | ---: | ---: | ---: |
| 200 人、400 条新消息及最终批清理 | 3302.414 ms | 41.285 ms | 79.99× |
| 80 候选排序 | 457.516 ms | 1.975 ms | 231.59× |

最终验收：release 构建成功、Clippy `-D warnings` 零警告、常规 89 项测试通过（其中 P4 新增 15 项）、另行显式执行的 release 性能测试通过。
