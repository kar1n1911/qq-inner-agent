# JS 实现已归档(不再更新)

本分支冻结了 v1.0.0 时代的 Node.js 实现。**Rust 内核(`rust/`)是唯一在更新的实现**,位于 `main` 分支。

- 这里保留全部 `src/*.mjs`(含旧引擎、策略、提示词、发送、学习、记忆)与 `test/*.test.mjs`(旧 Node 测试);
- **不再接受改动**;如需调整提示词,请改 `main` 的 `src/prompts.mjs`,再运行 `node rust/tools/gen-prompts.mjs`。

归档时间:2026-10-05(§21 完成后)。
