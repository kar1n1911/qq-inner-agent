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
        for table in [
            "media_assets",
            "media_contexts",
            "media_receipts",
            "media_senders",
        ] {
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
    for table in [
        "media_assets",
        "media_contexts",
        "media_receipts",
        "media_senders",
    ] {
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

#[test]
fn source_classifier_defaults_overrides_and_cross_chat_gate_are_exact() {
    use qq_inner_core::media_source::{can_use, classify, Evidence, Override, SourceTier};
    // 不变量：默认未知；无论使用者有多少都不能证明群友照片在网上公开。
    assert_eq!(SourceTier::default(), SourceTier::Unknown);
    assert_eq!(classify(Evidence::default(), 3).tier, SourceTier::Unknown);
    for n in [0, 1, 5, 10000] {
        assert_eq!(
            classify(
                Evidence {
                    distinct_senders: n,
                    ..Default::default()
                },
                3
            )
            .tier,
            SourceTier::Unknown
        );
    }
    let matched = Evidence {
        public_corpus_match: true,
        ..Default::default()
    };
    assert_eq!(classify(matched, 3).tier, SourceTier::Public);
    assert_eq!(
        classify(
            Evidence {
                manual_override: Some(Override::Private),
                ..matched
            },
            3
        )
        .tier,
        SourceTier::Private
    );
    assert_eq!(
        classify(
            Evidence {
                manual_override: Some(Override::Public),
                ..Default::default()
            },
            3
        )
        .tier,
        SourceTier::Public
    );
    for tier in [SourceTier::Private, SourceTier::Unknown] {
        assert!(!can_use(tier, "group:10", "group:11"));
        assert!(can_use(tier, "group:10", "group:10"));
    }
    assert!(can_use(SourceTier::Public, "group:10", "group:11"));
    assert!(!can_use(SourceTier::Public, "private:20", "group:10"));
    // 启发式只断言传播信号方向，不将某个阈值冻结为设计真理。
    let low = classify(
        Evidence {
            distinct_senders: 1,
            ..Default::default()
        },
        3,
    );
    let high = classify(
        Evidence {
            distinct_senders: 100,
            ..Default::default()
        },
        3,
    );
    assert!(!low.widespread && high.widespread);
}

#[test]
fn unverified_segment_markers_and_popularity_never_publish_images() {
    let t = Temp::new();
    let s = Store::in_memory().unwrap();
    let c = collector(&t);
    let p = t.0.join("input");
    fs::write(&p, "abc").unwrap();
    // 合成标记是防猜测的负例，并非声称实际桥返回过这些扩展字段。
    for (id, sender) in [(1, 20), (2, 20), (3, 21), (4, 22), (5, 99)] {
        let mut e = event(
            id,
            json!([{"type":"image","data":{"file":p,"sub_type":1,"emoji":true,"sticker":true}}]),
        );
        e["user_id"] = json!(sender);
        e["sub_type"] = json!("sticker");
        ingest(&c, &s, &e);
        if id == 4 {
            ingest(&c, &s, &e);
        }
    }
    let row = &s.media_assets("group:10").unwrap()[0];
    assert_eq!(row["occurrences"], 4);
    assert_eq!(row["distinct_senders"], 3);
    assert_eq!(row["source_tier"], "unknown");
    assert_eq!(row["source_override"], Value::Null);
    let mut other = event(6, image(p.to_str().unwrap()));
    other["group_id"] = json!(11);
    ingest(&c, &s, &other);
    assert_eq!(
        s.media_assets("group:11").unwrap()[0]["distinct_senders"],
        1
    );
    assert_eq!(
        s.media_assets("group:10").unwrap()[0]["distinct_senders"],
        3
    );
}

#[test]
fn local_corpus_matches_bytes_and_manual_override_survives_more_uses_and_reopen() {
    use qq_inner_core::media_source::{corpus_hashes, Override};
    let t = Temp::new();
    let corpus = t.0.join("public");
    fs::create_dir_all(corpus.join("nested")).unwrap();
    fs::write(corpus.join("nested/arbitrary-name.png"), "abc").unwrap();
    let hashes = corpus_hashes(&corpus, 3).unwrap();
    assert_eq!(hashes.len(), 1);
    let hash = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    assert!(hashes.contains(hash));
    assert!(corpus_hashes(&t.0.join("absent"), 3).unwrap().is_empty());
    assert!(corpus_hashes(&corpus, 2).unwrap().is_empty());
    let c = Collector::new(
        &t.0,
        Config {
            enabled: true,
            public_corpus_dir: Some(corpus),
            ..Default::default()
        },
    );
    let path = t.0.join("db.sqlite");
    let s = Store::open(&path).unwrap();
    let p = t.0.join("input");
    fs::write(&p, "abc").unwrap();
    ingest(&c, &s, &event(1, image(p.to_str().unwrap())));
    assert_eq!(
        s.media_assets("group:10").unwrap()[0]["source_tier"],
        "public"
    );
    s.set_media_source_override("group:10", hash, Some(Override::Private))
        .unwrap();
    ingest(&c, &s, &event(2, image(p.to_str().unwrap())));
    assert_eq!(
        s.media_assets("group:10").unwrap()[0]["source_tier"],
        "private"
    );
    drop(s);
    let s = Store::open(&path).unwrap();
    s.enable_media().unwrap();
    assert_eq!(
        s.media_assets("group:10").unwrap()[0]["source_tier"],
        "private"
    );
    assert_eq!(
        s.media_assets("group:10").unwrap()[0]["distinct_senders"],
        1
    );
    s.set_media_source_override("group:10", hash, None).unwrap();
    assert_eq!(
        s.media_assets("group:10").unwrap()[0]["source_tier"],
        "public"
    );
    // 未命中语料的素材依然未知；人工 public 可明确放行，清除后恢复未知。
    fs::write(&p, "def").unwrap();
    ingest(&c, &s, &event(3, image(p.to_str().unwrap())));
    let rows = s.media_assets("group:10").unwrap();
    let unmatched = rows.iter().find(|r| r["public_corpus_match"] == 0).unwrap();
    let other = unmatched["hash"].as_str().unwrap();
    assert_eq!(unmatched["source_tier"], "unknown");
    s.set_media_source_override("group:10", other, Some(Override::Public))
        .unwrap();
    assert_eq!(
        s.media_assets("group:10")
            .unwrap()
            .iter()
            .find(|r| r["hash"] == other)
            .unwrap()["source_tier"],
        "public"
    );
    s.set_media_source_override("group:10", other, None)
        .unwrap();
    assert_eq!(
        s.media_assets("group:10")
            .unwrap()
            .iter()
            .find(|r| r["hash"] == other)
            .unwrap()["source_tier"],
        "unknown"
    );
    assert!(s
        .set_media_source_override("group:11", hash, Some(Override::Public))
        .is_err());
}

#[test]
fn old_media_schema_migrates_conservatively_and_backfills_only_known_humans() {
    let t = Temp::new();
    let path = t.0.join("legacy.sqlite");
    let s = Store::open(&path).unwrap();
    // 固定旧版表形状，不从当前 schema 反推迁移输入。
    s.connection().execute_batch("CREATE TABLE media_assets(chat TEXT NOT NULL,hash TEXT NOT NULL,kind TEXT NOT NULL,file TEXT NOT NULL,occurrences INTEGER NOT NULL,first_seen REAL NOT NULL,last_seen REAL NOT NULL,bytes INTEGER NOT NULL,fitness TEXT NOT NULL DEFAULT '{}',PRIMARY KEY(chat,hash));
    CREATE TABLE media_contexts(chat TEXT NOT NULL,hash TEXT NOT NULL,message_id TEXT NOT NULL,role TEXT NOT NULL,PRIMARY KEY(chat,hash,message_id,role));
    INSERT INTO media_assets VALUES('group:10','legacy','image','media/group:10/legacy.bin',50,1,9,3,'{}');
    INSERT INTO media_assets VALUES('group:11','missing-history','image','media/group:11/old.bin',99,1,9,3,'{}');
    INSERT INTO messages VALUES('group:10','1','20','','',1,0),('group:10','2','20','','',2,0),('group:10','3','21','','',3,0),('group:10','4','99','','',4,1),('group:10','5','22','','',5,0);
    INSERT INTO media_contexts VALUES('group:10','legacy','1','usage'),('group:10','legacy','2','usage'),('group:10','legacy','3','usage'),('group:10','legacy','4','usage'),('group:10','legacy','5','before'),('group:10','legacy','deleted','usage');").unwrap();
    s.enable_media().unwrap();
    s.enable_media().unwrap();
    let row = &s.media_assets("group:10").unwrap()[0];
    assert_eq!(row["distinct_senders"], 2);
    assert_eq!(row["occurrences"], 50);
    assert_eq!(row["source_tier"], "unknown");
    assert_eq!(row["public_corpus_match"], 0);
    assert_eq!(row["source_override"], Value::Null);
    assert_eq!(
        s.media_assets("group:11").unwrap()[0]["distinct_senders"],
        0
    );
    drop(s);
    let s = Store::open(&path).unwrap();
    s.enable_media().unwrap();
    assert_eq!(count(&s, "media_senders"), 2);
    // 新库与迁移后的列名/默认值形状一致，重复启用不会再加列。
    assert_eq!(
        s.connection()
            .query_row(
                "SELECT count(*) FROM pragma_table_info('media_assets')",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        13
    );
}

#[test]
fn source_write_failure_rolls_back_sender_and_occurrence_together() {
    let t = Temp::new();
    let s = Store::in_memory().unwrap();
    let c = collector(&t);
    let p = t.0.join("input");
    fs::write(&p, "abc").unwrap();
    ingest(&c, &s, &event(1, image(p.to_str().unwrap())));
    s.connection().execute_batch("CREATE TRIGGER reject_source BEFORE UPDATE OF source_tier ON media_assets BEGIN SELECT RAISE(ABORT,'test failure'); END;").unwrap();
    let mut e = event(2, image(p.to_str().unwrap()));
    e["user_id"] = json!(21);
    assert_eq!(ingest(&c, &s, &e).failures.len(), 1);
    let row = &s.media_assets("group:10").unwrap()[0];
    assert_eq!(row["occurrences"], 1);
    assert_eq!(row["distinct_senders"], 1);
    assert_eq!(count(&s, "media_senders"), 1);
    assert_eq!(count(&s, "media_receipts"), 1);
    s.connection()
        .execute_batch("DROP TRIGGER reject_source")
        .unwrap();
    assert_eq!(ingest(&c, &s, &e).collected, 1);
    assert_eq!(
        s.media_assets("group:10").unwrap()[0]["distinct_senders"],
        2
    );
}
