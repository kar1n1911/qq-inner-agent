//! Read-only account identity, separate from persona-driven account mutations.
use super::{Engine, IdentityAdapter};
use crate::engine::orientation::OrientationTransport;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Mutex};

#[derive(Clone, Default)]
pub(super) struct Identity {
    pub qq: String,
    pub nickname: String,
    pub card: String,
    other_bots: Option<Vec<OtherBot>>,
    pub member: Member,
}
impl Identity {
    pub fn visible_name<'a>(&'a self, fallback: &'a str) -> &'a str {
        [&self.card, &self.nickname]
            .into_iter()
            .find(|name| !name.trim().is_empty())
            .map_or(fallback, String::as_str)
    }
    pub fn payload(&self, fallback: &str) -> Value {
        let mut payload = json!({"qq":self.qq,"nickname":self.nickname,"groupCard":self.card,
            "visibleName":self.visible_name(fallback),
            "instructions":format!("只有证据明确指向你这个账号时，群友讨论的那个机器人或 bot 才是你。判据包括：①消息里 @ 了你的 QQ（[@{}]）；②同一话题下有你自己发出的消息；③称呼命中你的群名片或昵称；④内容明确指向你的自身属性。群里可能有多个机器人，也可能有其他 bot；otherBots（若提供）列出的是本群其他机器人账号，不是你；仅出现“机器人/bot”字样，或称呼可能指别人时，不等于你，不要以第一人称谈论别人的事。确认指向你时，不要以第三方身份谈论自己。", self.qq)});
        if let Some(bots) = &self.other_bots {
            payload["otherBots"] = json!(bots);
        }
        self.member.annotate(&mut payload);
        payload
    }
}
#[derive(Clone, serde::Serialize)]
struct OtherBot {
    qq: String,
    name: String,
}

/// Public roster metadata only; never stored in person memory or learning evidence.
#[derive(Clone, Default, serde::Serialize)]
pub(super) struct Member {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    level: Option<Value>,
}
impl Member {
    fn parse(value: &Value) -> Self {
        Self {
            role: value["role"]
                .as_str()
                .filter(|s| matches!(*s, "owner" | "admin" | "member"))
                .map(str::to_owned),
            title: value["title"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            level: match &value["level"] {
                Value::String(s) if !s.trim().is_empty() => Some(json!(s.trim())),
                Value::Number(n) => Some(json!(n)),
                _ => None,
            },
        }
    }
    pub fn annotate(&self, value: &mut Value) {
        if let Some(role) = &self.role {
            value["role"] = json!(role);
        }
        if let Some(title) = &self.title {
            value["title"] = json!(title);
        }
    }
}
#[derive(Default)]
struct Roster {
    members: HashMap<String, Member>,
    other_bots: Option<Vec<OtherBot>>,
}
#[derive(Default)]
struct BotSnapshot {
    qq: String,
    groups: HashMap<String, Roster>,
}
#[derive(Default)]
struct RosterReads {
    qq: String,
    groups: HashMap<String, (f64, Option<Value>)>,
}

pub(super) fn titled_members(members: &Value) -> Vec<Value> {
    members
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let member = Member::parse(m);
            member.title.as_ref()?;
            let id = member_id(m)?;
            let mut value = json!({"qq":id, "name":member_name(m, &id)});
            member.annotate(&mut value);
            Some(value)
        })
        .take(40)
        .collect()
}
fn member_id(m: &Value) -> Option<String> {
    match &m["user_id"] {
        Value::String(id) if !id.trim().is_empty() => Some(id.trim().into()),
        Value::Number(id) => Some(id.to_string()),
        _ => None,
    }
}
fn member_name(m: &Value, id: &str) -> String {
    [field(m, "card"), field(m, "nickname")]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or_else(|| id.into())
}

#[derive(Default)]
struct Snapshot {
    qq: String,
    checked: Option<f64>,
    nickname: String,
    groups: HashMap<String, (String, String, Member)>,
}
#[derive(Default)]
pub(super) struct Cache {
    snapshot: Mutex<Snapshot>,
    bots: Mutex<BotSnapshot>,
    roster_reads: tokio::sync::Mutex<RosterReads>,
    refreshing: tokio::sync::Mutex<()>,
}
impl Cache {
    pub fn observe_members(&self, qq: &str, chat: &str, members: &Value) {
        let mut bots = self.bots.lock().unwrap();
        if bots.qq != qq {
            *bots = BotSnapshot {
                qq: qq.into(),
                ..BotSnapshot::default()
            };
        }
        let rows = members.as_array().map(Vec::as_slice).unwrap_or_default();
        let roster = Roster {
            members: rows
                .iter()
                .filter_map(|m| Some((member_id(m)?, Member::parse(m))))
                .collect(),
            // A title named "bot" is not evidence that the member is a robot.
            other_bots: rows.iter().any(|m| m["is_robot"].is_boolean()).then(|| {
                rows.iter()
                    .filter(|m| m["is_robot"] == true)
                    .filter_map(|m| {
                        let id = member_id(m)?;
                        (id != qq).then(|| OtherBot {
                            name: member_name(m, &id),
                            qq: id,
                        })
                    })
                    .collect()
            }),
        };
        bots.groups.insert(chat.into(), roster);
    }

