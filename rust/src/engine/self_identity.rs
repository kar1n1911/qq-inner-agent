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
        payload
    }
}
#[derive(Clone, serde::Serialize)]
struct OtherBot {
    qq: String,
    name: String,
}

// Independent from the login/card refresh so that it cannot overwrite a roster
// received concurrently through either existing persona member-list call.
#[derive(Default)]
struct BotSnapshot {
    qq: String,
    groups: HashMap<String, Vec<OtherBot>>,
}

#[derive(Default)]
struct Snapshot {
    qq: String,
    checked: Option<f64>,
    nickname: String,
    groups: HashMap<String, (String, String)>,
}
#[derive(Default)]
pub(super) struct Cache {
    snapshot: Mutex<Snapshot>,
    bots: Mutex<BotSnapshot>,
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
        // 实测：群 65840633 有一名成员的头衔就叫 bot，所以“群内bot”绝不能等于自己。
        // Only is_robot is evidence for this list; titles/names never classify robots.
        let Some(members) = members
            .as_array()
            .filter(|members| members.iter().any(|m| m["is_robot"].is_boolean()))
        else {
            // Unsupported ports omit is_robot: omit otherBots, including any older cache.
            bots.groups.remove(chat);
            return;
        };
        let others = members
            .iter()
            .filter(|m| m["is_robot"] == true)
            .filter_map(|m| {
                let id = match &m["user_id"] {
                    Value::String(id) if !id.trim().is_empty() => id.trim().to_owned(),
                    Value::Number(id) => id.to_string(),
                    _ => return None,
                };
                if id == qq {
                    return None;
                }
                let card = field(m, "card");
                let nickname = field(m, "nickname");
                let name = if !card.is_empty() {
                    card
                } else if !nickname.is_empty() {
                    nickname
                } else {
                    id.clone()
                };
                Some(OtherBot { qq: id, name })
            })
            .collect();
        bots.groups.insert(chat.into(), others);
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
            if let Some((card, nickname)) = s.groups.get(chat) {
                identity.card.clone_from(card);
                if !nickname.is_empty() {
                    identity.nickname.clone_from(nickname);
                }
            }
        }
        let bots = self.bots.lock().unwrap();
        if bots.qq == qq {
            identity.other_bots = bots.groups.get(chat).cloned();
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
            if let Ok(member) = transport
                .call(
                    "get_group_member_info",
                    json!({"group_id":group,"user_id":qq,"no_cache":true}),
                )
                .await
            {
                next.groups.insert(
                    format!("group:{group}"),
                    (field(&member, "card"), field(&member, "nickname")),
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
