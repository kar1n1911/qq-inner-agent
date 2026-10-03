//! 控制协议 v1：复用运行时资源，配置文件仍由 Node 写入。
//! 协议未指定处：learningCounts 为 {memories, expressions}；entries 先列三层记忆
//! （附 revisions），再列表达行，总数受 limit 限制。行字段保持仪表盘 SQLite 形状。
//! state.get 读取引擎最新 status.json（沿用五秒心跳）；接收诊断复用现有 OneBot，
//! 只捕获本帐号消息，60 秒截止、最多 30 条。重载停止旧捕获，后续请求用新资源。
//! 非法 JSON/超限行无法可信关联 id，返回 id:null；排空至换行后允许继续请求。
//! 无换行的 EOF 不执行请求。输出也限制为 1 MiB；过大事件丢弃，响应返回错误。
use crate::{
    config::{self, Config},
    onebot::OneBot,
    provider::Provider,
    store::Store,
};
use anyhow::{ensure, Result};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    io::{Read, Seek, SeekFrom},
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::mpsc,
    task::{JoinHandle, JoinSet},
};

pub const MAX_LINE: usize = 1024 * 1024;
const QUEUE: usize = 32;
const EVENTS: &[&str] = &[
    "onebot",
    "decision",
    "send_assessment",
    "message_sent",
    "chat_learning_updated",
    "config_applied",
    "cycle_error",
];

#[derive(Clone, Default)]
pub struct Events(Arc<Mutex<Subscribers>>);
type Subscribers = HashMap<u64, mpsc::Sender<Arc<Vec<u8>>>>;
impl Events {
    /// 引擎日志回调只能 try_send：慢连接满队列时丢事件，绝不等待或断开。
    pub fn publish(&self, event: &str, data: Value) {
        if !EVENTS.contains(&event) {
            return;
        }
        let bytes = frame(json!({"event":event,"data":data}));
        if bytes.len() > MAX_LINE + 1 {
            return;
        }
        self.0.lock().unwrap().retain(|_, tx| {
            !matches!(
                tx.try_send(bytes.clone()),
                Err(mpsc::error::TrySendError::Closed(_))
            )
        });
    }
}
fn frame(value: Value) -> Arc<Vec<u8>> {
    let mut bytes = serde_json::to_vec(&value).expect("JSON value");
    bytes.push(b'\n');
    Arc::new(bytes)
}
fn failure(id: Value, code: &str) -> Value {
    json!({"id":id,"ok":false,"error":{"code":code,"message":code}})
}

pub trait Handler: Send + Sync + 'static {
    fn request<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value>>;
}

// 保存 inode，避免退出时误删后来替换的路径；取消/异常退出同样清理。
struct SocketFile {
    path: PathBuf,
    dev: u64,
    ino: u64,
}
impl Drop for SocketFile {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.dev() == self.dev && m.ino() == self.ino)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}
pub struct Server {
    task: JoinHandle<()>,
    _file: SocketFile,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}
