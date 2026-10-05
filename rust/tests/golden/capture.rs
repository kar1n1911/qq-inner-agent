// 一次性捕获辅助：只记录当前 JS oracle 的输出，不从 Rust 实现生成期望值。
use serde_json::Value;
use std::{cell::Cell, fs, path::PathBuf};
thread_local! { static INDEX: Cell<usize> = const { Cell::new(0) }; }
pub fn record(input: Value, output: Value) -> Value {
    let index = INDEX.with(|n| { let i = n.get(); n.set(i + 1); i });
    let name = std::thread::current().name().unwrap().to_owned();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    fs::write(dir.join(format!("{name}-{index}.json")), serde_json::to_string_pretty(&serde_json::json!({"input":input,"expected":output})).unwrap()).unwrap();
    output
}
