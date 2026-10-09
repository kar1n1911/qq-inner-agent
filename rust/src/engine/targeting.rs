//! Validate untrusted ARTICULATE targets against this chat's latest 100 messages.
use super::policy::Hint;
use crate::store::Store;
use anyhow::Result;
use serde_json::Value;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Targets {
    pub reply_to: Option<String>,
    pub mention: Option<String>,
}

/// One shared 10% gate for optional noisy behavior; addressed replies bypass it.
pub fn validate(
    db: &Store,
    chat: &str,
    self_id: &str,
    response: &Value,
    addressed_id: Option<&str>,
    hint: Hint,
    draw: impl FnOnce() -> f64,
) -> Result<Targets> {
    let recent = db.rows(
        "SELECT id,sender,self FROM messages WHERE chat=? ORDER BY ts DESC,rowid DESC LIMIT 100",
        [chat],
    )?;
    let mut targets = Targets {
        reply_to: response["replyTo"]
            .as_str()
            .filter(|id| recent.iter().any(|m| m["id"].as_str() == Some(id)))
            .map(str::to_owned),
        mention: response["mention"]
            .as_str()
            .filter(|qq| {
                chat.starts_with("group:")
                    && *qq != self_id
                    && qq.starts_with(|c: char| ('1'..='9').contains(&c))
                    && qq.bytes().all(|b| b.is_ascii_digit())
                    && recent
                        .iter()
                        .any(|m| m["sender"].as_str() == Some(qq) && m["self"] == 0)
            })
            .map(str::to_owned),
    };
    if hint == Hint::SelfChat {
        // Trusted inbound trigger, retained separately from the latest message.
        targets.reply_to = addressed_id.map(str::to_owned);
    } else if (targets.reply_to.is_some() || targets.mention.is_some())
        && draw().partial_cmp(&0.1) != Some(std::cmp::Ordering::Less)
    {
        targets = Targets::default();
    }
    Ok(targets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn targets_are_scoped_recent_and_gated() -> Result<()> {
        let db = Store::in_memory()?;
        for (chat, id, sender, own) in [
            ("group:1", "a", "20", false),
            ("group:1", "b", "99", true),
            ("group:2", "foreign", "30", false),
            ("private:20", "p", "20", false),
        ] {
            db.message(&json!({"chat":chat,"id":id,"sender":sender,"name":"n","text":"hi","ts":1,"self":own}))?;
        }
        let check = |chat, response: Value, forced, hint, draw| {
            validate(&db, chat, "99", &response, forced, hint, || draw).unwrap()
        };
        for mention in ["99", "30", "all"] {
            assert_eq!(
                check(
                    "group:1",
                    json!({"replyTo":"foreign","mention":mention}),
                    None,
                    Hint::Open,
                    0.
                ),
                Targets::default()
            );
        }
        assert_eq!(
            check(
                "group:1",
                json!({"replyTo":"made-up"}),
                None,
                Hint::Open,
                0.
            ),
            Targets::default()
        );
        assert_eq!(
            check(
                "group:1",
                json!({"replyTo":"forged","mention":"20"}),
                None,
                Hint::Open,
                0.
            ),
            Targets {
                reply_to: None,
                mention: Some("20".into())
            }
        );
        assert_eq!(
            check(
                "group:1",
                json!({"replyTo":"a","mention":"99"}),
                None,
                Hint::Open,
                0.
            ),
            Targets {
                reply_to: Some("a".into()),
                mention: None
            }
        );
        let intent = json!({"replyTo":"a","mention":"20"});
        assert_eq!(
            check("group:1", intent.clone(), None, Hint::Open, 0.099),
            Targets {
                reply_to: Some("a".into()),
                mention: Some("20".into())
            }
        );
        for draw in [0.1, 0.5, 0.999] {
            assert_eq!(
                check("group:1", intent.clone(), None, Hint::Open, draw),
                Targets::default()
            );
        }
        assert_eq!(
            check("group:1", json!({}), Some("a"), Hint::SelfChat, 1.)
                .reply_to
                .as_deref(),
            Some("a")
        );
        assert_eq!(
            check(
                "private:20",
                json!({"replyTo":"p","mention":"20"}),
                None,
                Hint::Open,
                0.
            ),
            Targets {
                reply_to: Some("p".into()),
                mention: None
            }
        );
        for i in 0..100 {
            db.message(&json!({"chat":"group:1","id":format!("new{i}"),"sender":"40","text":"hi","ts":2,"self":false}))?;
        }
        assert_eq!(
            check("group:1", intent, None, Hint::Open, 0.),
            Targets::default()
        );
        Ok(())
    }
}