    pub fn member(&self, qq: &str, chat: &str, sender: &str) -> Member {
        let bots = self.bots.lock().unwrap();
        if bots.qq != qq {
            return Member::default();
        }
        bots.groups
            .get(chat)
            .and_then(|r| r.members.get(sender))
            .cloned()
            .unwrap_or_default()
    }

    /// One shared 300s refresh for all consumers, including concurrent calls.
    /// Cache failures too so unsupported ports are not retried within the same tick.
    pub async fn members<T: OrientationTransport + ?Sized>(
        &self,
        transport: &T,
        group: &str,
        now: f64,
    ) -> anyhow::Result<Value> {
        let qq = transport.self_id();
        let mut reads = self.roster_reads.lock().await;
        if reads.qq != qq {
            *reads = RosterReads {
                qq: qq.clone(),
                ..Default::default()
            };
        }
        if let Some((checked, value)) = reads.groups.get(group) {
            if now >= *checked && now - checked < 300. {
                return value
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("members_unavailable"));
            }
        }
        let result = transport
            .call(
                "get_group_member_list",
                json!({"group_id":group,"no_cache":true}),
            )
            .await;
        anyhow::ensure!(transport.self_id() == qq, "identity_account_changed");
        let value = result.ok().filter(Value::is_array);
        self.observe_members(
            &qq,
            &format!("group:{group}"),
            value.as_ref().unwrap_or(&Value::Null),
        );
        reads.groups.insert(group.into(), (now, value.clone()));
        value.ok_or_else(|| anyhow::anyhow!("members_unavailable"))
    }

    pub fn due(&self, qq: &str, now: f64) -> bool {
        let s = self.snapshot.lock().unwrap();
        s.qq != qq || s.checked.is_none_or(|t| now < t || now - t >= 300.)
    }
    pub fn identity(&self, qq: &str, chat: &str) -> Identity {
        let s = self.snapshot.lock().unwrap();
        let mut identity = Identity {
            qq: qq.into(),
            ..Identity::default()
        };
        if s.qq == qq {
            identity.nickname.clone_from(&s.nickname);
            if let Some((card, nickname, member)) = s.groups.get(chat) {
                identity.card.clone_from(card);
                identity.member = member.clone();
                if !nickname.is_empty() {
                    identity.nickname.clone_from(nickname);
                }
            }
        }
        let bots = self.bots.lock().unwrap();
        if bots.qq == qq {
            if let Some(roster) = bots.groups.get(chat) {
                identity.other_bots = roster.other_bots.clone();
                if let Some(member) = roster.members.get(qq) {
                    identity.member.role = member.role.clone().or(identity.member.role);
                    identity.member.title = member.title.clone().or(identity.member.title);
                    identity.member.level = member.level.clone().or(identity.member.level);
                }
            }
        }
        identity
    }
    pub async fn refresh(&self, engine: &Engine) {
        let _guard = self.refreshing.lock().await;
        let qq = engine.transport.self_id();
        let now = engine.now();
        if qq.is_empty() || !self.due(&qq, now) {
            return;
        }
        let transport = IdentityAdapter(engine);
        let mut next = Snapshot {
            qq: qq.clone(),
            checked: Some(now),
            ..Snapshot::default()
        };
        if let Ok(login) = transport.call("get_login_info", json!({})).await {
            next.nickname = field(&login, "nickname");
        }
        for group in &engine.config.agent.allowed_groups {
            let _ = transport
                .call(
                    "get_group_member_list",
                    json!({"group_id":group,"no_cache":true}),
                )
                .await;
            if let Ok(member) = transport
                .call(
                    "get_group_member_info",
                    json!({"group_id":group,"user_id":qq,"no_cache":true}),
                )
                .await
            {
                next.groups.insert(
                    format!("group:{group}"),
                    (
                        field(&member, "card"),
                        field(&member, "nickname"),
                        Member::parse(&member),
                    ),
                );
            }
        }
        // Do not publish data from an account that switched during I/O.
        if engine.transport.self_id() == qq {
            *self.snapshot.lock().unwrap() = next;
        }
    }
}
fn field(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_metadata_is_optional_scoped_and_bounded() {
        let cache = Cache::default();
        let rows = json!([
            {"user_id":99,"role":"member","title":"bot","level":"100"},
            {"user_id":20,"role":"owner","title":"总督","level":100},
            {"user_id":21,"role":"invalid","title":"  ","level":null}
        ]);
        cache.observe_members("99", "group:10", &rows);
        let payload = cache.identity("99", "group:10").payload("");
        assert_eq!(payload["role"], "member");
        assert_eq!(payload["title"], "bot");
        assert!(payload.get("otherBots").is_none());
        assert_eq!(
            serde_json::to_value(cache.member("99", "group:10", "20")).unwrap(),
            json!({"role":"owner","title":"总督","level":100})
        );
        assert_eq!(
            cache.member("99", "group:10", "99").level,
            Some(json!("100"))
        );
        for (account, chat, sender) in [
            ("99", "group:11", "20"),
            ("100", "group:10", "20"),
            ("99", "group:10", "21"),
        ] {
            assert_eq!(
                serde_json::to_value(cache.member(account, chat, sender)).unwrap(),
                json!({})
            );
        }
        let many = json!((0..60)
            .map(|n| json!({"user_id":n,"title":"总督"}))
            .collect::<Vec<_>>());
        assert_eq!(titled_members(&many).len(), 40);
        assert_eq!(titled_members(&rows).len(), 2);
        cache.observe_members("99", "group:10", &json!([{"user_id":99}]));
        let payload = cache.identity("99", "group:10").payload("");
        assert!(payload.get("role").is_none());
        assert!(payload.get("title").is_none());
        assert!(titled_members(&json!([{"user_id":99}])).is_empty());
    }

    #[tokio::test]
    async fn roster_refresh_coalesces_concurrent_reads_and_caches_failures() {
        struct Transport(std::sync::atomic::AtomicUsize);
        impl OrientationTransport for Transport {
            fn self_id(&self) -> String {
                "99".into()
            }
            fn call<'a>(
                &'a self,
                action: &'a str,
                _: Value,
            ) -> futures_util::future::BoxFuture<'a, anyhow::Result<Value>> {
                Box::pin(async move {
                    assert_eq!(action, "get_group_member_list");
                    let n = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    anyhow::ensure!(n == 0, "unsupported");
                    Ok(json!([{"user_id":99,"role":"admin","title":"奶淇琳","is_robot":true}]))
                })
            }
        }
        let cache = Cache::default();
        let transport = Transport(Default::default());
        let (a, b) = tokio::join!(
            cache.members(&transport, "10", 100.),
            cache.members(&transport, "10", 100.)
        );
        assert_eq!(a.unwrap(), b.unwrap());
        assert_eq!(transport.0.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(cache.members(&transport, "10", 399.).await.is_ok());
        assert!(cache.members(&transport, "10", 400.).await.is_err());
        assert!(cache.members(&transport, "10", 400.).await.is_err());
        assert_eq!(transport.0.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(cache
            .identity("99", "group:10")
            .payload("")
            .get("title")
            .is_none());
        assert!(cache.members(&transport, "10", 99.).await.is_err());
        assert_eq!(transport.0.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn other_bots_are_explicit_group_scoped_and_exclude_self() {
        let cache = Cache::default();
        cache.observe_members(
            "99",
            "group:65840633",
            &json!([
                {"user_id":99,"card":"丹德莱","is_robot":true},
                {"user_id":20,"title":"bot","nickname":"Human","is_robot":false},
                {"user_id":21,"nickname":"bot"},
                {"user_id":22,"card":"  其他机器人  ","nickname":"Other","is_robot":true},
                {"user_id":"23","card":" ","nickname":"Nickname","is_robot":true},
                {"user_id":24,"is_robot":true},
                {"is_robot":true}
            ]),
        );
        let payload = cache.identity("99", "group:65840633").payload("fallback");
        assert_eq!(
            payload["otherBots"],
            json!([
                {"qq":"22","name":"其他机器人"},
                {"qq":"23","name":"Nickname"},
                {"qq":"24","name":"24"}
            ])
        );
        assert!(cache
            .identity("99", "group:11")
            .payload("")
            .get("otherBots")
            .is_none());
        assert!(cache
            .identity("100", "group:65840633")
            .payload("")
            .get("otherBots")
            .is_none());
        // Login/card publication must not discard the independently observed roster.
        *cache.snapshot.lock().unwrap() = Snapshot {
            qq: "99".into(),
            ..Snapshot::default()
        };
        assert_eq!(
            cache.identity("99", "group:65840633").payload("")["otherBots"],
            payload["otherBots"]
        );
    }

    #[test]
    fn unsupported_rosters_remove_stale_bots_without_errors() {
        let cache = Cache::default();
        for unsupported in [
            json!([{"user_id":20,"title":"bot"}]),
            json!([]),
            Value::Null,
        ] {
            cache.observe_members("99", "group:10", &json!([{"user_id":20,"is_robot":true}]));
            cache.observe_members("99", "group:10", &unsupported);
            assert!(cache
                .identity("99", "group:10")
                .payload("")
                .get("otherBots")
                .is_none());
        }
        cache.observe_members("99", "group:10", &json!([{"user_id":20,"is_robot":false}]));
        assert_eq!(
            cache.identity("99", "group:10").payload("")["otherBots"],
            json!([])
        );
        cache.observe_members("100", "group:11", &json!([{"user_id":20,"is_robot":true}]));
        assert!(cache
            .identity("99", "group:10")
            .payload("")
            .get("otherBots")
            .is_none());
    }
}
