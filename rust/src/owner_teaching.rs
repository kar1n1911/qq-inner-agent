//! 主人私聊教学：独立授权、形式校验与特殊来源，不构造虚假的人类证据。
use crate::{
    config::Agent,
    expression::ExpressionMemory,
    memory::{memory_subjects, valid_text, LayeredMemory},
    store::Store,
};
use anyhow::{ensure, Result};
use rusqlite::params;
use serde_json::{json, Value};

pub(crate) fn sources(v: &[Value]) -> bool {
    v == [json!("owner-teaching")]
}

/// None 必须继续普通聊天；Some 无论成功失败都消费该指令。
pub fn handle(
    store: &Store,
    agent: &Agent,
    chat: &str,
    sender: &str,
    input: &str,
    now: f64,
) -> Option<String> {
    // 授权边界：群聊无例外，私聊目标必须就是配置中的主人。
    if !agent.owner_teaching.enabled
        || sender != agent.owner_teaching.owner_uin
        || chat != format!("private:{sender}")
    {
        return None;
    }
    let input = input.trim();
    let (command, body) = ["/黑话", "/记住", "/忘记"]
        .iter()
        .find_map(|c| input.strip_prefix(c).map(|b| (*c, b.trim())))?;
    Some(
        match apply(store, agent, chat, sender, command, body, now) {
            Ok(()) => if command == "/忘记" {
                "忘记了"
            } else {
                "记住了"
            }
            .into(),
            Err(e) => format!("没看懂：{e}"),
        },
    )
}
fn checked(s: &str, max: usize) -> Result<()> {
    ensure!(
        valid_text(&json!(s), max),
        "内容不能为空，且不能超过 {max} 个 UTF-16 字符"
    );
    Ok(())
}
fn apply(
    store: &Store,
    a: &Agent,
    chat: &str,
    sender: &str,
    command: &str,
    body: &str,
    now: f64,
) -> Result<()> {
    let subject = memory_subjects(chat, sender)?.remove(0);
    // 绕过 8 条/300 秒门控；§16 尚未实现，复用 memory 的文本与 subject 形式校验兜底。
    // 来源永远只有此特殊标记，绝不把指令 message_id 当成人类学习证据。
    let source = json!(["owner-teaching"]);
    let tx = store.immediate()?;
    match command {
        "/黑话" => {
            let (term, meaning) = body
                .split_once('=')
                .ok_or_else(|| anyhow::anyhow!("黑话格式应为 /黑话 词 = 意思"))?;
            let (term, meaning) = (term.trim(), meaning.trim());
            checked(term, 40)?;
            checked(meaning, 160)?;
            ExpressionMemory::new(store).apply(chat, &[json!({"subject":subject,"kind":"jargon","term":term,"meaning":meaning,"situation":"主人私聊教学","example":term,"confidence":1.0,"sources":source})], now, &a.expression)?;
        }
        "/记住" => {
            checked(body, 500usize.min(a.memory.long_chars as usize))?;
            LayeredMemory::new(store).apply(chat, &[json!({"subject":subject,"layer":"long_term","operation":"upsert","key":format!("owner-teaching:{}", crate::store::uuid()),"text":body,"importance":0.8,"confidence":1.0,"keywords":[],"sources":source})], now, &a.memory)?;
        }
        _ => {
            checked(body, 500)?;
            // 参数化的字面子串匹配，%/_ 不会变成通配符；不触碰其他人的记忆。
            store.execute("DELETE FROM memory_revisions WHERE memory_id IN (SELECT id FROM memory_layers WHERE chat=? AND subject=? AND layer IN ('long_term','traits') AND (instr(text,?)>0 OR instr(slot,?)>0))", params![chat,subject,body,body])?;
            store.execute("DELETE FROM memory_layers WHERE chat=? AND subject=? AND layer IN ('long_term','traits') AND (instr(text,?)>0 OR instr(slot,?)>0)", params![chat,subject,body,body])?;
            store.execute("DELETE FROM expressions WHERE chat=? AND subject=? AND (instr(term,?)>0 OR instr(meaning,?)>0)", params![chat,subject,body,body])?;
            store.execute(
                "DELETE FROM learned_memories WHERE chat=? AND instr(text,?)>0",
                params![chat, body],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}
