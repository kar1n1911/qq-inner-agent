use super::*;
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

struct Mock {
    url: String,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Mock {
    fn new(responses: Vec<(u16, String, String, Duration)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = thread::spawn(move || {
            let mut responses = responses.into_iter();
            while !stopped.load(Ordering::SeqCst) {
                let (mut socket, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut headers = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    headers.push_str(&line);
                }
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(|s| s.parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let mut bytes = vec![0; length];
                reader.read_exact(&mut bytes).unwrap();
                captured.lock().unwrap().push((
                    headers,
                    serde_json::from_slice(&bytes).unwrap_or(Value::Null),
                ));
                let (status, body, extra, delay) = responses.next().expect("unexpected request");
                thread::sleep(delay);
                let _ = write!(socket, "HTTP/1.1 {status} Mock\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}", body.len());
            }
        });
        Self {
            url,
            requests,
            stop,
            worker: Some(worker),
        }
    }
    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let result = self.worker.take().unwrap().join();
        if !thread::panicking() {
            result.unwrap();
        }
    }
}
fn reply(status: u16, body: impl ToString) -> (u16, String, String, Duration) {
    (status, body.to_string(), String::new(), Duration::ZERO)
}
fn success() -> Value {
    json!({"choices":[{"message":{"content":"{\"ok\":true}"}}]})
}
fn provider(mock: &Mock) -> Provider {
    let config = serde_json::from_value(json!({"kind":"openai","baseUrl":mock.url,"model":"test-model",
        "maxTokens":123,"tokenParameter":"max_completion_tokens","timeoutSeconds":1,
        "retries":2,"requestsPerHour":100,"anthropicAuth":"x-api-key","workspaceId":"space","thinking":"disabled"})).unwrap();
    let mut p = Provider::new(
        config,
        "mock-key",
        Arc::new(Mutex::new(Store::in_memory().unwrap())),
    );
    p.now = Arc::new(|| 1000.);
    p.sleep = Some(Arc::new(|_| {}));
    p
}
fn budget(p: &Provider) -> usize {
    p.store
        .lock()
        .unwrap()
        .connection()
        .query_row("SELECT count(*) FROM calls", [], |r| r.get(0))
        .unwrap()
}

#[tokio::test]
async fn bodies_headers_and_json() {
    for (kind, auth) in [
        ("openai", "x-api-key"),
        ("anthropic", "x-api-key"),
        ("anthropic", "bearer"),
    ] {
        let response = if kind == "anthropic" {
            json!({"content":[{"type":"text","text":"{\"ok\":true}"}]})
        } else {
            success()
        };
        let mock = Mock::new(vec![reply(200, response)]);
        let mut p = provider(&mock);
        p.config.kind = kind.into();
        p.config.anthropic_auth = auth.into();
        assert_eq!(
            p.json("system", &json!({"input":1})).await.unwrap(),
            json!({"ok":true})
        );
        let requests = mock.requests.lock().unwrap();
        let (headers, body) = &requests[0];
        let headers = headers.to_ascii_lowercase();
        assert!(headers.contains("content-type: application/json"));
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["thinking"], json!({"type":"disabled"}));
        if kind == "anthropic" {
            assert!(headers.starts_with("post /v1/messages "));
            assert!(headers.contains("anthropic-version: 2023-06-01"));
            assert!(headers.contains("anthropic-workspace-id: space"));
            assert!(headers.contains(if auth == "bearer" {
                "authorization: bearer mock-key"
            } else {
                "x-api-key: mock-key"
            }));
            assert_eq!(body["max_tokens"], 123.);
            assert_eq!(body["system"], "system");
            assert_eq!(
                body["messages"],
                json!([{"role":"user","content":"{\"input\":1}"}])
            );
        } else {
            assert!(headers.starts_with("post /chat/completions "));
            assert!(headers.contains("authorization: bearer mock-key"));
            assert!(!headers.contains("anthropic-version"));
            assert_eq!(body["max_completion_tokens"], 123.);
            assert_eq!(
                body["messages"],
                json!([{"role":"system","content":"system"},{"role":"user","content":"{\"input\":1}"}])
            );
        }
        assert_eq!(budget(&p), 1);
    }
}

#[tokio::test]
async fn config_errors_block_for_exactly_300_seconds_without_budget() {
    for status in [401, 403, 400, 404, 422] {
        let mock = Mock::new(vec![reply(status, ""), reply(200, success())]);
        let mut p = provider(&mock);
        assert_eq!(
            p.complete("s", "u").await.unwrap_err().to_string(),
            format!("http_{status}_check_provider_config")
        );
        assert_eq!(*p.blocked_until.lock().unwrap(), 1300.);
        p.now = Arc::new(|| 1299.999);
        assert_eq!(p.complete("s", "u").await, Err(ProviderError::Backoff));
        assert_eq!(p.list_models().await, Err(ProviderError::Backoff));
        assert_eq!(mock.count(), 1);
        assert_eq!(budget(&p), 1);
        p.now = Arc::new(|| 1300.);
        p.complete("s", "u").await.unwrap();
        assert_eq!(mock.count(), 2);
        assert_eq!(budget(&p), 2);
    }
}