impl Server {
    pub fn bind(dir: &Path, handler: Arc<dyn Handler>, events: Events) -> Result<Self> {
        let path = dir.join("control.sock");
        match fs::symlink_metadata(&path) {
            Ok(m) => {
                ensure!(m.file_type().is_socket(), "control_path_not_socket");
                // 只清理失去监听者的旧文件，不夺取另一个运行中进程的 socket。
                match std::os::unix::net::UnixStream::connect(&path) {
                    Ok(_) => anyhow::bail!("control_socket_in_use"),
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                        ) => {}
                    Err(e) => return Err(e.into()),
                }
                fs::remove_file(&path)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let listener = UnixListener::bind(&path)?;
        let meta = fs::symlink_metadata(&path)?;
        let file = SocketFile {
            path,
            dev: meta.dev(),
            ino: meta.ino(),
        };
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut clients = JoinSet::new();
            let mut serial = 0u64;
            loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    accepted = listener.accept() => match accepted {
                        Ok((socket, _)) => {
                            serial += 1;
                            clients.spawn(connection(socket, handler.clone(), events.clone(), serial));
                        }
                        Err(_) => break,
                    },
                    Some(_) = clients.join_next(), if !clients.is_empty() => {},
                }
            }
            clients.abort_all();
            while clients.join_next().await.is_some() {}
        });
        Ok(Self {
            task,
            _file: file,
            stop: Some(stop),
        })
    }
    pub async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        // 先等待全部连接取消，再由路径守卫移除 socket；重连使用全新队列。
        let _ = (&mut self.task).await;
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
struct Subscription {
    events: Events,
    id: u64,
}
impl Drop for Subscription {
    fn drop(&mut self) {
        self.events.0.lock().unwrap().remove(&self.id);
    }
}
struct Writer(JoinHandle<()>);
impl Drop for Writer {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn connection(socket: UnixStream, handler: Arc<dyn Handler>, events: Events, id: u64) {
    let (mut read, mut write) = socket.into_split();
    let (tx, mut rx) = mpsc::channel::<Arc<Vec<u8>>>(QUEUE);
    events.0.lock().unwrap().insert(id, tx.clone());
    let subscription = Subscription { events, id };
    let mut writer = Writer(tokio::spawn(async move {
        while let Some(bytes) = rx.recv().await {
            if write.write_all(&bytes).await.is_err() {
                break;
            }
        }
    }));
    let reader = async {
        let mut chunk = [0u8; 8192];
        let mut line = Vec::new();
        let mut oversized = false;
        loop {
            let n = match read.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            for &byte in &chunk[..n] {
                // 半包保留到下次 read；粘包逐行处理。超长行只排空至换行，内存不增长。
                if byte != b'\n' {
                    if !oversized {
                        if line.len() == MAX_LINE {
                            oversized = true;
                            line.clear();
                        } else {
                            line.push(byte);
                        }
                    }
                    continue;
                }
                let response = if oversized {
                    failure(Value::Null, "line_too_long")
                } else {
                    dispatch(handler.as_ref(), &line).await
                };
                line.clear();
                oversized = false;
                let mut bytes = frame(response.clone());
                if bytes.len() > MAX_LINE + 1 {
                    bytes = frame(failure(response["id"].clone(), "response_too_large"));
                    // 极长 id 连错误封套都放不下时，仍不能发出超过协议上限的行。
                    if bytes.len() > MAX_LINE + 1 {
                        bytes = frame(failure(Value::Null, "response_too_large"));
                    }
                }
                // 响应不可丢；只暂停本连接读请求，不占用引擎或其它客户端。
                if tx.send(bytes).await.is_err() {
                    return;
                }
            }
        }
        // EOF 的不完整行不是 NDJSON 请求，不执行诊断副作用。
    };
    let read_finished = tokio::select! { _ = reader => true, _ = &mut writer.0 => false };
    drop(subscription);
    drop(tx);
    // 对端只关闭写半边时，排完已接受请求的响应；服务停止仍可取消慢写任务。
    if read_finished {
        let _ = (&mut writer.0).await;
    }
}
async fn dispatch(handler: &dyn Handler, line: &[u8]) -> Value {
    let request: Value = match serde_json::from_slice(line) {
        Ok(v) => v,
        Err(_) => return failure(Value::Null, "invalid_json"),
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return failure(id, "invalid_request");
    };
    if !id.is_string() || request.get("params").is_some_and(|v| !v.is_object()) {
        return failure(id, "invalid_request");
    }
    match handler
        .request(
            method,
            request.get("params").cloned().unwrap_or_else(|| json!({})),
        )
        .await
    {
        Ok(result) => json!({"id":id,"ok":true,"result":result}),
        Err(e) => failure(id, &e.to_string()),
    }
}

/// 网络边界可注入，行为测试不访问真实 QQ 或模型服务。
pub trait Remote: Send + Sync {
    fn state(&self) -> crate::onebot::State;
    fn request<'a>(&'a self, method: &'a str) -> BoxFuture<'a, Result<Value>>;
}
pub struct LiveRemote {
    pub bot: Arc<OneBot>,
    pub provider: Arc<Provider>,
    pub config: Config,
}
impl Remote for LiveRemote {
    fn state(&self) -> crate::onebot::State {
        self.bot.state()
    }
    fn request<'a>(&'a self, method: &'a str) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            match method {
                "models.list" => Ok(json!({"models":self.provider.list_models().await?})),
                "test.model" => {
                    ensure!(
                        self.config.api_key.is_truthy && self.config.provider.model.is_truthy,
                        "waiting_for_setup"
                    );
                    let start = Instant::now();
                    let response = self
                        .provider
                        .json(
                            "只返回 JSON：{\"ok\":true}。",
                            &json!({"test":"仅测试 API 连通性，不包含 QQ 聊天内容"}),
                        )
                        .await?;
                    ensure!(response["ok"] == true, "unexpected_model_response");
                    Ok(
                        json!({"ok":true,"latencyMs":start.elapsed().as_millis(),"model":self.config.provider.model.text}),
                    )
                }
                "contacts.list" => {
                    let groups = self.bot.call("get_group_list", json!({})).await?;
                    let friends = self.bot.call("get_friend_list", json!({})).await?;
                    let map = |v: Value, id: &str, name: &str| -> Vec<Value> {
                        v.as_array()
                            .into_iter()
                            .flatten()
                            .map(|r| json!({"id":config::js_string(&r[id]),"name":r[name]}))
                            .collect()
                    };
                    Ok(
                        json!({"groups":map(groups,"group_id","group_name"),"friends":map(friends,"user_id","nickname")}),
                    )
                }
                "debug.send" => {
                    let state = self.bot.state();
                    ensure!(
                        state.connected && state.online && !state.self_id.is_empty(),
                        "qq_offline"
                    );
                    // 接收者与正文固定：不接受调用者指定目标，不重试不确定发送。
                    let text = format!(
                        "[QQ Inner Agent diagnostic {}] Self-account send test.",
                        rand::random::<u64>()
                    );
                    let result = self
                        .bot
                        .send(&format!("private:{}", state.self_id), &text, None)
                        .await?;
                    Ok(json!({"ok":true,"messageId":result["message_id"]}))
                }
                _ => anyhow::bail!("unknown_method"),
            }
        })
    }
}
#[derive(Default)]
struct Capture {
    until: i64,
    account: String,
    events: Vec<Value>,
}
pub struct Backend {
    dir: PathBuf,
    store: Arc<Mutex<Store>>,
    remote: Mutex<Arc<dyn Remote>>,
    capture: Mutex<Capture>,
    diagnostic: tokio::sync::Mutex<()>,
    secrets: Mutex<Vec<String>>,
}
impl Backend {
    pub fn new(
        dir: PathBuf,
        store: Arc<Mutex<Store>>,
        remote: Arc<dyn Remote>,
        secrets: Vec<String>,
    ) -> Self {
        Self {
            dir,
            store,
            remote: Mutex::new(remote),
            capture: Mutex::new(Capture::default()),
            diagnostic: tokio::sync::Mutex::new(()),
            secrets: Mutex::new(secrets),
        }
    }
    pub fn replace(&self, remote: Arc<dyn Remote>, secrets: Vec<String>) {
        *self.remote.lock().unwrap() = remote;
        *self.secrets.lock().unwrap() = secrets;
        self.capture.lock().unwrap().until = 0;
    }
    pub fn observe(&self, event: &Value) {
        let mut capture = self.capture.lock().unwrap();
        if capture.until <= chrono::Utc::now().timestamp_millis()
            || !matches!(
                event["post_type"].as_str(),
                Some("message" | "message_sent")
            )
            || !matches!(event["message_type"].as_str(), Some("private" | "group"))
            || config::js_string(&event["self_id"]) != capture.account
            || config::js_string(&event["user_id"]) != capture.account
        {
            return;
        }
        let mut types = Vec::new();
        let mut text = String::new();
        if let Some(segments) = event["message"].as_array() {
            for segment in segments {
                let kind = segment["type"].as_str().unwrap_or("unknown");
                if types.len() < 20 && !types.contains(&kind.to_string()) {
                    types.push(kind.to_string());
                }
                if kind == "text" {
                    text.push_str(segment["data"]["text"].as_str().unwrap_or(""));
                }
            }
        } else {
            types.push("text".into());
            let mut rest = event["message"].as_str().unwrap_or("");
            while let Some(start) = rest.find("[CQ:") {
                let Some(end) = rest[start..].find(']') else {
                    break;
                };
                text.push_str(&rest[..start]);
                let kind = rest[start + 4..start + end]
                    .split(',')
                    .next()
                    .unwrap_or("unknown")
                    .to_string();
                if types.len() < 20 && !types.contains(&kind) {
                    types.push(kind);
                }
                text.push_str("[attachment]");
                rest = &rest[start + end + 1..];
            }
            text.push_str(rest);
        }
        for secret in self
            .secrets
            .lock()
            .unwrap()
            .iter()
            .filter(|s| !s.is_empty())
        {
            text = text.replace(secret, "[redacted]");
        }
        capture.events.push(json!({"receivedAt":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"messageId":event.get("message_id").map(config::js_string).unwrap_or_default(),"postType":event["post_type"],"chatType":event["message_type"],"types":types,"text":text.chars().take(1000).collect::<String>()}));
        if capture.events.len() > 30 {
            capture.events.remove(0);
        }
    }
}
fn limit(params: &Value, key: &str, default: u64, max: u64) -> Result<i64> {
    let n = match params.get(key) {
        None => default,
        Some(v) => v
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("invalid_params"))?,
    };
    ensure!(n <= max, "invalid_params");
    Ok(n as i64)
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 20
        && !id.starts_with('0')
        && id.bytes().all(|b| b.is_ascii_digit())
}
impl Handler for Backend {
    fn request<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            match method {
                "models.list" | "test.model" | "contacts.list" | "debug.send" => {
                    let _guard = self
                        .diagnostic
                        .try_lock()
                        .map_err(|_| anyhow::anyhow!("diagnostic_already_running"))?;
                    let remote = self.remote.lock().unwrap().clone();
                    remote.request(method).await
                }
                "debug.receive.start" => {
                    let state = self.remote.lock().unwrap().state();
                    ensure!(state.connected && state.online, "qq_offline");
                    let mut c = self.capture.lock().unwrap();
                    let now = chrono::Utc::now().timestamp_millis();
                    ensure!(c.until <= now, "receive_test_already_running");
                    *c = Capture {
                        until: now + 60_000,
                        account: state.self_id,
                        events: Vec::new(),
                    };
                    Ok(json!({"listening":true,"until":c.until}))
                }
                "debug.receive.status" => {
                    let c = self.capture.lock().unwrap();
                    Ok(
                        json!({"listening":c.until > chrono::Utc::now().timestamp_millis(),"events":c.events}),
                    )
                }
                "debug.receive.stop" => {
                    self.capture.lock().unwrap().until = 0;
                    Ok(json!({"stopped":true}))
                }
                "state.get" | "learning.list" | "learning.reset" | "logs.tail" => {
                    let method = method.to_string();
                    let dir = self.dir.clone();
                    let store = self.store.clone();
                    tokio::task::spawn_blocking(move || local(&dir, &store, &method, &params))
                        .await?
                }
                _ => anyhow::bail!("unknown_method"),
            }
        })
    }
}
fn local(dir: &Path, store: &Mutex<Store>, method: &str, params: &Value) -> Result<Value> {
    let now = chrono::Utc::now().timestamp_millis() as f64 / 1000.;
    match method {
        "state.get" => {
            let mut status: Value = serde_json::from_slice(&fs::read(dir.join("status.json"))?)?;
            ensure!(status.is_object(), "status_unavailable");
            let db = store.lock().unwrap();
            status["learningCounts"] = json!({"memories":db.rows("SELECT COUNT(*) AS n FROM memory_layers WHERE expires>?",[now])?[0]["n"],"expressions":db.rows("SELECT COUNT(*) AS n FROM expressions",[])?[0]["n"]});
            Ok(status)
        }
        "learning.list" => {
            let n = limit(params, "limit", 200, 1000)?;
            let db = store.lock().unwrap();
            let mut entries = db.rows("SELECT * FROM memory_layers WHERE expires>? ORDER BY CASE layer WHEN 'long_term' THEN 0 WHEN 'traits' THEN 1 ELSE 2 END,updated DESC LIMIT ?",rusqlite::params![now,n])?;
            for m in &mut entries {
                m["revisions"] = json!(db.rows("SELECT revision,text,sources,updated,replaced FROM memory_revisions WHERE memory_id=? ORDER BY revision DESC LIMIT 10",[m["id"].as_str().unwrap_or("")])?);
            }
            // 保留仪表盘现有 SQLite 行字段（sources 等仍为 JSON 文本）。
            let remaining = n - entries.len() as i64;
            entries.extend(db.rows(
                "SELECT * FROM expressions ORDER BY updated DESC LIMIT ?",
                [remaining],
            )?);
            Ok(json!({"entries":entries}))
        }
        "learning.reset" => {
            let chat = params["chat"].as_str().unwrap_or("");
            let (kind, id) = chat.split_once(':').unwrap_or(("", ""));
            ensure!(
                matches!(kind, "group" | "private") && valid_id(id),
                "invalid_params"
            );
            let subject = params
                .get("subject")
                .map(|v| v.as_str().ok_or_else(|| anyhow::anyhow!("invalid_params")))
                .transpose()?;
            if let Some(s) = subject {
                ensure!(
                    (s == "group" && kind == "group")
                        || s.strip_prefix("person:").is_some_and(
                            |person| valid_id(person) && (kind == "group" || person == id)
                        ),
                    "invalid_params"
                );
            }
            store.lock().unwrap().reset_learning(chat, now, subject)?;
            Ok(json!({"ok":true}))
        }
        "logs.tail" => {
            let n = limit(params, "lines", 100, 1000)? as usize;
            let mut file = match fs::File::open(dir.join("agent.log")) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(json!({"lines":[]}))
                }
                Err(e) => return Err(e.into()),
            };
            let start = file.metadata()?.len().saturating_sub(65536);
            file.seek(SeekFrom::Start(start))?;
            let mut bytes = Vec::new();
            file.take(65536).read_to_end(&mut bytes)?;
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<_> = text
                .split('\n')
                .skip(usize::from(start > 0))
                .filter(|s| !s.is_empty())
                .collect();
            Ok(json!({"lines":lines[lines.len().saturating_sub(n)..]}))
        }
        _ => anyhow::bail!("unknown_method"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::time::timeout;

    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "qc-{}-{}",
                std::process::id(),
                rand::random::<u32>()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    struct Echo;
    impl Handler for Echo {
        fn request<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value>> {
            Box::pin(async move {
                ensure!(method == "echo", "unknown_method");
                Ok(params)
            })
        }
    }
    struct Client(
        BufReader<tokio::net::unix::OwnedReadHalf>,
        tokio::net::unix::OwnedWriteHalf,
    );
    impl Client {
        async fn connect(dir: &Path) -> Self {
            let (r, w) = UnixStream::connect(dir.join("control.sock"))
                .await
                .unwrap()
                .into_split();
            Self(BufReader::new(r), w)
        }
        async fn read(&mut self) -> Value {
            let mut line = String::new();
            let n = timeout(Duration::from_secs(5), self.0.read_line(&mut line))
                .await
                .expect("response deadline")
                .unwrap();
            assert!(n > 0, "unexpected disconnect");
            serde_json::from_str(&line).unwrap()
        }
        async fn call(&mut self, id: &str, method: &str, params: Value) -> Value {
            self.1
                .write_all(&frame(json!({"id":id,"method":method,"params":params})))
                .await
                .unwrap();
            let result = self.read().await;
            assert_eq!(result["id"], id);
            result
        }
    }
    #[tokio::test]
    async fn framing_ids_errors_and_recovery() {
        let dir = Temp::new();
        let server = Server::bind(&dir.0, Arc::new(Echo), Events::default()).unwrap();
        let mut c = Client::connect(&dir.0).await;
        c.1.write_all(b"{\"id\":\"half\",\"method\":")
            .await
            .unwrap();
        let mut byte = [0];
        assert!(timeout(Duration::from_millis(30), c.0.read(&mut byte))
            .await
            .is_err());
        c.1.write_all(b"\"echo\",\"params\":{\"text\":\"\xe4\xb8")
            .await
            .unwrap();
        c.1.write_all(b"\xad\"}}\n{\"id\":\"two\",\"method\":\"echo\"}\n")
            .await
            .unwrap();
        assert_eq!(
            c.read().await,
            json!({"id":"half","ok":true,"result":{"text":"中"}})
        );
        assert_eq!(c.read().await, json!({"id":"two","ok":true,"result":{}}));
        c.1.write_all(b"{bad}\n{\"id\":\"unknown\",\"method\":\"missing\"}\n{\"method\":\"echo\"}\n{\"id\":\"p\",\"method\":\"echo\",\"params\":[]}\n").await.unwrap();
        assert_eq!(c.read().await, failure(Value::Null, "invalid_json"));
        assert_eq!(c.read().await, failure(json!("unknown"), "unknown_method"));
        assert_eq!(c.read().await, failure(Value::Null, "invalid_request"));
        assert_eq!(c.read().await, failure(json!("p"), "invalid_request"));
        // 上限按字节、不含换行；用合法 JSON 加空白精确填满 1 MiB。
        let mut exact = b"{\"id\":\"edge\",\"method\":\"echo\"}".to_vec();
        exact.resize(MAX_LINE, b' ');
        exact.push(b'\n');
        c.1.write_all(&exact).await.unwrap();
        assert_eq!(c.read().await["id"], "edge");
        let mut huge = vec![b'x'; MAX_LINE + 9000];
        huge.extend_from_slice(b"\n{\"id\":\"after\",\"method\":\"echo\"}\n");
        c.1.write_all(&huge).await.unwrap();
        assert_eq!(c.read().await, failure(Value::Null, "line_too_long"));
        assert_eq!(c.read().await["id"], "after");
        assert_eq!(c.call("escaped\n雪", "echo", json!({})).await["ok"], true);
        let mut outgoing = json!({"id":"large","method":"echo","params":{"text":""}});
        // 满长度且非空白的请求，校验正常响应也能在字节上限内完成。
        let overhead = outgoing.to_string().len();
        outgoing["params"]["text"] = json!("x".repeat(MAX_LINE - overhead));
        c.1.write_all(&frame(outgoing)).await.unwrap();
        let response = c.read().await;
        // echo 请求封套比响应更长，因此此例仍可成功且不能越界。
        assert_eq!(response["id"], "large");
        assert_eq!(response["ok"], true);
        let mut huge_id = json!({"id":"","method":"unknown"});
        let overhead = huge_id.to_string().len();
        huge_id["id"] = json!("x".repeat(MAX_LINE - overhead));
        c.1.write_all(&frame(huge_id)).await.unwrap();
        assert_eq!(c.read().await, failure(Value::Null, "response_too_large"));
        server.stop().await;
    }
    #[tokio::test]
    async fn concurrent_clients_events_and_socket_lifecycle() {
        let dir = Temp::new();
        let path = dir.0.join("control.sock");
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        let events = Events::default();
        let server = Server::bind(&dir.0, Arc::new(Echo), events.clone()).unwrap();
        assert!(Server::bind(&dir.0, Arc::new(Echo), Events::default()).is_err());
        let mut tasks = JoinSet::new();
        for n in 0..12 {
            let dir = dir.0.clone();
            tasks.spawn(async move {
                let mut c = Client::connect(&dir).await;
                for i in 0..10 {
                    let id = format!("{n}/{i}");
                    assert_eq!(
                        c.call(&id, "echo", json!({"n":n})).await["result"],
                        json!({"n":n})
                    );
                }
            });
        }
        while let Some(result) = tasks.join_next().await {
            result.unwrap();
        }
        let mut c = Client::connect(&dir.0).await;
        c.call("ready", "echo", json!({})).await;
        for event in EVENTS {
            let data = json!({"chat":"group:12","score":0.625,"tags":["a"],"ts":1700000000.25,"code":"qq_offline"});
            events.publish(event, data.clone());
            assert_eq!(c.read().await, json!({"event":event,"data":data}));
        }
        server.stop().await;
        assert!(!path.exists());
        let mut bytes = Vec::new();
        timeout(Duration::from_secs(2), c.0.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        let server = Server::bind(&dir.0, Arc::new(Echo), Events::default()).unwrap();
        assert_eq!(
            Client::connect(&dir.0)
                .await
                .call("reconnect", "echo", json!({}))
                .await["ok"],
            true
        );
        drop(server);
        assert!(!path.exists());
        fs::write(&path, "not a socket").unwrap();
        assert!(Server::bind(&dir.0, Arc::new(Echo), Events::default()).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "not a socket");
    }
    #[tokio::test]
    async fn slow_reader_drops_events_without_blocking_or_disconnect() {
        let dir = Temp::new();
        let events = Events::default();
        let server = Server::bind(&dir.0, Arc::new(Echo), events.clone()).unwrap();
        let mut slow = Client::connect(&dir.0).await;
        slow.call("ready", "echo", json!({})).await;
        // 不读客户端；远超内核 socket 缓冲和应用队列容量，模拟引擎持续产生日志。
        let publishing = events.clone();
        timeout(
            Duration::from_secs(5),
            tokio::task::spawn_blocking(move || {
                for seq in 0..400 {
                    publishing.publish("decision", json!({"seq":seq,"padding":"x".repeat(65536)}));
                }
            }),
        )
        .await
        .unwrap()
        .unwrap();
        let mut fast = Client::connect(&dir.0).await;
        assert_eq!(
            fast.call("fast", "echo", json!({"alive":true})).await["result"],
            json!({"alive":true})
        );
        slow.1
            .write_all(b"{\"id\":\"still-connected\",\"method\":\"echo\"}\n")
            .await
            .unwrap();
        let mut count = 0;
        loop {
            let value = slow.read().await;
            if value["id"] == "still-connected" {
                assert_eq!(value["ok"], true);
                break;
            }
            assert_eq!(value["event"], "decision");
            count += 1;
        }
        assert!(
            count > 0 && count < 400,
            "must deliver some and drop overflow"
        );
        events.publish("onebot", json!({"state":"connected"}));
        assert_eq!(
            slow.read().await,
            json!({"event":"onebot","data":{"state":"connected"}})
        );
        server.stop().await;
    }
    #[tokio::test]
    async fn half_close_drains_responses_but_never_executes_unterminated_request() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Recording(Arc<AtomicUsize>);
        impl Handler for Recording {
            fn request<'a>(&'a self, method: &'a str, _: Value) -> BoxFuture<'a, Result<Value>> {
                Box::pin(async move {
                    self.0.fetch_add(1, Ordering::SeqCst);
                    if method == "large" {
                        Ok(json!({"text":"x".repeat(MAX_LINE)}))
                    } else {
                        Ok(json!({"accepted":true}))
                    }
                })
            }
        }
        let dir = Temp::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let server = Server::bind(
            &dir.0,
            Arc::new(Recording(calls.clone())),
            Events::default(),
        )
        .unwrap();
        let mut c = Client::connect(&dir.0).await;
        assert_eq!(
            c.call("large", "large", json!({})).await,
            failure(json!("large"), "response_too_large")
        );
        c.1.write_all(
            b"{\"id\":\"complete\",\"method\":\"run\"}\n{\"id\":\"partial\",\"method\":\"run\"}",
        )
        .await
        .unwrap();
        c.1.shutdown().await.unwrap();
        assert_eq!(
            c.read().await,
            json!({"id":"complete","ok":true,"result":{"accepted":true}})
        );
        let mut rest = Vec::new();
        timeout(Duration::from_secs(2), c.0.read_to_end(&mut rest))
            .await
            .unwrap()
            .unwrap();
        assert!(rest.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        server.stop().await;
    }
    struct OfflineRemote;
    impl Remote for OfflineRemote {
        fn state(&self) -> crate::onebot::State {
            crate::onebot::State {
                connected: true,
                online: true,
                self_id: "42".into(),
                reconnects: 0,
            }
        }
        fn request<'a>(&'a self, method: &'a str) -> BoxFuture<'a, Result<Value>> {
            Box::pin(async move {
                Ok(match method {
                    "models.list" => json!({"models":["fixture-model"]}),
                    "test.model" => json!({"ok":true,"model":"fixture-model","latencyMs":12}),
                    "contacts.list" => {
                        json!({"groups":[{"id":"12","name":"Group"}],"friends":[{"id":"42","name":"Self"}]})
                    }
                    "debug.send" => json!({"ok":true,"messageId":123}),
                    _ => panic!("unexpected diagnostic"),
                })
            })
        }
    }
    #[tokio::test]
    async fn backend_status_learning_logs_and_diagnostics_contracts() {
        let dir = Temp::new();
        let store = Arc::new(Mutex::new(Store::in_memory().unwrap()));
        // 独立构造协议字段，不能从实现生成测试期望。
        let status = json!({"updatedAt":"2026-10-04T00:00:00.000Z","pid":123,"mode":"active","appliedRevision":"abc","reloading":false,"reloadError":null,"scheduleActive":true,"activityRhythm":null,"missing":[],"onebotConnected":true,"qqOnline":true,"selfId":"42","reconnects":0,"activeChats":1,"model":"fixture-model","provider":"openai","apiCallsThisRun":3,"lastCycleAt":1700000000.125,"lastError":null});
        fs::write(dir.0.join("status.json"), status.to_string()).unwrap();
        fs::write(dir.0.join("agent.log"), "first\nsecond\nthird\n").unwrap();
        {
            let db = store.lock().unwrap();
            db.execute("INSERT INTO memory_layers(id,chat,subject,layer,slot,text,sources,created,updated,expires) VALUES('m','private:42','person:42','long_term','s','remember','[]',1.25,2.5,9999999999)",[]).unwrap();
            db.execute(
                "INSERT INTO memory_revisions VALUES('m',1,'old','[]',1.25,2.5)",
                [],
            )
            .unwrap();
            db.execute("INSERT INTO expressions(chat,subject,kind,term,updated) VALUES('group:12','group','word','hi',2.5)",[]).unwrap();
        }
        let backend = Arc::new(Backend::new(
            dir.0.clone(),
            store.clone(),
            Arc::new(OfflineRemote),
            vec!["secret".into()],
        ));
        let server = Server::bind(&dir.0, backend.clone(), Events::default()).unwrap();
        let mut c = Client::connect(&dir.0).await;
        let mut expected = status;
        expected["learningCounts"] = json!({"memories":1,"expressions":1});
        assert_eq!(
            c.call("s", "state.get", json!({})).await["result"],
            expected
        );
        assert_eq!(
            c.call("l", "logs.tail", json!({"lines":2})).await["result"],
            json!({"lines":["second","third"]})
        );
        assert_eq!(
            c.call("z", "logs.tail", json!({"lines":0})).await["result"],
            json!({"lines":[]})
        );
        let entries = c.call("list", "learning.list", json!({})).await["result"]["entries"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["text"], "remember");
        assert_eq!(entries[0]["sources"], "[]");
        assert_eq!(entries[0]["updated"], 2.5);
        assert_eq!(
            entries[0]["revisions"],
            json!([{"revision":1,"text":"old","sources":"[]","updated":1.25,"replaced":2.5}])
        );
        assert_eq!(entries[1]["term"], "hi");
        for params in [
            json!({"chat":"private:42","subject":"person:43"}),
            json!({"chat":"group:0"}),
            json!({"chat":"group:12","subject":null}),
        ] {
            assert_eq!(
                c.call("bad", "learning.reset", params).await["error"]["code"],
                "invalid_params"
            );
        }
        assert_eq!(
            c.call(
                "reset",
                "learning.reset",
                json!({"chat":"private:42","subject":"person:42"})
            )
            .await["result"],
            json!({"ok":true})
        );
        assert_eq!(
            c.call("count", "state.get", json!({})).await["result"]["learningCounts"],
            json!({"memories":0,"expressions":1})
        );
        assert!(store
            .lock()
            .unwrap()
            .rows("SELECT * FROM memory_revisions", [])
            .unwrap()
            .is_empty());
        assert_eq!(
            c.call("models", "models.list", json!({})).await["result"],
            json!({"models":["fixture-model"]})
        );
        assert_eq!(
            c.call("test", "test.model", json!({})).await["result"],
            json!({"ok":true,"model":"fixture-model","latencyMs":12})
        );
        assert_eq!(
            c.call("send", "debug.send", json!({"chat":"group:evil"}))
                .await["result"],
            json!({"ok":true,"messageId":123})
        );
        assert_eq!(
            c.call("contacts", "contacts.list", json!({})).await["result"]["groups"],
            json!([{"id":"12","name":"Group"}])
        );
        assert_eq!(
            c.call("idle", "debug.receive.status", json!({})).await["result"],
            json!({"listening":false,"events":[]})
        );
        let started = c.call("start", "debug.receive.start", json!({})).await;
        assert_eq!(started["result"]["listening"], true);
        assert!(
            (started["result"]["until"].as_i64().unwrap() - chrono::Utc::now().timestamp_millis())
                .abs_diff(60000)
                < 2000
        );
        assert_eq!(
            c.call("again", "debug.receive.start", json!({})).await["error"]["code"],
            "receive_test_already_running"
        );
        backend.observe(&json!({"self_id":42,"user_id":99,"post_type":"message","message_type":"private","message":"not self"}));
        for i in 0..35 {
            backend.observe(&json!({"self_id":42,"user_id":42,"post_type":"message","message_type":"private","message_id":i,"message":[{"type":"text","data":{"text":"secret hello"}},{"type":"image","data":{"url":"private"}}]}));
        }
        let captured = c.call("capture", "debug.receive.status", json!({})).await;
        let captured = captured["result"]["events"].as_array().unwrap();
        assert_eq!(captured.len(), 30);
        assert_eq!(captured[0]["messageId"], "5");
        assert_eq!(captured[0]["text"], "[redacted] hello");
        assert_eq!(captured[0]["types"], json!(["text", "image"]));
        assert_eq!(
            c.call("stop", "debug.receive.stop", json!({})).await["result"],
            json!({"stopped":true})
        );
        assert_eq!(
            c.call("stopped", "debug.receive.status", json!({})).await["result"]["listening"],
            false
        );
        backend.replace(Arc::new(OfflineRemote), vec![]);
        assert!(!dir.0.join("config.json").exists());
        assert!(!dir.0.join("secrets.json").exists());
        server.stop().await;
    }
}
