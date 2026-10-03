//! 不变量用设计推导的精确数量；分类只限定合理类别/方向，模糊置信度是硬约束。
use qq_inner_core::{
    config::{defaults, merge, Agent},
    conversation::{self, Relation, Stage},
    media::{Collector, Config},
    policy::{Hint, Message},
    store::Store,
};
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
};
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("media-test-{}", rand::random::<u64>()));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn agent() -> Agent {
    serde_json::from_value(merge(
        &defaults()["agent"],
        &json!({"allowedGroups":["10","11"],"allowedUsers":["20"]}),
    ))
    .unwrap()
}
fn event(id: u64, message: Value) -> Value {
    json!({"post_type":"message","message_type":"group","self_id":99,"user_id":20,"group_id":10,"message_id":id,"time":1000,"message":message})
}
fn image(file: &str) -> Value {
    json!([{"type":"image","data":{"file":file}}])
}
fn collector(t: &Temp) -> Collector {
    Collector::new(
        &t.0,
        Config {
            enabled: true,
            ..Default::default()
        },
    )
}
fn ingest(c: &Collector, s: &Store, e: &Value) -> qq_inner_core::media::Report {
    c.ingest(s, e, "99", &agent(), 1000.).unwrap()
}
fn count(s: &Store, table: &str) -> i64 {
    s.connection()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
fn server(
    status: &str,
    body: &'static [u8],
    declared: usize,
) -> (String, std::thread::JoinHandle<()>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/temporary?token=secret", l.local_addr().unwrap());
    let status = status.to_owned();
    let h = std::thread::spawn(move || {
        let (mut stream, _) = l.accept().unwrap();
        let mut b = [0; 4096];
        let mut request = Vec::new();
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut b).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&b[..n]);
        }
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        stream.write_all(body).unwrap();
    });
    (url, h)
}
#[test]
fn bytes_survive_expired_url_and_fifty_uses_have_one_asset() {
    let t = Temp::new();
    let c = collector(&t);
    let s = Store::in_memory().unwrap();
    let (url, h) = server("200 OK", b"abc", 3);
    assert_eq!(ingest(&c, &s, &event(1, image(&url))).collected, 1);
    h.join().unwrap();
    // SHA-256 的公开 abc 向量，期望不是从待测实现计算。
    let file =
        "media/group:10/ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad.bin";
    assert_eq!(fs::read(t.0.join(file)).unwrap(), b"abc");
    let local = t.0.join("input.png");
    fs::write(&local, b"abc").unwrap();
    for id in 2..=50 {
        assert_eq!(
            ingest(&c, &s, &event(id, image(local.to_str().unwrap()))).collected,
            1
        );
    }
    assert_eq!(
        ingest(&c, &s, &event(50, image(local.to_str().unwrap()))).collected,
        0
    );
    let rows = s.media_assets("group:10").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["occurrences"], 50);
    assert_eq!(rows[0]["file"], file);
    assert_eq!(rows[0]["bytes"], 3);
    assert_eq!(rows[0]["fitness"], json!({}));
    assert_eq!(fs::read_dir(t.0.join("media/group:10")).unwrap().count(), 1);
    assert_eq!(
        s.connection()
            .query_row(
                "SELECT count(*) FROM media_contexts WHERE role='usage'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        50
    );
    // 上下文表只允许四个引用字段，不复制正文。
    let cols: Vec<String> = s
        .connection()
        .prepare("PRAGMA table_info(media_contexts)")
        .unwrap()
        .query_map([], |r| r.get(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(cols, vec!["chat", "hash", "message_id", "role"]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(t.0.join("media/group:10"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}
#[test]
fn failed_download_never_leaves_asset_context_receipt_or_file() {
    for (status, body, declared) in [
        ("404 Not Found", b"abc" as &'static [u8], 3),
        ("200 OK", b"abc", 20),
    ] {
        let t = Temp::new();
        let c = collector(&t);
        let s = Store::in_memory().unwrap();
        let (url, h) = server(status, body, declared);
        let r = ingest(&c, &s, &event(1, image(&url)));
        h.join().unwrap();
        assert_eq!(r.collected, 0);
        assert_eq!(r.failures.len(), 1);
        assert!(!r.failures[0].contains("secret"));
        for table in ["media_assets", "media_contexts", "media_receipts"] {
            assert_eq!(count(&s, table), 0);
        }
        assert!(!t.0.join("media").exists());
    }
}
#[test]
fn self_unknown_identity_and_default_disabled_are_noops() {
    let t = Temp::new();
    let c = collector(&t);
    let s = Store::in_memory().unwrap();
    let mut e = event(1, image("/missing"));
    e["user_id"] = json!(99);
    assert_eq!(ingest(&c, &s, &e).collected, 0);
    assert_eq!(
        c.ingest(&s, &event(2, image("/missing")), "", &agent(), 1000.)
            .unwrap()
            .collected,
        0
    );
    assert_eq!(
        ingest(
            &Collector::new(&t.0, Config::default()),
            &s,
            &event(3, image("/missing"))
        )
        .collected,
        0
    );
    assert!(!s.schema().unwrap()["tables"]
        .as_object()
        .unwrap()
        .contains_key("media_assets"));
    assert_eq!(count(&s, "messages"), 0);
}
#[test]
fn faces_cq_and_chat_isolation() {
    let t = Temp::new();
    let c = collector(&t);
    let s = Store::in_memory().unwrap();
    let p = t.0.join("source");
    fs::write(&p, b"abc").unwrap();
    let e = event(
        1,
        json!(format!("[CQ:image,file={}][CQ:face,id=14]", p.display())),
    );
    assert_eq!(ingest(&c, &s, &e).collected, 2);
    let rows = s.media_assets("group:10").unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .any(|r| r["kind"] == "face" && r["file"] == "14" && r["bytes"] == 0));
    let mut other = event(2, image(p.to_str().unwrap()));
    other["message_type"] = json!("private");
    assert_eq!(ingest(&c, &s, &other).collected, 1);
    assert_eq!(s.media_assets("private:20").unwrap().len(), 1);
    assert_eq!(s.media_assets("group:11").unwrap().len(), 0);
    assert_eq!(count(&s, "media_stages"), 2);
}
#[test]
fn size_limits_and_low_frequency_eviction() {
    let t = Temp::new();
    let s = Store::in_memory().unwrap();
    let c = Collector::new(
        &t.0,
        Config {
            enabled: true,
            max_file_bytes: 3,
            max_total_bytes: 6,
            ..Default::default()
        },
    );
    let p = t.0.join("input");
    for (id, data) in [(1, "abc"), (2, "def"), (3, "def"), (4, "ghi")] {
        fs::write(&p, data).unwrap();
        assert_eq!(
            ingest(&c, &s, &event(id, image(p.to_str().unwrap()))).collected,
            1
        );
    }
    let rows = s.media_assets("group:10").unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|r| r["occurrences"] == 2));
    assert_eq!(fs::read_dir(t.0.join("media/group:10")).unwrap().count(), 2);
    assert!(!rows
        .iter()
        .any(|r| r["hash"] == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
    fs::write(&p, "long").unwrap();
    assert_eq!(
        ingest(&c, &s, &event(5, image(p.to_str().unwrap())))
            .failures
            .len(),
        1
    );
    assert_eq!(count(&s, "media_assets"), 2);
}
fn m(id: usize, text: &str) -> Message {
    Message {
        chat: "group:10".into(),
        id: id.to_string(),
        sender: (id % 2).to_string(),
        name: "".into(),
        text: text.into(),
        ts: id as f64 * 10.,
        is_self: false,
        hint: Hint::Open,
    }
}
#[test]
fn classifier_design_cases_and_ambiguity() {
    let c = conversation::Config {
        closing_markers: vec!["明天见".into()],
        ..Default::default()
    };
    let active = vec![
        m(0, "项目计划讨论"),
        m(1, "项目计划继续"),
        m(2, "项目计划进展"),
    ];
    let r = conversation::classify(&active, 2, 21., &c);
    assert!(matches!(r.stage, Stage::Developing));
    assert_eq!(r.relation, Relation::Continuation);
    assert!(r.confident);
    let stopped = conversation::classify(&active, 2, 300., &c);
    assert!(matches!(stopped.stage, Stage::NaturalEnd | Stage::Closing));
    assert!(!stopped.confident);
    let closed = vec![m(0, "项目计划讨论"), m(1, "项目计划明天见")];
    let r = conversation::classify(&closed, 1, 200., &c);
    assert!(matches!(r.stage, Stage::Closing | Stage::NaturalEnd));
    assert_ne!(r.stage, Stage::Developing);
    // 同样的结束标记，刚到达或后来续聊不能被确定判为收束。
    assert!(!conversation::classify(&closed, 1, 11., &c).confident);
    let mut resumed = closed.clone();
    resumed.push(m(2, "项目计划继续"));
    assert!(!conversation::classify(&resumed, 1, 200., &c).confident);
    let standalone = vec![m(0, "项目计划"), m(1, "今天好热"), m(2, "晚饭吃鱼")];
    let r = conversation::classify(&standalone, 1, 200., &c);
    assert!(matches!(r.stage, Stage::Standalone));
    assert_eq!(r.relation, Relation::Unrelated);
    for messages in [
        vec![],
        vec![m(0, "嗯")],
        vec![m(0, "项目"), m(1, "项目天气")],
    ] {
        assert!(
            !conversation::classify(&messages, messages.len().saturating_sub(1), 20., &c).confident
        );
    }
}

#[test]
fn failed_index_transaction_rolls_back_bytes_and_retry_is_counted_once() {
    let t = Temp::new();
    let s = Store::in_memory().unwrap();
    s.enable_media().unwrap();
    let c = collector(&t);
    let p = t.0.join("input");
    fs::write(&p, "abc").unwrap();
    s.connection().execute_batch("CREATE TRIGGER reject_context BEFORE INSERT ON media_contexts BEGIN SELECT RAISE(ABORT,'test failure'); END;").unwrap();
    let e = event(1, image(p.to_str().unwrap()));
    assert_eq!(ingest(&c, &s, &e).failures.len(), 1);
    for table in ["media_assets", "media_contexts", "media_receipts"] {
        assert_eq!(count(&s, table), 0);
    }
    assert_eq!(fs::read_dir(t.0.join("media/group:10")).unwrap().count(), 0);
    s.connection()
        .execute_batch("DROP TRIGGER reject_context")
        .unwrap();
    assert_eq!(ingest(&c, &s, &e).collected, 1);
    assert_eq!(ingest(&c, &s, &e).collected, 0);
    assert_eq!(s.media_assets("group:10").unwrap()[0]["occurrences"], 1);
}

#[test]
fn migration_reopen_and_context_lookup_use_original_messages() {
    let t = Temp::new();
    let path = t.0.join("db.sqlite");
    let c = collector(&t);
    {
        let s = Store::open(&path).unwrap();
        s.enable_media().unwrap();
        s.enable_media().unwrap();
        ingest(
            &c,
            &s,
            &event(1, json!([{"type":"text","data":{"text":"项目计划"}}])),
        );
        ingest(&c, &s, &event(2, json!([{"type":"face","data":{"id":14}}])));
        ingest(
            &c,
            &s,
            &event(3, json!([{"type":"text","data":{"text":"后续反应"}}])),
        );
        let lookup:String=s.connection().query_row("SELECT m.text FROM media_contexts c JOIN messages m ON c.chat=m.chat AND c.message_id=m.id WHERE c.role='after'",[],|r|r.get(0)).unwrap();
        assert_eq!(lookup, "后续反应");
    }
    let s = Store::open(&path).unwrap();
    s.enable_media().unwrap();
    assert_eq!(s.media_assets("group:10").unwrap()[0]["occurrences"], 1);
    assert_eq!(count(&s, "messages"), 3);
}