#[tokio::test]
async fn retries_delays_retry_after_and_exhaustion() {
    for status in [429, 500, 502, 503, 504] {
        let mock = Mock::new((0..8).map(|_| reply(status, "")).collect());
        let mut p = provider(&mock);
        p.config.retries = 7.;
        let delays = Arc::new(Mutex::new(Vec::new()));
        let captured = delays.clone();
        p.sleep = Some(Arc::new(move |d| {
            captured.lock().unwrap().push(d.as_secs_f64())
        }));
        assert_eq!(
            p.complete("s", "u").await,
            Err(ProviderError::ProviderUnavailable)
        );
        assert_eq!(*delays.lock().unwrap(), vec![1., 2., 4., 8., 16., 30., 30.]);
        assert_eq!(mock.count(), 8);
        assert_eq!(budget(&p), 8);
        assert_eq!(*p.blocked_until.lock().unwrap(), 1060.);
        assert_eq!(p.complete("s", "u").await, Err(ProviderError::Backoff));
    }
    for (header, expected) in [
        ("120", 60.),
        ("3", 3.),
        ("Thu, 01 Jan 1970 00:17:00 GMT", 20.),
        ("garbage", 1.),
        ("-1", 1.),
    ] {
        let mut first = reply(429, "");
        first.2 = format!("Retry-After: {header}\r\n");
        let mock = Mock::new(vec![first, reply(200, success())]);
        let mut p = provider(&mock);
        p.sleep = Some(Arc::new(move |d| assert_eq!(d.as_secs_f64(), expected)));
        p.complete("s", "u").await.unwrap();
        assert_eq!(budget(&p), 2);
    }
}

#[tokio::test]
async fn budget_and_missing_key_never_send() {
    let mock = Mock::new(vec![reply(429, "")]);
    let mut p = provider(&mock);
    p.config.requests_per_hour = 1.;
    assert_eq!(p.complete("s", "u").await, Err(ProviderError::HourlyBudget));
    assert_eq!(p.list_models().await, Err(ProviderError::HourlyBudget));
    assert_eq!(mock.count(), 1);
    assert_eq!(budget(&p), 1);
    let mock = Mock::new(vec![]);
    let mut p = provider(&mock);
    p.key.clear();
    assert_eq!(
        p.complete("s", "u").await,
        Err(ProviderError::SaveApiKeyFirst)
    );
    assert_eq!(p.list_models().await, Err(ProviderError::SaveApiKeyFirst));
    assert_eq!(budget(&p), 0);
    assert_eq!(mock.count(), 0);
}

#[tokio::test]
async fn content_failures_do_not_retry() {
    for (kind, body, error) in [
        ("openai", "".into(), ProviderError::InvalidProviderResponse),
        (
            "openai",
            "bad".into(),
            ProviderError::InvalidProviderResponse,
        ),
        ("openai", "[]".into(), ProviderError::EmptyModelResponse),
        ("openai", "{}".into(), ProviderError::EmptyModelResponse),
        (
            "openai",
            json!({"choices":[{"message":{"content":" "}}]}).to_string(),
            ProviderError::EmptyModelResponse,
        ),
        (
            "openai",
            json!({"choices":[{"finish_reason":"length"}]}).to_string(),
            ProviderError::OutputTruncated,
        ),
        (
            "anthropic",
            json!({"stop_reason":"max_tokens"}).to_string(),
            ProviderError::OutputTruncated,
        ),
        (
            "openai",
            "x".repeat(1_000_001),
            ProviderError::ResponseTooLarge,
        ),
    ] {
        let mock = Mock::new(vec![reply(200, body)]);
        let mut p = provider(&mock);
        p.config.kind = kind.into();
        assert_eq!(p.complete("s", "u").await, Err(error));
        assert_eq!(budget(&p), 1);
        assert_eq!(mock.count(), 1);
    }
    for (content, error) in [
        ("[]", ProviderError::InvalidJsonObject),
        ("bad", ProviderError::InvalidJson),
    ] {
        let mock = Mock::new(vec![reply(
            200,
            json!({"choices":[{"message":{"content":content}}]}),
        )]);
        let p = provider(&mock);
        assert_eq!(p.json("s", &json!({})).await, Err(error));
        assert_eq!(mock.count(), 1);
    }
    let mock = Mock::new(vec![reply(418, "")]);
    let p = provider(&mock);
    assert_eq!(
        p.complete("s", "u").await.unwrap_err().to_string(),
        "http_418"
    );
    assert_eq!(mock.count(), 1);
}

