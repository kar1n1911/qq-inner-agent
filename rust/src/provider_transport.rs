//! 阻塞传输仅在 blocking 池执行；退避等待留在异步侧，可随 future 丢弃而取消。
use super::*;
use crate::{config, store::Store};
use serde_json::json;
use std::{
    io::Read,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

type AttemptResult = Result<Value, (ProviderError, Option<String>)>;

#[derive(Clone)]
pub struct Provider {
    config: config::Provider,
    key: String,
    store: Arc<Mutex<Store>>,
    blocked_until: Arc<Mutex<f64>>,
    now: Arc<dyn Fn() -> f64 + Send + Sync>,
    #[cfg(test)]
    sleep: Option<Arc<dyn Fn(Duration) + Send + Sync>>,
}

impl Provider {
    /// config 应来自已校验的配置；共享 Store 的锁只覆盖预算事务，不覆盖网络请求。
    pub fn new(config: config::Provider, key: impl Into<String>, store: Arc<Mutex<Store>>) -> Self {
        Self {
            config,
            key: key.into(),
            store,
            blocked_until: Arc::new(Mutex::new(0.)),
            now: Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64()
            }),
            #[cfg(test)]
            sleep: None,
        }
    }

    fn kind(&self) -> ProviderKind {
        if self.config.kind == "anthropic" {
            ProviderKind::Anthropic
        } else {
            ProviderKind::OpenAi
        }
    }

    fn model_request(&self) -> (String, bool) {
        // 使用 ureq 所用的 URL 解析器，按 hostname 精确匹配（不按字符串前缀匹配）。
        let deepseek = ureq::get(&self.config.base_url)
            .request_url()
            .ok()
            .map(|url| url.host().to_owned())
            .is_some_and(|host| host == "api.deepseek.com");
        (
            if deepseek {
                "https://api.deepseek.com/models".into()
            } else {
                models_endpoint(&self.config.base_url, self.kind())
            },
            deepseek,
        )
    }

    pub async fn complete(&self, system: &str, user: &str) -> Result<String, ProviderError> {
        let c = &self.config;
        let mut body = if self.kind() == ProviderKind::Anthropic {
            json!({"model": c.model.text, "max_tokens": c.max_tokens, "system": system,
                "messages": [{"role":"user", "content":user}]})
        } else {
            json!({"model":c.model.text, (c.token_parameter.clone()):c.max_tokens,
                "messages":[{"role":"system", "content":system},{"role":"user", "content":user}]})
        };
        if c.thinking.as_deref() == Some("disabled") {
            body["thinking"] = json!({"type":"disabled"});
        }
        let body = body.to_string();
        for attempt in 0..=c.retries as u32 {
            let this = self.clone();
            let body = body.clone();
            // 与 JS AbortSignal 不同：丢弃 future 只能丢弃结果，已启动的阻塞请求会跑完。
            // 引擎取消后仍须丢弃旧版本结果；不能退还预算，也不能假定 API 额度立即释放。
            let result = tokio::task::spawn_blocking(move || this.request(Some(&body)))
                .await
                .map_err(|_| ProviderError::ProviderUnavailable)?;
            match result {
                Ok(data) => return extract_text(self.kind(), &data),
                Err((ProviderError::TransientHttp, retry_after)) => {
                    if attempt == c.retries as u32 {
                        *self
                            .blocked_until
                            .lock()
                            .map_err(|_| ProviderError::ProviderUnavailable)? = (self.now)() + 60.;
                        return Err(ProviderError::ProviderUnavailable);
                    }
                    let delay = retry_delay(attempt, retry_after.as_deref(), (self.now)());
                    #[cfg(test)]
                    if let Some(sleep) = &self.sleep {
                        sleep(delay);
                        continue;
                    }
                    tokio::time::sleep(delay).await;
                }
                // 配置、预算、JSON、空正文、截断和非 429 的其他 4xx 均不重试。
                Err((error, _)) => return Err(error),
            }
        }
        Err(ProviderError::ProviderUnavailable)
    }

    pub async fn json(&self, system: &str, payload: &Value) -> Result<Value, ProviderError> {
        parse_object(&self.complete(system, &payload.to_string()).await?)
    }

    pub async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        let this = self.clone();
        // 列表请求与 JS 一样不重试；但按移植契约同样受共享小时预算及封锁约束。
        let data = tokio::task::spawn_blocking(move || this.request(None))
            .await
            .map_err(|_| ProviderError::ProviderUnavailable)?
            .map_err(|(error, _)| error)?;
        let rows = data
            .get("data")
            .and_then(Value::as_array)
            .ok_or(ProviderError::InvalidModelList)?;
        let mut models = Vec::new();
        for id in rows
            .iter()
            .filter_map(|row| row.get("id").and_then(Value::as_str))
        {
            if id.encode_utf16().count() <= 200 && !models.iter().any(|s| s == id) {
                models.push(id.to_owned());
                if models.len() == 500 {
                    break;
                }
            }
        }
        // JS 默认按 UTF-16 码元排序，而非 Unicode 标量值排序。
        models.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        Ok(models)
    }

    fn request(&self, body: Option<&str>) -> AttemptResult {
        self.request_inner(body).map_err(|error| (error, None))?
    }

    fn request_inner(&self, body: Option<&str>) -> Result<AttemptResult, ProviderError> {
        let c = &self.config;
        let models = body.is_none();
        let (url, deepseek) = if models {
            self.model_request()
        } else {
            (endpoint(&c.base_url, self.kind()), false)
        };
        // 每次新建 agent，避免连接池的透明重试绕过预算；禁止自动跳转泄露凭据。
        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            .try_proxy_from_env(false)
            .timeout(Duration::from_secs_f64(if models {
                15.
            } else {
                c.timeout_seconds
            }))
            .build();
        let mut request = agent.request(if models { "GET" } else { "POST" }, &url);
        if self.kind() == ProviderKind::Anthropic && !deepseek {
            request = request.set("anthropic-version", "2023-06-01");
            if c.anthropic_auth == "bearer" {
                request = request.set("Authorization", &format!("Bearer {}", self.key));
            } else {
                request = request.set("x-api-key", &self.key);
            }
            if c.workspace_id.is_truthy {
                request = request.set("anthropic-workspace-id", &c.workspace_id.text);
            }
        } else {
            request = request.set("Authorization", &format!("Bearer {}", self.key));
        }
        if body.is_some() {
            request = request.set("Content-Type", "application/json");
        }
        {
            let blocked = self
                .blocked_until
                .lock()
                .map_err(|_| ProviderError::ProviderUnavailable)?;
            if (self.now)() < *blocked {
                return Err(ProviderError::Backoff);
            }
            if self.key.is_empty() {
                return Err(ProviderError::SaveApiKeyFirst);
            }
            // 仅在 blocking 任务真正准备发送时计费；无 key、封锁和预算拒绝都不发请求。
            // 重试逐次计费，失败的传输也占一次；数据库失败时禁止继续发送。
            if !self
                .store
                .lock()
                .map_err(|_| ProviderError::ProviderUnavailable)?
                .call_budget((self.now)(), c.requests_per_hour)
                .map_err(|_| ProviderError::ProviderUnavailable)?
            {
                return Err(ProviderError::HourlyBudget);
            }
        }
        let response = match body {
            Some(body) => request.send_string(body),
            None => request.call(),
        };
        let response = match response {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(_) => {
                return Err(if models {
                    ProviderError::ProviderUnavailable
                } else {
                    ProviderError::TransientHttp
                })
            }
        };
        let status = response.status();
        if !(200..300).contains(&status) {
            if models {
                return Err(ProviderError::ModelsHttp(format!("models_http_{status}")));
            }
            if classify_status(status) == StatusClass::CheckConfig {
                *self
                    .blocked_until
                    .lock()
                    .map_err(|_| ProviderError::ProviderUnavailable)? = (self.now)() + 300.;
            }
            // fetch 的 redirect:error 属于可重试传输失败；纯逻辑分类函数保持原契约。
            let error = if (300..400).contains(&status) {
                ProviderError::TransientHttp
            } else {
                status_error(status)
            };
            return Ok(Err((
                error,
                response.header("retry-after").map(str::to_owned),
            )));
        }
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(4_000_001)
            .read_to_end(&mut bytes)
            .map_err(|_| {
                if models {
                    ProviderError::ProviderUnavailable
                } else {
                    ProviderError::TransientHttp
                }
            })?;
        let text = String::from_utf8_lossy(&bytes);
        if text.encode_utf16().count() > 1_000_000 {
            return Err(ProviderError::ResponseTooLarge);
        }
        Ok(Ok(
            serde_json::from_str(&text).map_err(|_| ProviderError::InvalidProviderResponse)?
        ))
    }
}

fn retry_delay(attempt: u32, header: Option<&str>, now: f64) -> Duration {
    let base = 2_f64.powi(attempt.min(5) as i32).min(30.);
    let parsed = header.and_then(|s| {
        s.trim()
            .parse::<f64>()
            .ok()
            .filter(|n| *n != 0.)
            .or_else(|| {
                chrono::DateTime::parse_from_rfc2822(s)
                    .ok()
                    .map(|d| d.timestamp() as f64 - now)
            })
    });
    Duration::from_secs_f64(
        parsed
            .filter(|n| n.is_finite())
            .map_or(base, |n| base.max(n.min(60.))),
    )
}

#[cfg(test)]
#[path = "provider_transport_tests.rs"]
mod tests;
