//! JS 数据库原始行的固化金标准：直接 SQL 回放，独立于 Rust 的业务写入逻辑。
use qq_inner_core::store::Store;
use rusqlite::{params_from_iter, types::Value as SqlValue, Connection};
use serde_json::{json, Value};

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

pub fn insert(db: &Connection, tables: &Value) {
    for (table, rows) in tables.as_object().unwrap() {
        for row in rows.as_array().unwrap() {
            let row = row.as_object().unwrap();
            let columns: Vec<_> = row.keys().map(|key| quote(key)).collect();
            let placeholders = vec!["?"; columns.len()].join(",");
            let values: Vec<_> = row
                .values()
                .map(|v| match v {
                    Value::Null => SqlValue::Null,
                    Value::String(s) => SqlValue::Text(s.clone()),
                    Value::Number(n) => n
                        .as_i64()
                        .map(SqlValue::Integer)
                        .unwrap_or_else(|| SqlValue::Real(n.as_f64().unwrap())),
                    _ => panic!("不是 SQLite 原始标量: {v}"),
                })
                .collect();
            db.execute(
                &format!(
                    "INSERT INTO {} ({}) VALUES ({placeholders})",
                    quote(table),
                    columns.join(",")
                ),
                params_from_iter(values),
            )
            .unwrap();
        }
    }
}

fn normalized(table: &str, rows: &Value) -> Value {
    let mut rows = rows.clone();
    for row in rows.as_array_mut().unwrap() {
        // 随机 UUID 从未是原 parity 的契约；消息 id 和所有语义字段保留。
        for key in match table {
            "decisions" | "send_assessments" | "memory_layers" => &["id"][..],
            "memory_revisions" => &["memory_id"][..],
            _ => &[],
        } {
            row.as_object_mut().unwrap().remove(*key);
        }
        for key in [
            "sources",
            "keywords",
            "tags",
            "details",
            "forecast",
            "observation",
        ] {
            if let Some(value) = row.get_mut(key) {
                if !value.is_null() {
                    *value = serde_json::from_str(value.as_str().expect("落库 JSON 必须为文本"))
                        .unwrap();
                }
            }
        }
    }
    rows
}

fn equal(actual: &Value, expected: &Value) {
    match (actual, expected) {
        // SQLite REAL 与 JS JSON 整数字面量允许表示不同，但值必须精确相等。
        (Value::Number(a), Value::Number(b)) => assert_eq!(a.as_f64(), b.as_f64()),
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(a.len(), b.len());
            for (a, b) in a.iter().zip(b) {
                equal(a, b);
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            assert_eq!(a.keys().collect::<Vec<_>>(), b.keys().collect::<Vec<_>>());
            for (key, value) in a {
                equal(value, &b[key]);
            }
        }
        _ => assert_eq!(actual, expected),
    }
}

pub fn assert_rows(store: &Store, tables: &Value) {
    for (table, expected) in tables.as_object().unwrap() {
        let actual = json!(store
            .rows(
                &format!("SELECT * FROM {} ORDER BY rowid", quote(table)),
                []
            )
            .unwrap());
        equal(&normalized(table, &actual), &normalized(table, expected));
    }
}