#[tokio::test]
async fn timeout_is_enforced_in_blocking_transport() {
    let mock = Mock::new(vec![(
        200,
        success().to_string(),
        String::new(),
        Duration::from_millis(100),
    )]);
    let mut p = provider(&mock);
    p.config.timeout_seconds = 0.02;
    p.config.retries = 0.;
    assert_eq!(
        p.complete("s", "u").await,
        Err(ProviderError::ProviderUnavailable)
    );
    assert_eq!(mock.count(), 1);
    assert_eq!(budget(&p), 1);
}

#[tokio::test]
async fn models_contract_and_deepseek_plan_without_network() {
    for kind in ["openai", "anthropic"] {
        let mock = Mock::new(vec![reply(
            200,
            json!({"data":[{"id":"z"},{"id":"a"},{"id":"z"},{"id":4},{"id":""},{"id":"x".repeat(201)}]}),
        )]);
        let mut p = provider(&mock);
        p.config.kind = kind.into();
        assert_eq!(p.list_models().await.unwrap(), vec!["", "a", "z"]);
        let requests = mock.requests.lock().unwrap();
        let headers = requests[0].0.to_ascii_lowercase();
        assert!(headers.starts_with(if kind == "anthropic" {
            "get /v1/models "
        } else {
            "get /models "
        }));
        assert!(headers.contains(if kind == "anthropic" {
            "x-api-key: mock-key"
        } else {
            "authorization: bearer mock-key"
        }));
        assert_eq!(budget(&p), 1);
        p.config.base_url = "https://API.DEEPSEEK.COM:443/anthropic".into();
        assert_eq!(
            p.model_request(),
            ("https://api.deepseek.com/models".into(), true)
        );
        p.config.base_url = "https://api.deepseek.com.evil.invalid".into();
        assert!(!p.model_request().1);
    }
    for (status, body, error) in [
        (403, "{}", "models_http_403"),
        (200, "{}", "invalid_model_list"),
        (200, "bad", "invalid_provider_response"),
    ] {
        let mock = Mock::new(vec![reply(status, body)]);
        let p = provider(&mock);
        assert_eq!(p.list_models().await.unwrap_err().to_string(), error);
        assert_eq!(mock.count(), 1);
    }
}

#[tokio::test]
async fn cancellation_discards_result_but_request_and_budget_survive() {
    let mock = Mock::new(vec![(
        401,
        String::new(),
        String::new(),
        Duration::from_millis(100),
    )]);
    let p = provider(&mock);
    let clone = p.clone();
    let task = tokio::spawn(async move { clone.complete("s", "u").await });
    for _ in 0..200 {
        if mock.count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(mock.count(), 1);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    for _ in 0..200 {
        if *p.blocked_until.lock().unwrap() == 1300. {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(*p.blocked_until.lock().unwrap(), 1300.);
    assert_eq!(budget(&p), 1);
    assert_eq!(mock.count(), 1);
}

#[tokio::test]
async fn shared_budget_serializes_concurrent_admission() {
    let mock = Mock::new(vec![reply(200, success())]);
    let mut p = provider(&mock);
    p.config.requests_per_hour = 1.;
    let (a, b) = tokio::join!(p.complete("s", "a"), p.complete("s", "b"));
    assert!(matches!(
        (a, b),
        (Ok(_), Err(ProviderError::HourlyBudget)) | (Err(ProviderError::HourlyBudget), Ok(_))
    ));
    assert_eq!(mock.count(), 1);
    assert_eq!(budget(&p), 1);
}

#[tokio::test]
async fn models_limit_before_utf16_sort_and_redirect_never_followed() {
    let mut rows = vec![json!({"id":"\u{e000}"}), json!({"id":"\u{10000}"})];
    rows.extend((0..500).map(|n| json!({"id":format!("m{n:03}")})));
    let mock = Mock::new(vec![reply(200, json!({"data":rows}))]);
    let p = provider(&mock);
    let models = p.list_models().await.unwrap();
    assert_eq!(models.len(), 500);
    assert!(!models.iter().any(|m| m == "m498"));
    assert_eq!(&models[498..], &["\u{10000}", "\u{e000}"]);
    let target = Mock::new(vec![]);
    let mut redirect = reply(302, "");
    redirect.2 = format!("Location: {}/secret\r\n", target.url);
    let mock = Mock::new(vec![redirect]);
    let mut p = provider(&mock);
    p.config.retries = 0.;
    assert_eq!(
        p.complete("s", "u").await,
        Err(ProviderError::ProviderUnavailable)
    );
    assert_eq!(target.count(), 0);
    assert_eq!(budget(&p), 1);
}
