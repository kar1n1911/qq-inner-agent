# 原生测试金标准

这些 JSON 是迁移时实际运行当前 JS oracle 捕获的完整输出，不由 Rust 被测实现生成。
首次捕获的源版本为 `0cd9190`；`capture.rs` 是一次性记录辅助，正式测试不引用它。
捕获时在原 oracle 反序列化输出后调用 `capture::record(input, output)`，运行对应测试，
再将同一套测试的记录合并为一个数组。`case` 保留原测试名及该测试内的调用序号。

正式测试通过 `include_str!` 将 JSON 编译入测试。多数套件使用 `mod.rs` 逐值匹配输入，
policy 使用序列化 payload 为键的 map，明确断言四个键（含 active_at）全部存在；
找不到记录即失败。金标准更新应单独审查，不能自动用当前 Rust 输出覆写。

- config：66 组，包括默认值、加载/环境覆盖、非法值和原始字节 revision。
  只有随机临时根路径用 `<ROOT>` 占位，读取时恢复；其余值保持捕获结果。
- policy：4 组完整矩阵（时区、本地分钟、安静时段、活跃时段、词项、相似度）。
- prompts：1 组，所有常量、规则组合、语言变体逐字比较。
- sending：1 组、1920 个参数组合，概率和所有因子按 IEEE-754 位精确比较。
- activity：1 组、183 个输入组合，保留原来的 exp 绝对误差规则。
- provider：3 组，端点、模型列表端点、JSON 解析及错误码。
- store：2 组，完整 schema / 索引 / 原始数据行，以及写入后的读取结果。
  通过捕获的 JS DDL 和原始行重建旧库，验证 Rust 打开、修改及重新打开。
- memory：13 组，包括默认执行的 benchmark 正确性、排名、旧 SQLite JSON 行和 revision。
  JS/Rust 随机 UUID 不参与值比较；JSON 文本字段必须仍是合法字符串，解析后逐值比较。
  JS 的重复分词次数与 Rust 缓存实现不同，Rust 的 161 次精确断言仍保留；计时不是金标准。
- phase5：10 组，规范化、活动持久化、阈值及 orientation 多次调用，所有比较无条件执行。
- engine：1 组、146 个脚本场景，完整阶段 trace、决策、发送和落库结果。

共新增 102 组 oracle 调用输出；另保留 `revision_golden.rs` 的六组已有哈希常量。
原始 `.mjs` 脚本移至 `rust/docs/p11-oracles/`，测试无引用。

浮点规则沿用原测试：纯四则运算逐位相等；含 exp 的 activity 允许绝对误差 1e-12。
其它模块维持各自原有的数值容忍、精确结构比较及不变量断言。

## 零跳过验收

2026-10-05 在 macOS / stable-aarch64-apple-darwin 上已验证：

```sh
# 仓库根目录；没有匹配项（rg 退出码 1）才符合验收。
rg -n 'Command::new\("node"\)|node_available\(|#\[ignore|fixtures/[^" ]*\.mjs' rust/tests -g '*.rs'

# 用实际 Rust toolchain 的 bin 目录，排除 Homebrew、.runtime 等 Node 所在目录。
# 其他主机需选择自己的 toolchain 路径，并保持 command -v node 的失败检查。
env PATH='/Users/coleanderson/.rustup/toolchains/stable-aarch64-apple-darwin/bin:/usr/bin:/bin:/usr/sbin:/sbin' \
  /bin/sh -c 'if command -v node; then exit 1; fi; cargo test --manifest-path rust/Cargo.toml'
```

上述源码检查无匹配；`command -v node` 找不到命令；完整测试在 25 个 harness 中合计
**176 passed、0 failed、0 ignored、0 measured、0 filtered out**，输出无 `SKIP` / `skipped`。
常规 PATH 下全量结果相同。迁移前总数为 176，其中 benchmark 默认被忽略，
迁移后总数不变、176 个全部执行。连接启动超时已由 4 秒增至 15 秒，原因见 `onebot_mock.rs`。

同次验收还通过 `cargo build --release` 和 `cargo clippy --all-targets -- -D warnings`。
