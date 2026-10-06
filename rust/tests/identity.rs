use anyhow::Result;
use futures_util::future::BoxFuture;
use qq_inner_core::{
    config::{defaults, merge, Config, Identity},
    engine::{Engine, EngineTransport, Options},
    identity::{self, Proposal},
    onebot::{OneBotError, State},
    orientation::{OrientationProvider, OrientationTransport},
    store::Store,
};
use rusqlite::params;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn seed(db: &Store) {
    db.ensure_orientation("group:10", 100.).unwrap();
    for (i, subject, expires) in [
        (0, "group", None),
        (1, "group", None),
        (2, "group", None),
        (3, "person:20", None),
        (4, "group", Some(200.)),
    ] {
        db.execute("INSERT INTO memory_layers(id,chat,subject,layer,slot,text,sources,expires) VALUES(?,'group:10',?,'traits',?,?,?,?)", params![i.to_string(),subject,i.to_string(),format!("特质{i}"),if i==1 {"[1,2,3]"} else {"[1]"},expires]).unwrap();
    }
}
#[test]
fn thresholds_scope_proposal_and_persistence() {
    let db = Store::in_memory().unwrap();
    seed(&db);
    let cfg = Identity::default();
    assert!(!cfg.enabled && !cfg.allow_nickname && !cfg.allow_group_card && !cfg.allow_avatar);
    let now = 100. + 7. * 86400.;
    assert!(!identity::enough(&db, "group:10", now - 1., &cfg).unwrap());
    assert!(identity::enough(&db, "group:10", now, &cfg).unwrap());
    assert!(!identity::enough(&db, "private:10", now, &cfg).unwrap());
    assert!(!identity::enough(&db, "group:11", now, &cfg).unwrap());
    let strict = Identity {
        min_traits: 4,
        ..cfg
    };
    assert!(!identity::enough(&db, "group:10", now, &strict).unwrap());
    let p = identity::propose(&db, "group:10", "小助手", now).unwrap();
    assert_eq!(p.nickname, "小助手·特质1");
    assert_eq!(p.avatar, None);
    identity::save(&db, &p).unwrap();
    assert_eq!(identity::pending(&db).unwrap(), Some(p.clone()));
    identity::clear(&db, false).unwrap();
    assert!(!identity::applied(&db).unwrap());
    identity::save(&db, &p).unwrap();
    identity::clear(&db, true).unwrap();
    assert!(identity::pending(&db).unwrap().is_none());
    assert!(identity::applied(&db).unwrap());
    db.orientation_joined("group:10", now, now).unwrap();
    assert!(!identity::enough(&db, "group:10", now, &strict).unwrap());
}
#[derive(Default)]
struct Mock {
    calls: Mutex<Vec<(String, Value)>>,
    sends: Mutex<Vec<(String, String)>>,
    fail: Mutex<Option<String>>,
}
impl OrientationTransport for Mock {
    fn self_id(&self) -> String {
        "99".into()
    }
    fn call<'a>(&'a self, action: &'a str, params: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push((action.into(), params));
            anyhow::ensure!(
                self.fail.lock().unwrap().as_deref() != Some(action),
                "mock_failure"
            );
            Ok(json!({}))
        })
    }
}
impl OrientationProvider for Mock {
    fn json<'a>(&'a self, _: &'a str, _: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async { panic!("identity must not call model") })
    }
}
impl EngineTransport for Mock {
    fn state(&self) -> State {
        State {
            connected: true,
            online: true,
            self_id: "99".into(),
            reconnects: 0,
        }
    }
    fn send<'a>(
        &'a self,
        chat: &'a str,
        text: &'a str,
        _: Option<&'a str>,
    ) -> BoxFuture<'a, std::result::Result<Value, OneBotError>> {
        Box::pin(async move {
            self.sends.lock().unwrap().push((chat.into(), text.into()));
            Ok(json!({"message_id":1}))
        })
    }
}
fn setup(settings: Value) -> (Arc<Engine>, Arc<Mock>, Arc<Mutex<Store>>) {
    let db = Arc::new(Mutex::new(Store::in_memory().unwrap()));
    seed(&db.lock().unwrap());
    let mock = Arc::new(Mock::default());
    let cfg=Config::from_value(&merge(&defaults(),&json!({"apiKey":"","onebotToken":"","dataDir":"unused","agent":merge(&json!({"identity":{"enabled":true,"allowNickname":true,"allowGroupCard":true,"allowAvatar":true},"ownerTeaching":{"ownerUin":"20"},"allowedGroups":["10"],"allowedUsers":["20","21"],"persona":"小助手","dryRun":false,"schedule":{"enabled":false},"rhythm":{"enabled":false},"observation":{"enabled":false},"quietHours":null}),&settings)}))).unwrap();
    let engine = Engine::new(
        cfg,
        db.clone(),
        mock.clone(),
        mock.clone(),
        Options {
            now: Arc::new(|| 700000.),
            ..Options::default()
        },
    )
    .unwrap();
    (engine, mock, db)
}
fn event(id: i32, sender: i32, kind: &str, text: &str) -> Value {
    json!({"post_type":"message","message_type":kind,"self_id":99,"user_id":sender,"group_id":10,"message_id":id,"time":700000.,"sender":{"nickname":"主人"},"message":text})
}
async fn tick(e: &Arc<Engine>) {
    e.tick().unwrap();
    e.wait_idle().await;
}

