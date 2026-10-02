# Provider 传输契约

`provider::Provider::new(config::Provider, key, Arc<Mutex<Store>>)` 接收已校验配置、显式传入的 key 与共享存储句柄；不读取配置文件或 secrets。`complete`、`json`、`list_models` 都是异步方法，克隆 Provider 共享退避状态与数据库。已有端点、解析和状态分类纯函数未改动。

HTTP 与 SQLite 预算事务在 `tokio::task::spawn_blocking` 中运行，数据库锁不跨网络请求。ureq 使用已有 rustls/ring 依赖，超时配置在 ureq 内部：补全用 `timeoutSeconds`，模型列表固定 15 秒。不在异步侧用 timeout 包裹阻塞请求。禁用自动跳转与环境代理，每次尝试新建 agent，防止池化连接的透明重试绕过计费。

**取消与 JS AbortSignal 不等价**：丢弃异步 future 或 abort 调用任务可以停止退避等待和后续重试，但已启动的 blocking 任务无法中断；它会运行到响应或 ureq 超时。若 blocking 任务尚未发送，它也可能继续执行到发送。取消不会立即释放 API 额度，已准入的预算不退还。引擎必须在取消/配置热重载后检查任务版本，丢弃旧结果，不能因为取消就假定服务端已停止生成。正在运行的 blocking 任务也可能延迟运行时关闭。收到配置错误时，即使调用方已取消，共享的 300 秒封锁仍会设置。

每次实际尝试发送之前，在 blocking 任务内先检查封锁和空 key，再调用 `Store::call_budget`。重试和模型列表请求都计入滚动一小时预算；连接失败/超时同样计一次。无 key 返回 `save_api_key_first`，封锁返回 `provider_backoff`，超限返回 `hourly_api_budget`，这些拒绝不发送请求、不新增预算。数据库错误也禁止发送。多个已通过准入的在途请求不会被后来的封锁撤销。

补全的最大尝试次数为 `floor(retries) + 1`（与 JS 非整数 retries 的循环边界一致）。429、5xx、网络和超时错误可重试，等待 1、2、4、8、16、30、30…秒。`Retry-After` 支持数值秒及标准 RFC 2822 HTTP 日期，以当前时间计算；等待取指数退避和 Retry-After 的较大者，后者上限 60 秒。耗尽后返回 `provider_unavailable` 并封锁 60 秒。401/403/400/404/422 返回原有 `http_<N>_check_provider_config` 并封锁 300 秒。其他 4xx、响应过大、非法响应 JSON、空模型正文、截断和 `json` 的对象解析错误不重试。跳转不跟随，按 fetch 的 redirect:error 作为传输失败重试。响应大小按 JS UTF-16 长度限制为 1,000,000，并设置字节读取上限。

模型列表保留 JS 的独立行为：不重试，HTTP 错误为 `models_http_<N>`，列表结构错误为 `invalid_model_list`；它不触发补全的配置封锁。按任务要求增加共享预算与既有封锁检查（JS 独立 listModels 没有这两个检查）。DeepSeek hostname 精确匹配 `api.deepseek.com` 时固定请求 `https://api.deepseek.com/models`，只用 Bearer，不发送 Anthropic 版本/workspace 头。其他主机使用已有 `models_endpoint` 和配置中的鉴权方式。列表先过滤、去重、取前 500，再按 UTF-16 顺序排序。

测试使用回环 `TcpListener` 与内存 SQLite，不访问外部网络、不读取真实 secrets。可注入的私有时钟/睡眠测试钩子用于验证完整退避序列而不实际等待数分钟；超时与取消测试使用真实短延迟。交叉验证类测试需要 `node` 在 PATH 上（见 `rust/README.md`）。运行：

```sh
cargo build --release
cargo clippy --all-targets -- -D warnings
cargo test
```
