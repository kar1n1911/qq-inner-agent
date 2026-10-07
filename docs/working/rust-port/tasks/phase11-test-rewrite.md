# P11 测试重写(迁移后收尾)

你是 qq-inner-agent 的 Rust 移植工程师。工作区是隔离的 git worktree。

## 定位

P10 实况切换完成后,`*_parity.rs` 这批交叉验证测试完成了历史使命(它们是迁移脚手架)。
本阶段把它们**重写成 Rust 原生测试**,并**移除 node 依赖**。

## 现状

- `rust/tests/` 下有 `config_parity` / `policy_parity` / `prompts_parity` / `sending_parity` /
  `activity_parity` / `provider_parity` / `store_parity` / `memory_parity` / `engine_parity` / `phase5_parity` /
  `revision_golden` 等,依赖真实 Node(与 `fixtures/*oracle.mjs`)算出期望值;
- 这些测试**在 node 不在 PATH 时会静默跳过**(假绿陷阱,已记录在 `rust/README.md`)。

## 任务

1. 把每个 parity 测试的**期望值**从"运行 node oracle 现算"改为**固化在 Rust 测试里的金标准**
   (用 oracle 现在算出的值,经 `serde_json` 精确转写,浮点按既有容忍规则);
2. 删除对 `Command::new("node")` 的依赖,改为纯 Rust;
3. 删除 `fixtures/*.mjs`(或移到文档存档,不再被测试引用);
4. **完成判据**:把 node 从 PATH 移除后,`cargo test` **一个都不跳过、全部原生通过** —— 这个要显式验证。

## 顺带修一处已知 flaky

`tests/onebot_mock.rs` 的 `running()` 用 `next(rx)` 等 "connected",超时 4 秒;
并行全量跑时负载高会偶发超时(非端口冲突,端口已是 `:0` 随机)。
把该超时从 4s 放宽到 15s(或改轮询),并在注释里说明原因。

## 硬性约束

1. 不引入新依赖;不改 `rust/` 以外文件;
2. 重写后的测试**断言强度不低于原 parity 测试**(不变量仍精确,启发式仍用区间/方向);
3. 中文注释,重点标注:哪些期望值是"固化金标准"、浮点容忍、以及"node 移除后零跳过"的验证方式。

## 验收

1. `cargo build --release`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 全过;
2. **`PATH` 去掉 node 后再 `cargo test`,无任何 `skipped`、无 node 调用**(grep 测试源码确认没有 `Command::new("node")`);
3. 测试总数与重写前一致或更多。

## 交付

1. `git add -A && git commit`,英文。
2. 一段话回报:固化了多少金标准、node 移除后如何验证零跳过、onebot_mock 超时是否已修。
