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
}
impl Identity {
    pub fn visible_name<'a>(&'a self, fallback: &'a str) -> &'a str {
        [&self.card, &self.nickname]
            .into_iter()
            .find(|name| !name.trim().is_empty())
            .map_or(fallback, String::as_str)
    }
    pub fn payload(&self, fallback: &str) -> Value {
        json!({"qq":self.qq,"nickname":self.nickname,"groupCard":self.card,
            "visibleName":self.visible_name(fallback),
            "instructions":"群友讨论的那个机器人就是你,不要以第三方身份谈论自己"})
    }
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
    refreshing: tokio::sync::Mutex<()>,
}
impl Cache {
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
