# P6-runtime 主循环与运行时

你是 qq-inner-agent 的 Rust 移植工程师。工作区是隔离的 git worktree。

## 定位:移植阶段(有 `src/main.mjs` 参照)

- **JS 是参照**,Rust 镜像它;验证走 JS 对照;
- 无法对齐处**必须在注释与测试里写明**。

## 范围(只做 `main.rs`)

P6a 已把 `Engine` 核心实现好,本阶段把它**接成可运行进程**:

1. **接线**:`Config`/`Settings` 加载 → `Store::open` → `Provider` → `OneBot` → `Engine`,
   用 `config.dataDir` 下的真实路径(不是临时目录);
2. **`status()` 原子写**:每 5 秒原子写 `data/status.json`,**字段与语义逐字对齐现有实现**
   (见 `docs/rust-port/ARCHITECTURE.md` 第 6.1 节的回归红线、`docs/rust-port/SURVEY.md` 第 5 节);
3. **revision 监视器**:每 1 秒比较 revision,检测到 `.settings-write` 时**暂停重载**,
   存在该标记时不得在写入中途读取;
4. **配置热重载语义**:停止引擎 → 保留仍被允许的 chat 状态(重置 `busy`/`lastThink`)
   → 整体重建 Engine/ActivityRhythm/GroupOrientation,但**共用同一个 store**;
5. **日志**:JSON 行格式与现有实现一致;
6. **信号处理**:干净停机(排空在途任务、关闭 socket/sqlite/ws)。

## ⛔ 不做

- 不做 P6c(人类化行为)、P6d(三层决策)、P6-media(素材选择)、P7(控制套接字);
- 不改提示词、不实现设计文档里未落地的设计。

## 依赖(已合并)

`engine`(P6a)、`config`、`settings`、`store`、`provider`、`onebot`、`activity`、`orientation`、`memory`、`ranking`、`expression`、`media`、`conversation`。

## 验收

1. `cargo build --release`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 全过。
2. `status.json` 字段与 JS `status()` 输出**交叉比对**(Node 不可用时跳过);
3. 单测:revision 变化被检测、`.settings-write` 存在时暂停、重载共用 store、
   信号停机干净、status 原子写(不产生半截文件);
4. 不写真实 `data/`、不发真实网络;时间/路径可注入。

## 交付

1. `git add -A && git commit`,英文。
2. 一段话回报:实现了什么、status 字段如何与 JS 对齐、与 JS 的已知差异。
