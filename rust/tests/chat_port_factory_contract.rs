//! A factory-published OneBot port must own a working loopback connection.
use futures_util::{SinkExt, StreamExt};
use qq_inner_core::engine::io::{
    adapters::onebot::connect::ConnectConfig,
    factory::{prepare, HostServices, SecretResolver},
    ChatPort,
};
use serde_json::{json, Value};
use std::{fs, path::{Path, PathBuf}, sync::Arc};
use tokio::{net::TcpListener, task::JoinHandle, time::{sleep, timeout, Duration}};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{handshake::server::{Request, Response}, Message},
};

const TOKEN: &str = "a+b &?";

struct Mock {
    url: String,
    task: JoinHandle<()>,
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[allow(clippy::result_large_err)]
async fn mock() -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let socket = accept_hdr_async(stream, |request: &Request, response: Response| {
                if request.uri().query() == Some("access_token=a%2Bb+%26%3F") {
                    Ok(response)
                } else {
                    Err(tokio_tungstenite::tungstenite::http::Response::builder()
                        .status(401)
                        .body(Some("unauthorized".into()))
                        .unwrap())
                }
            })
            .await;
            let Ok(mut socket) = socket else { continue; };
            let mut first_status = true;
            while let Some(Ok(Message::Text(text))) = socket.next().await {
                let request: Value = serde_json::from_str(&text).unwrap();
                let data = match request["action"].as_str() {
                    Some("get_login_info") => Some(json!({"user_id":123})),
                    Some("get_status") if first_status => {
                        first_status = false;
                        Some(json!({"online":true}))
                    }
                    _ => None,
                };
                if let Some(data) = data {
                    let reply = json!({"echo":request["echo"],"status":"ok","retcode":0,"data":data});
                    if socket.send(Message::Text(reply.to_string())).await.is_err() { break; }
                }
            }
        }
    });
    Mock { url, task }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("plugins")
}

fn onebot_list(root: &Path) -> Value {
    let mut list: Value = serde_json::from_slice(&fs::read(root.join("plugins.json")).unwrap()).unwrap();
    list["mode"] = json!("manual");
    let plugins = list["plugins"].as_array_mut().unwrap();
    plugins.retain(|entry| entry["id"] == "onebot");
    assert_eq!(plugins.len(), 1, "repository list must contain onebot");
    plugins[0]["enabled"] = json!(true);
    list
}

fn secret() -> SecretResolver {
    Arc::new(|_| None)
}

fn connect_config(mock: &Mock) -> ConnectConfig {
    ConnectConfig {
        url: mock.url.clone(),
        self_id: "123".into(),
        self_id_is_truthy: true,
        heartbeat_seconds: 10.0,
        request_timeout_seconds: 0.15,
        reconnect_max_seconds: 1.0,
        max_lost_seconds: 30.0,
        degraded_probe_seconds: 1.0,
        forward_enabled: false,
    }
}

fn host_with_connection(mock: &Mock) -> HostServices {
    HostServices {
        secret: secret(),
        onebot_connect: Some((connect_config(mock), TOKEN.into())),
        audit: None,
        model_hourly_limit: u32::MAX,
    }
}

async fn wait_connected(port: &dyn ChatPort) {
    timeout(Duration::from_secs(15), async {
        loop {
            let state = port.state();
            if state.connected && state.online { break; }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("factory-published OneBot port must connect and come online");
}

#[tokio::test]
async fn factory_publishes_a_connected_onebot_port_without_host_callbacks() {
    let mock = mock().await;
    let root = root();
    let outcome = prepare(&root, &onebot_list(&root), &host_with_connection(&mock));
    let published = outcome.published.expect("factory must publish the enabled OneBot port");
    let port = published.onebot.expect("published generation must include OneBot");

    assert_eq!(port.manifest().name, "onebot");
    assert!(published.registry.chat.get("onebot").is_some());
    wait_connected(port.as_ref()).await;
    assert!(port.capabilities().notices);
}

#[test]
fn factory_without_connection_config_reports_why() {
    let root = root();
    let outcome = prepare(&root, &onebot_list(&root), &HostServices::without_runtime(secret()));

    assert!(outcome.published.as_ref().is_none_or(|ports| ports.onebot.is_none()));
    assert_eq!(outcome.status["plugins"]["onebot"]["reason"], "host_onebot_unavailable");
}

#[tokio::test]
async fn published_onebot_port_reports_its_own_disconnect() {
    let mock = mock().await;
    let root = root();
    let outcome = prepare(&root, &onebot_list(&root), &host_with_connection(&mock));
    let port = outcome
        .published
        .expect("factory must publish")
        .onebot
        .expect("OneBot must be in the generation");
    wait_connected(port.as_ref()).await;
    assert!(port.state().connected);

    drop(mock);
    timeout(Duration::from_secs(15), async {
        loop {
            if !port.state().connected { break; }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("published port must reflect its own socket disconnect");
}
