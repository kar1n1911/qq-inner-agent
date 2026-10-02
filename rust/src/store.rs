//! 与 JS 共用 SQLite 文件；所有时间参数均为秒，REAL 保留小数秒。
use anyhow::{Context, Result};
use rusqlite::{types::ValueRef, Connection, Params};
use serde_json::{json, Map, Value};
use std::path::Path;

pub struct Store {
    db: Connection,
}
impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::initialize(Connection::open(path).context("open store")?)
    }
    pub fn in_memory() -> Result<Self> {
        Self::initialize(Connection::open_in_memory()?)
    }
    fn initialize(db: Connection) -> Result<Self> {
        db.set_prepared_statement_cache_capacity(64);
        db.execute_batch(include_str!("store/schema.sql"))?;
        let store = Self { db };
        // 先探测再 ALTER，兼容旧库与重复打开；触发器 DDL 集中在 schema.sql。
        for (table, column, definition) in [
            ("thoughts", "subject", "TEXT"),
            ("memory_layers", "keywords", "TEXT NOT NULL DEFAULT '[]'"),
            ("memory_layers", "confidence", "REAL NOT NULL DEFAULT 0.6"),
        ] {
            if !store
                .rows(&format!("PRAGMA table_info({table})"), [])?
                .iter()
                .any(|r| r["name"] == column)
            {
                store.db.execute_batch(&format!(
                    "ALTER TABLE {table} ADD COLUMN {column} {definition}"
                ))?;
            }
        }
        Ok(store)
    }
    /// 供后续共享连接的模块使用；查询应使用 prepare_cached。
    pub fn connection(&self) -> &Connection {
        &self.db
    }
    fn execute(&self, sql: &str, args: impl Params) -> Result<usize> {
        Ok(self.db.prepare_cached(sql)?.execute(args)?)
    }
    fn rows(&self, sql: &str, args: impl Params) -> Result<Vec<Value>> {
        let mut statement = self.db.prepare_cached(sql)?;
        let names: Vec<String> = statement
            .column_names()
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let mut rows = statement.query(args)?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            let mut object = Map::new();
            for (i, name) in names.iter().enumerate() {
                let v = match row.get_ref(i)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(n) => json!(n),
                    ValueRef::Real(n) => json!(n),
                    ValueRef::Text(s) => Value::String(std::str::from_utf8(s)?.to_owned()),
                    ValueRef::Blob(_) => anyhow::bail!("unexpected blob in store"),
                };
                object.insert(name.clone(), v);
            }
            result.push(Value::Object(object));
        }
        Ok(result)
    }
    /// 实际结构快照，包含自动索引、约束 SQL、列顺序和索引列顺序。
    pub fn schema(&self) -> Result<Value> {
        let objects = self.rows("SELECT type,name,tbl_name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name", [])?;
        let mut tables = Map::new();
        for row in &objects {
            if row["type"] != "table" {
                continue;
            }
            let name = row["name"].as_str().unwrap();
            let quoted = name.replace('"', "\"\"");
            let columns = self.rows(&format!("PRAGMA table_info(\"{quoted}\")"), [])?;
            let mut indexes = self.rows(&format!("PRAGMA index_list(\"{quoted}\")"), [])?;
            for index in &mut indexes {
                let key = index["name"].as_str().unwrap().replace('"', "\"\"");
                index["columns"] = json!(self.rows(&format!("PRAGMA index_info(\"{key}\")"), [])?);
                index.as_object_mut().unwrap().remove("seq");
            }
            indexes.sort_by_key(|i| i["name"].as_str().unwrap().to_owned());
            tables.insert(
                name.to_owned(),
                json!({"columns":columns,"indexes":indexes}),
            );
        }
        Ok(json!({"objects": objects,"tables":tables}))
    }
}

#[cfg(test)]
#[path = "store/tests.rs"]
mod tests;

#[path = "store/operations.rs"]
mod operations;
