use qq_inner_core::{affect, store::Store};
use serde_json::json;
pub fn store() -> Store {
    let s = Store::in_memory().unwrap();
    affect::enable(&s).unwrap();
    for (chat, id, sender, own) in [
        ("a", "1", "u", false),
        ("b", "1", "v", false),
        ("a", "2", "bot", true),
    ] {
        s.message(&json!({"chat":chat,"id":id,"sender":sender,"name":sender,"text":"约定明天","ts":100.,"self":own})).unwrap();
    }
    s
}
