//! P6c 门控新功能；长度分档属于 engine 中的 parity 接线，不在这里重新实现。
use crate::{
    engine::policy::Hint,
    memory::{num, text},
    store::Store,
};
use anyhow::Result;
use serde_json::Value;

/// 追加到 articulation user 内容的运行时片段，默认值由 prompts.mjs 生成。
pub use crate::prompts::FACE_ONLY_INSTRUCTIONS as FACE_ONLY_INSTRUCTIONS;
/// 多气泡（门控）：允许模型额外返回 `bubbles` 数组模拟连续多气泡。
pub use crate::prompts::MULTI_BUBBLE_INSTRUCTIONS as MULTI_BUBBLE_INSTRUCTIONS;

/// 原始段标记单独保存，避免把用户输入的字面量 [QQface:...] 误当真实 face。
/// 只在学习开关开启后采集；旧消息没有段证据，按冷启动处理，不猜测回填。
pub fn enable(store: &Store) -> Result<()> {
    store.connection().execute_batch(
        "CREATE TABLE IF NOT EXISTS humanize_reply_state(
            chat TEXT PRIMARY KEY, face_only INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS humanize_faces(
            chat TEXT NOT NULL, id TEXT NOT NULL, has_face INTEGER NOT NULL,
            PRIMARY KEY(chat,id));
         CREATE TRIGGER IF NOT EXISTS humanize_faces_cleanup AFTER DELETE ON messages
         BEGIN DELETE FROM humanize_faces WHERE chat=OLD.chat AND id=OLD.id; END;",
    )?;
    Ok(())
}

pub fn capture(store: &Store, chat: &str, id: &str, event: &Value) -> Result<()> {
    if !chat.starts_with("group:") {
        return Ok(());
    }
    let valid_id = |s: &str| !s.is_empty() && s.len() <= 5 && s.bytes().all(|b| b.is_ascii_digit());
    let has_face = if let Some(segments) = event["message"].as_array() {
        segments
            .iter()
            .any(|s| s["type"] == "face" && valid_id(&crate::config::js_string(&s["data"]["id"])))
    } else {
        // CQ 字符串中仅接受真实未转义段，解码后的字面量不算段。
        event["message"].as_str().is_some_and(|s| {
            s.split("[CQ:face,id=")
                .skip(1)
                .any(|tail| tail.split_once(']').is_some_and(|(id, _)| valid_id(id)))
        })
    };
    store.execute(
        "INSERT OR IGNORE INTO humanize_faces VALUES(?,?,?)",
        rusqlite::params![chat, id, has_face],
    )?;
    Ok(())
}

/// 30 天窗口、7 天半衰期；有效样本不足 5 条时回到保守冷启动。
/// 从持久化 messages 读取，因此重启不重置统计，删除消息时证据随之清理。
pub fn face_probability(store: &Store, chat: &str, now: f64) -> Result<f64> {
    let rows = store.rows(
        "SELECT m.ts,f.has_face FROM messages m JOIN humanize_faces f
         ON m.chat=f.chat AND m.id=f.id
         WHERE m.chat=? AND m.self=0 AND m.ts>=? AND m.ts<=?",
        rusqlite::params![chat, now - 30. * 86400., now],
    )?;
    let (mut total, mut faces) = (0., 0.);
    for row in rows {
        let weight = 2_f64.powf(-(now - num(&row, "ts")) / (7. * 86400.));
        total += weight;
        faces += weight * num(&row, "has_face");
    }
    Ok(if total < 5. {
        0.08
    } else {
        (faces / total * 0.8).clamp(0., 0.35)
    })
}

/// 求助/情绪理解尚无可靠分类器：采用保守正向白名单，只放行纯附和/笑声。
/// 未知、混合内容一律需要正文，而不是靠几个负向关键词声称识别所有难过。
fn light_reaction(content: &str) -> bool {
    let mut rest = content.trim();
    // 显式 other 可以带 @ 段；只跳过已归一化的数字 QQ 标记。
    while let Some(tail) = rest.strip_prefix("[@") {
        let Some((id, tail)) = tail.split_once(']') else {
            return false;
        };
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        rest = tail.trim();
    }
    let rest = rest.trim_matches(|c: char| c.is_whitespace() || "!！。~～".contains(c));
    matches!(
        rest,
        "确实"
            | "确实如此"
            | "同感"
            | "赞同"
            | "笑死"
            | "笑死了"
            | "好好笑"
            | "哈哈"
            | "哈哈哈"
            | "哈哈哈哈"
            | "233"
            | "2333"
            | "lol"
            | "LOL"
    )
}

pub fn face_only_allowed(
    store: &Store,
    chat: &str,
    hint: Hint,
    motivation: f64,
    history: &[Value],
    now: f64,
    cooldown: f64,
) -> Result<bool> {
    if !matches!(hint, Hint::Open | Hint::Other) || !(1. ..=3.).contains(&motivation) {
        return Ok(false);
    }
    // 独立状态不随 messages 保留期丢失；投递前置位，崩溃/不确定送达仍禁止连发。
    if store
        .first(
            "SELECT face_only FROM humanize_reply_state WHERE chat=?",
            [chat],
        )?
        .is_some_and(|r| r["face_only"] == 1)
    {
        return Ok(false);
    }
    // 数据库检查跨重启有效；空正文的其它素材也保守视作无实质回答。
    if let Some(last) = store.first(
        "SELECT text,ts FROM messages WHERE chat=? AND self=1 ORDER BY ts DESC,rowid DESC LIMIT 1",
        [chat],
    )? {
        if text(&last, "text").trim().is_empty() || now - num(&last, "ts") < cooldown.max(30.) {
            return Ok(false);
        }
    }
    // 不确定送达有可能已经发出 face，直到下一次确定发送前不再尝试只发表情。
    if store
        .first(
            "SELECT status FROM deliveries WHERE chat=? ORDER BY ts DESC,rowid DESC LIMIT 1",
            [chat],
        )?
        .is_some_and(|last| matches!(text(&last, "status"), "pending" | "uncertain"))
    {
        return Ok(false);
    }
    let start = history
        .iter()
        .rposition(|m| m["self"] == true || m["self"] == 1)
        .map_or(0, |i| i + 1);
    let humans: Vec<_> = history[start..]
        .iter()
        .filter(|m| m["self"] != true && m["self"] != 1)
        .collect();
    Ok(!humans.is_empty() && humans.iter().all(|m| light_reaction(text(m, "text"))))
}
