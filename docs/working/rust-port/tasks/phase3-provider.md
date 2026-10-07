你是 qq-inner-agent 的 Rust 移植工程师。当前工作区是一个**隔离的 git worktree**,基线是
Phase 2 之后的 `main`。改动不会影响主分支。

## 前置

Phase 1(配置层 `settings.rs` / `config.rs`)与 Phase 2(OneBot 传输 `onebot.rs`)已完成,
请先读它们,复用既有的错误类型、日志风格与测试组织方式。

## 必读

- `docs/working/rust-port/ARCHITECTURE.md` —— 架构与不可变更的契约
- `docs/working/rust-port/SURVEY.md` —— **第 7 节是 provider 层的完整规格**(逐条,含全部错误码与重试规则)
- `src/provider.mjs`(99 行,务请逐行读完)—— 唯一真源

## 任务:Phase 3 —— provider 适配层

在 `rust/src/provider.rs` 中实现,并接入 `main.rs`(新增子命令 `test-model`)。

### 端点构造

`endpoint(base, kind)`:先去尾斜杠;若已以目标后缀结尾则原样返回;否则拼接。
- `kind == anthropic` → 后缀 `/messages`,且**当且仅当 base 不以 `/v1` 结尾时**插入 `/v1`
- 否则 → 后缀 `/chat/completions`

`list_models`:若 base 的 host 是 `api.deepseek.com` → 固定 `https://api.deepseek.com/models`;
否则把 endpoint 结果里的 `/(chat\/completions|messages)$/` 换成 `/models`。去重、id 长度 ≤200、
取前 500 条并排序;超时 15s。

### 请求构造

- OpenAI:头 `Authorization: Bearer <key>`;体 `{model, <tokenParameter>: maxTokens, messages:[
  {role:"system",content:system},{role:"user",content:user}]}`
- Anthropic:头按 `anthropicAuth` 二选一(`x-api-key` 或 `Authorization: Bearer`),
  另加 `anthropic-version: 2023-06-01`,若 `workspaceId` 非空再加 `anthropic-workspace-id`;
  体 `{model, max_tokens: maxTokens, system, messages:[{role:"user",content:user}]}`
- 两种格式在 `thinking === "disabled"` 时都要加 `thinking: {type:"disabled"}`
- `redirect` 策略为拒绝重定向;请求超时为 `timeoutSeconds`,并且必须能与调用方的取消信号组合
  (**两者中先到者生效**)

### 重试与限流

- 每次尝试前检查 `blockedUntil`:**未到点直接返回 `provider_backoff`,不发请求**
- 每次尝试前检查**滚动一小时**的调用预算;预算不过则返回 `hourly_api_budget`,且**不计数**;
  通过后才 `calls += 1`
- `retryDelay = min(30, 2^attempt)`;若响应带 `Retry-After`,则取 `max(retryDelay, min(60, 解析值))`
- HTTP 分类(务必逐条对齐):
  - `401/403/400/404/422` → `blockedUntil = now + 300`,错误码 `http_<code>_check_provider_config`,
    **不重试**
  - 其余非 429 且 <500 → `http_<code>`,**不重试**
  - `429` 与 `>=500` → `transient_http`,**重试**
  - 重试用尽 → `blockedUntil = now + 60`,错误码 `provider_unavailable`
- 调用方取消时立即返回取消,而不是被计入重试

### 响应解析

- 正文长度 > 1_000_000 → `response_too_large`;JSON 解析失败 → `invalid_provider_response`
- Anthropic:`stop_reason == "max_tokens"` → `output_truncated_increase_maxTokens`;
  正文 = `content` 中所有 `type == "text"` 的 `text` 用 `\n` 连接
- OpenAI:`choices[0].finish_reason == "length"` → 同上;正文 = `choices[0].message.content`
- 正文非字符串或纯空白 → `empty_model_response`

### `parseObject` 与 `json()`

`parse_object(text)`:先去首尾空白 → 剥掉开头的 ` ```json ` / ` ``` ` 围栏与结尾围栏 →
`JSON.parse`;解析失败 `invalid_json`;结果非对象(数组也算非对象)则 `invalid_json_object`。
`json(system, payload)` = `parse_object(complete(system, serde_json::to_string(payload)))`。

## 硬性约束

1. **不得引入新依赖。** HTTPS 必须走现有的 `ureq`(`rustls` + `ring`,已在部署主机验证可编译);
   不要引入 `reqwest`/`hyper`/`native-tls`/`openssl`。
2. **不得修改 `rust/` 以外的任何文件。**
3. 中文注释,重点标注易错处:`/v1` 插入条件、`Retry-After` 的两种格式(秒数或 HTTP 日期)、
   预算"不通过则不计数"的次序、取消与超时的组合、Anthropic 正文多段拼接。
4. 错误码字符串必须与 JS **逐字一致**(它们是与前端/日志的契约)。

## 验收标准

1. `cargo build --release` 成功;`cargo clippy -- -D warnings` 无警告;`cargo test` 全过。
2. **必须用本地 mock HTTP 服务端测试**(不得访问真实 API),覆盖:
   - `endpoint()` 的 4 类输入(含 `/v1` 结尾与完整端点结尾)
   - 两种 API 格式的请求头与请求体**逐字段**断言(含 `thinking` 与 workspace 头)
   - 重试次数、退避序列、`Retry-After`(秒数与日期两种)抬高延迟
   - 六类错误码映射(401/403/400/404/422 → 300s 且不重试;429/5xx → 重试;耗尽 → unavailable)
   - 预算:未通过时不发请求且 `calls` 不增;通过后 `calls` 递增
   - 响应解析:两种格式的成功路径、`max_tokens`/`length` 截断、超大响应、非法 JSON、空正文
   - `parse_object` 的围栏剥离与"非对象"拒绝
3. `test-model` 子命令:用配置里的 key 发一次最小请求并打印结果(无 key 时给出明确错误)。

## 交付

1. `git add -A && git commit`,英文 commit message。
2. 用**一段话**回报:实现了什么、测试怎么跑、与 JS 的已知差异或不确定点。