#[tokio::test]
async fn proposes_once_authorizes_owner_and_applies_all_actions() {
    let (e, m, db) = setup(json!({}));
    tick(&e).await;
    assert_eq!(m.sends.lock().unwrap().len(), 1);
    assert_eq!(m.sends.lock().unwrap()[0].0, "private:20");
    assert!(m.sends.lock().unwrap()[0].1.contains("/同意改名 或 /忽略"));
    assert!(m.calls.lock().unwrap().is_empty());
    tick(&e).await;
    assert_eq!(m.sends.lock().unwrap().len(), 1);
    e.ingest(&event(1, 21, "private", "/同意改名")).unwrap();
    e.ingest(&event(2, 20, "group", "/同意改名")).unwrap();
    tick(&e).await;
    assert!(m.calls.lock().unwrap().is_empty());
    // 图片来源不在本任务范围；显式文件提案仍可走同一确认路径。
    let mut p = identity::pending(&db.lock().unwrap()).unwrap().unwrap();
    p.avatar = Some("file:///avatar.png".into());
    identity::save(&db.lock().unwrap(), &p).unwrap();
    e.ingest(&event(3, 20, "private", "/同意改名")).unwrap();
    e.ingest(&event(3, 20, "private", "/同意改名")).unwrap();
    tick(&e).await;
    let calls = m.calls.lock().unwrap().clone();
    assert_eq!(
        calls.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(),
        ["set_group_card", "set_qq_profile", "set_qq_avatar"]
    );
    assert_eq!(
        calls[0].1,
        json!({"group_id":"10","user_id":"99","card":p.group_card})
    );
    assert_eq!(calls[1].1, json!({"nickname":p.nickname}));
    assert_eq!(calls[2].1, json!({"file":"file:///avatar.png"}));
    assert!(identity::pending(&db.lock().unwrap()).unwrap().is_none());
    assert!(identity::applied(&db.lock().unwrap()).unwrap());
    tick(&e).await;
    assert_eq!(m.calls.lock().unwrap().len(), 3);
}
#[tokio::test]
async fn ignore_switches_and_partial_failure() {
    for settings in [
        json!({"identity":{"enabled":false}}),
        json!({"dryRun":true}),
    ] {
        let (e, m, _) = setup(settings);
        tick(&e).await;
        assert!(m.calls.lock().unwrap().is_empty());
        assert!(m.sends.lock().unwrap().is_empty());
    }
    let (e, m, db) = setup(json!({}));
    tick(&e).await;
    e.ingest(&event(1, 20, "private", "/忽略")).unwrap();
    tick(&e).await;
    assert!(identity::pending(&db.lock().unwrap()).unwrap().is_none());
    assert!(!identity::applied(&db.lock().unwrap()).unwrap());
    let p = Proposal {
        chat: "group:10".into(),
        nickname: "昵称".into(),
        group_card: "群名片".into(),
        avatar: None,
        completed: vec![],
    };
    identity::save(&db.lock().unwrap(), &p).unwrap();
    *m.fail.lock().unwrap() = Some("set_qq_profile".into());
    e.ingest(&event(2, 20, "private", "/同意改名")).unwrap();
    tick(&e).await;
    assert_eq!(
        identity::pending(&db.lock().unwrap())
            .unwrap()
            .unwrap()
            .completed,
        ["set_group_card"]
    );
    assert!(!identity::applied(&db.lock().unwrap()).unwrap());
    *m.fail.lock().unwrap() = None;
    tick(&e).await;
    assert_eq!(m.calls.lock().unwrap().len(), 2);
    e.ingest(&event(3, 20, "private", "/同意改名")).unwrap();
    tick(&e).await;
    assert_eq!(m.calls.lock().unwrap().len(), 3);
    assert_eq!(m.calls.lock().unwrap()[2].0, "set_qq_profile");
    assert!(identity::applied(&db.lock().unwrap()).unwrap());
}

#[tokio::test]
async fn permissions_are_independent_and_disabled_preserves_schema() {
    let (e, m, db) = setup(
        json!({"identity":{"allowNickname":false,"allowGroupCard":false,"allowAvatar":false}}),
    );
    tick(&e).await;
    assert!(identity::pending(&db.lock().unwrap()).unwrap().is_some());
    e.ingest(&event(1, 20, "private", "/同意改名")).unwrap();
    tick(&e).await;
    assert!(m.calls.lock().unwrap().is_empty());
    assert!(!identity::applied(&db.lock().unwrap()).unwrap());
    let (e, m, db) = setup(json!({"identity":{"allowNickname":false,"allowAvatar":false}}));
    tick(&e).await;
    e.ingest(&event(1, 20, "private", "/同意改名")).unwrap();
    tick(&e).await;
    assert_eq!(m.calls.lock().unwrap().len(), 1);
    assert_eq!(m.calls.lock().unwrap()[0].0, "set_group_card");
    assert!(identity::applied(&db.lock().unwrap()).unwrap());
    let (e, _, db) = setup(json!({"identity":{"enabled":false}}));
    tick(&e).await;
    assert!(db
        .lock()
        .unwrap()
        .rows(
            "SELECT name FROM sqlite_master WHERE name='identity_proposal'",
            []
        )
        .unwrap()
        .is_empty());
}

#[test]
fn identity_config_rejects_invalid_thresholds_and_accepts_partial_settings() {
    for settings in [
        json!({"minAgeDays":-1}),
        json!({"minTraits":1.5}),
        json!({"enabled":"yes"}),
    ] {
        assert!(
            Config::from_value(&merge(&defaults(), &json!({"agent":{"identity":settings}})))
                .is_err()
        );
    }
    let cfg=Config::from_value(&merge(&defaults(),&json!({"apiKey":"","onebotToken":"","dataDir":"unused","agent":{"identity":{"enabled":true}}}))).unwrap();
    assert_eq!(cfg.agent.identity.min_traits, 3);
    assert_eq!(cfg.agent.identity.min_age_days, 7.);
    assert!(!cfg.agent.identity.allow_nickname);
}
