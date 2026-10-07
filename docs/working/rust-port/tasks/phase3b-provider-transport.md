你是 qq-inner-agent 的 Rust 移植工程师。当前工作区是**隔离的 git worktree**,基线是最新的
`main`。改动不影响主分支。

## 前置(先读,不要重复实现)

`rust/src/provider.rs` **已经实现并测试了纯逻辑**,直接复用,不要重写:

- `endpoint(base, kind)`、`models_endpoint(base, kind)` —— 已与 JS 的 `listModels` 实际请求逐条比对
- `parse_object(text)` —— 已与 JS 逐字比对(含两种错误码 `invalid_json` / `invalid_json_object`)
- `classify_status(status)` → `StatusClass::{CheckConfig, Fatal, Transient, Ok}`
- `status_error(status)` → `http_<N>_check_provider_config` / `http_<N>` / `transient_http`
- `extract_text(kind, data)` —— 含 `output_truncated_increase_maxTokens` 与 `empty_model_response`

`rust/src/store.rs` 已有 `call_budget(now, max) -> Result<bool>` 与 `connection()`,用于小时限流。

## 必读

- `docs/working/rust-port/ARCHITECTURE.md`(不可变更契约)
- `docs/working/rust-port/SURVEY.md` —— provider 那一节
- `src/provider.mjs`(约 99 行,**逐行读完**)

## 任务:P3 收尾 —— HTTP 传输、重试退避与小时预算

### 1. `Provider`

在 `provider.rs` 里补齐传输层(纯逻辑保持不变):

- `Provider::new(config, key, store 句柄)`;`key` 为空时视为未配置
- `complete(system, user) -> Result<String, ProviderError>`:两种 API 格式各自组装
  body/headers(`openai` 用 `Authorization: Bearer`,`anthropic` 用 `x-api-key` +
  `anthropic-version`),解析响应,失败时走 `status_error`
- `json(system, payload) -> Result<Value, ProviderError>`:`complete` 之后套 `parse_object`
- `list_models() -> Result<Vec<String>, ProviderError>`:用 `models_endpoint`;
  注意 JS 对 deepseek 主机有特判,**照抄该特判**(以 `src/provider.mjs` 为准)

### 2. 阻塞与异步的边界(必须写清楚)

`ureq` 是**阻塞**的。请:

- 在 `tokio::task::spawn_blocking` 里执行请求;
- **明确注释并写进文档**:这与 JS 的 `AbortSignal` **语义不同** —— 取消只能"丢弃结果",
  请求本身会跑完。请说明这一差异对引擎意味着什么(取消不会立刻释放 API 额度);
- 超时用 `ureq` 的 timeout 配置,不要在异步侧用 `timeout()` 包阻塞调用(那不会真正中断)。

### 3. 退避、预算与错误

- 可重试的(`Transient`)按指数退避重试,退避上限与 JS 一致;
- `CheckConfig` 类:设置 `blocked_until = now + 300`,期间直接返回 `provider_backoff`;
- 每次**实际发出**的请求都要计入小时预算(用 `store.call_budget`),超限返回 `hourly_api_budget`;
- 请求**不发出**时(被退避拦截、无 key)不得消耗预算。

### 4. 依赖约束

- **不得引入新依赖**;TLS 走已有的 `ureq` + `rustls`(`ring`)。
- 若确有必要新增依赖,**先在部署主机上验证干净构建**,否则改用现有依赖实现。

## 硬性约束

1. **不得修改 `rust/` 以外的任何文件。**
2. 不改动已通过测试的纯逻辑函数签名(若有必须改的理由,先说明)。
3. 中文注释,重点标注:阻塞与取消的差异、重试与退避边界、预算计入时机、
   以及"哪些错误不重试"。

## 验收标准

1. `cargo build --release`;`cargo clippy --all-targets -- -D warnings`;`cargo test` 全过。
2. **用本地 mock HTTP 服务端测试**(`std::net::TcpListener` 即可,不要真的联网,不要加载新依赖):
   - 两种 API 格式的 body/headers 正确(含 anthropic 的 `anthropic-version`);
   - 401/403/400/404/422 → 对应错误码,且进入 300 秒退避;
   - 429 与 5xx → 重试,且重试次数与退避序列符合预期;
   - 超时、非法 JSON、非对象 JSON、空正文、`max_tokens`/`length` 截断;
   - 小时预算用尽 → `hourly_api_budget`,且**不再发出请求**;
   - 退避期间不发请求、不消耗预算。
3. 不访问真实网络、不读取真实 `secrets.json`。

## 交付

1. `git add -A && git commit`,英文 commit message。
2. 用**一段话**回报:实现了什么、测试怎么跑、阻塞式取消与 JS `AbortSignal` 的差异、
   以及任何不确定点。
