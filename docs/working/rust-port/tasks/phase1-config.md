你是 qq-inner-agent 的 Rust 移植工程师。当前工作区是一个**隔离的 git worktree**,分支
`pebrel/rust-config`,基线提交 `172a35d`。你在这里的任何改动都不会影响主分支。

## 背景

这个仓库原本是 Node.js 实现的 QQ 对话智能体。我们要把**性能关键路径**移植到 Rust,
Node 侧只保留仪表盘与前端。Rust crate 位于 `rust/`,目前只有 Phase 0 骨架
(`Cargo.toml` + `src/main.rs` 里一个 clap selftest)。

## 必读(动手前请全部读完)

- `docs/working/rust-port/ARCHITECTURE.md` —— 架构、进程边界、控制协议、不可变更的契约
- `docs/working/rust-port/SURVEY.md` —— 19 个模块的逐行勘察报告;其中**第 4 节是完整的配置 schema**
  (全部键、默认值、区间、校验规则)。注意:`config.example.json` 已过期,不要当 schema 用。
- `src/settings.mjs` —— 要移植的 `revision` / `atomicJson`
- `src/config.mjs` —— 要移植的 `defaults` / `merge` / `validate` / `loadConfig` / `readiness`

## 任务:Phase 1 —— 配置层

在 `rust/src/` 下实现下面两个模块,并在 `src/main.rs` 中接线。

### 1. `settings.rs`

- `pub fn revision(root: &Path) -> Result<String>`
  必须与 `settings.mjs` 的 `revision` **逐字节等价**:sha256( config.json 内容 + 单个 0x00 字节
  + secrets.json 内容 ),输出小写十六进制。**缺失文件的处理必须与 JS 完全一致**
  (请读 `settings.mjs` 确认是当作空串还是报错,不要臆测)。
- `pub fn read_json(path: &Path) -> Result<Option<Value>>`(文件不存在返回 None)
- `pub fn atomic_json(path: &Path, value: &Value) -> Result<()>`(写临时文件 + rename,权限 0600)

### 2. `config.rs`

- `pub fn defaults() -> Value` —— 完整复刻 `config.mjs` 的 `defaults`(含全部嵌套结构、数组默认值)
- `pub fn merge(base: &Value, extra: &Value) -> Value` —— 深合并;数组整体替换(不逐元素合并);
  跳过 `__proto__` / `constructor` / `prototype`;语义与 JS 版一致
- `pub fn validate(c: &Value) -> Result<(), ConfigError>` —— 复刻 `config.mjs` 的**全部**校验规则,
  包括:数值区间、枚举、正则、跨字段约束(`rhythm.centerProbability ≤ edgeProbability`、
  schedule 的 `activeStart ≠ inactiveStart`、各 Min ≤ Max)、以及两条默认 persona 的
  **全等比较→替换**迁移逻辑
- `pub struct Config { ... }` —— 供运行时使用的**强类型视图**(后续热路径要快),
  由 `merge` + `validate` 之后的 `Value` 构造。字段用 Rust 命名风格,但与配置键一一对应;
  请为每个字段写清楚它对应的键名。
- `pub fn load_config(root: &Path) -> Result<Loaded>` —— 复刻 `loadConfig`:
  - `config.json` 缺失时按 `{}` 处理
  - apiKey 优先级:`LLM_API_KEY` > (host == `api.deepseek.com` ? `DEEPSEEK_API_KEY`
    : kind == openai ? `OPENAI_API_KEY` : `ANTHROPIC_API_KEY`) > `secrets.apiKey`
  - onebotToken 优先级:`ONEBOT_TOKEN` > `secrets.onebotToken`
  - dataDir = root 拼接 `storage.directory` 后取绝对路径
  - 返回强类型 `Config` 与合并后的原始 `Value`(两者都要,后续模块会用到 Value)
- `pub fn readiness(&Config) -> Vec<String>` —— 返回缺失项,字面量必须与 JS 一致
  (`"API key"`、`"selected chat IDs"`)

### 3. `main.rs` 接线

- 新增子命令 `config`:打印归一化后的配置摘要,**必须对 apiKey / onebotToken 脱敏**
- 保留现有 `selftest` 子命令
- 不要把 `main.rs` 写成一个巨型文件;配置相关的类型都放在 `config.rs`

## 硬性约束

1. **不得引入任何新依赖。** 尤其禁止引入需要 cmake / make / pkg-config / OpenSSL / 系统 SQLite 的
   crate —— 部署主机上这些全都没有,且没有免密 sudo,无法安装。
2. **不得修改 `rust/` 以外的任何文件**(`docs/` 下可以加说明)。
3. 代码中要有**中文注释**,解释每一段与 JS 实现的对应关系;纯翻译式的注释没意义,
   重点标注**容易出错的地方**(边界、缺失值、类型转换、正则差异)。
4. Rust 侧的正则、Unicode 处理与 JS 存在差异的地方,要用注释显式标出并说明是否等价。

## 验收标准(必须全部满足)

1. `cargo build --release` 成功。
2. `cargo clippy -- -D warnings` 无警告。
3. `cargo test` 全部通过。测试至少覆盖:
   - `defaults` / `merge` / `validate` 的单元测试:数值区间边界、非法枚举、
     跨字段约束、persona 迁移、`merge` 的数组替换与原型污染键跳过
   - **交叉验证测试(重点)**:构造若干 fixture 目录,每个目录放一份 `config.json` 与
     `secrets.json`;测试内调用 `node` 运行 `src/settings.mjs` 的 `revision` 与
     `src/config.mjs` 的 `loadConfig`,与 Rust 侧结果逐一比较 **完全相等**。
     `node` 不可用时该测试要 **skip 而不是 fail**(可用 `#[ignore]` 或运行时探测并返回)。
4. 不产生真实网络调用、不读写仓库外的真实 `data/`。

## 交付方式

1. 完成后 `git add -A && git commit`。commit message 用英文,单行标题 + 简要正文,
   说明这是 Phase 1 配置层移植。
2. 最后用**一段话**回报:实现了什么、测试如何运行、与 JS 的**已知差异或不确定点**(如有)。
