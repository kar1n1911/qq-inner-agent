//! 当前 JS oracle 输出的固化金标准；运行时只读 JSON，不启动外部解释器。
//! 同时校验输入，新增/改变用例而未更新金标准时直接失败，绝不跳过。
use serde_json::Value;

pub fn expected(source: &str, input: &Value) -> Value {
    let cases: Vec<Value> = serde_json::from_str(source).expect("有效的固化金标准 JSON");
    cases
        .iter()
        .find(|case| &case["input"] == input)
        .unwrap_or_else(|| panic!("输入没有对应的固化金标准: {input}"))["expected"]
        .clone()
}
