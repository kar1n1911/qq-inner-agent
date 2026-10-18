//! 显式启用的 Rust 入站采集入口；默认 Store 和消息写入契约保持不变。
pub mod media_select;
pub mod media_source;
pub mod ocr;

use crate::{
    config::{js_string, Agent},
    engine::io::types::{MediaSrc, Segment},
    engine::policy,
    media::media_source::{Evidence, Override},
    persona::{affect, conversation},
    settings::sha256,
    store::Store,
};
use anyhow::{ensure, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub timeout_seconds: u64,
    pub classification: conversation::Config,
    pub public_corpus_dir: Option<PathBuf>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            max_file_bytes: 2 * 1024 * 1024,
            max_total_bytes: 100 * 1024 * 1024,
            timeout_seconds: 15,
            classification: Default::default(),
            public_corpus_dir: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct Report {
    pub collected: usize,
    pub failures: Vec<String>,
}

pub struct Collector {
    root: PathBuf,
    config: Config,
    public_hashes: HashSet<String>,
}
impl Collector {
    pub fn new(root: impl AsRef<Path>, config: Config) -> Self {
        // 本地公开语料在构造时取快照；不可读时保守视为空，绝不回退到网络查询。
        let public_hashes = if config.enabled {
            config
                .public_corpus_dir
                .as_ref()
                .and_then(|dir| media_source::corpus_hashes(dir, config.max_file_bytes).ok())
                .unwrap_or_default()
        } else {
            HashSet::new()
        };
        Self {
            public_hashes,
            root: root.as_ref().into(),
            config,
        }
    }
    /// 同步 I/O；运行时应在专用阻塞工作线程调用。复用 normalize 的身份、白名单与时效检查。
    pub fn ingest(
        &self,
        store: &Store,
        event: &Value,
        self_id: &str,
        agent: &Agent,
        now: f64,
    ) -> Result<Report> {
        if !self.config.enabled {
            return Ok(Report::default());
        }
        // 只采人类，排除自己的发送回显，避免自己学自己产生自我强化。
        let Some(m) = policy::normalize(event, self_id, agent, now) else {
            return Ok(Report::default());
        };
        ensure!(
            m.chat.split_once(':').is_some_and(|(_, id)|
                !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())),
            "invalid_media_chat"
        );
        store.enable_media()?;
        let (_, key) = store.record_message(&serde_json::to_value(&m)?)?;
        self.ingest_segments(store, &m, &key, &typed_segments(event), now)
    }
    pub fn ingest_segments(&self, store: &Store, m: &policy::Message, key: &str,
        segments: &[Segment], now: f64) -> Result<Report> {
        let mut report = Report::default();
        if !self.config.enabled { return Ok(report); }
        ensure!(
            m.chat
                .split_once(':')
                .is_some_and(|(_, id)| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())),
            "invalid_media_chat"
        );
        store.enable_media()?;
        for (index, segment) in segments.iter().enumerate() {
            if !matches!(segment, Segment::Image { .. } | Segment::Face { .. }) {
                continue;
            }
            match self.collect(store, m, key, segment, index) {
                Ok(true) => report.collected += 1,
                Ok(false) => {}
                // 不记录可能含凭据的临时 URL；错误码和消息 id 足以定位失败。
                Err(_) => report
                    .failures
                    .push(format!("media_collect_failed:{}:{index}", m.id)),
            }
        }
        // 后续消息也仅存 id；已有使用场景可随新证据重新分类，不产生学习更新。
        store.execute("INSERT OR IGNORE INTO media_contexts(chat,hash,message_id,role,valence,rationality) SELECT c.chat,c.hash,?,'after',c.valence,c.rationality FROM media_contexts c JOIN messages previous ON previous.chat=c.chat AND previous.message_key=c.message_id WHERE c.chat=? AND c.role='usage' AND c.message_id<>? AND previous.ts<=? AND previous.ts>=?", params![key,m.chat,key,m.ts,m.ts-self.config.classification.gap_seconds])?;
        for row in store.rows("SELECT message_id FROM media_stages WHERE chat=? AND message_id IN (SELECT message_key FROM messages WHERE chat=? ORDER BY ts DESC,rowid DESC LIMIT 24)",params![m.chat,m.chat])? {
            store.record_media_stage(&m.chat,row["message_id"].as_str().unwrap(),now,&self.config.classification)?;
        }
        store.record_media_stage(&m.chat, key, now, &self.config.classification)?;
        Ok(report)
    }
    fn collect(&self, store: &Store, m: &policy::Message, key: &str, s: &Segment, index: usize) -> Result<bool> {
        let tx = store.immediate()?;
        if store
            .first(
                "SELECT 1 FROM media_receipts WHERE chat=? AND message_id=? AND segment=?",
                params![m.chat, key, index as i64],
            )?
            .is_some()
        {
            return Ok(false);
        }
        let kind = if matches!(s, Segment::Face { .. }) { "face" } else { "image" };
        let (hash, file, bytes, content) = if let Segment::Face { id } = s {
            ensure!(
                !id.is_empty() && id.len() <= 5 && id.bytes().all(|b| b.is_ascii_digit()),
                "invalid_face"
            );
            (sha256(format!("face:{id}").as_bytes()), id.clone(), 0, None)
        } else {
            let Segment::Image { src, .. } = s else { unreachable!() };
            let data = read_image_source(src, self.config.max_file_bytes, self.config.timeout_seconds)?;
            let hash = sha256(&data);
            let ext = if data.starts_with(b"\x89PNG\r\n\x1a\n") {
                "png"
            } else if data.starts_with(b"\xff\xd8\xff") {
                "jpg"
            } else if data.starts_with(b"GIF8") {
                "gif"
            } else if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
                "webp"
            } else {
                "bin"
            };
            (
                hash.clone(),
                format!("media/{}/{hash}.{ext}", m.chat),
                data.len() as u64,
                Some(data),
            )
        };
        let existing = store.first(
            "SELECT file FROM media_assets WHERE chat=? AND hash=?",
            params![m.chat, hash],
        )?;
        let mut written = None;
        if existing.is_none() {
            let total = store
                .first(
                    "SELECT coalesce(sum(bytes),0) AS total FROM media_assets",
                    [],
                )?
                .unwrap()["total"]
                .as_u64()
                .unwrap();
            // 容量不足按低频、旧使用优先淘汰；整个索引更新在同一事务内。
            let mut remaining = total;
            let mut victims = Vec::new();
            ensure!(bytes <= self.config.max_total_bytes, "media_capacity");
            for row in store.rows("SELECT chat,hash,file,bytes FROM media_assets WHERE kind='image' ORDER BY occurrences,last_seen,chat,hash", [])? {
                if remaining.saturating_add(bytes) <= self.config.max_total_bytes { break; }
                remaining = remaining.saturating_sub(row["bytes"].as_u64().unwrap());
                victims.push(row);
            }
            if let Some(data) = content {
                let dir = self.root.join("media").join(&m.chat);
                private_dir(&self.root.join("media"))?;
                private_dir(&dir)?;
                let path = self.root.join(&file);
                let tmp = dir.join(format!(".{}.tmp", crate::store::uuid()));
                let result = (|| -> Result<()> {
                    let mut out = fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&tmp)?;
                    out.write_all(&data)?;
                    out.sync_all()?;
                    fs::rename(&tmp, &path)?;
                    Ok(())
                })();
                if result.is_err() {
                    let _ = fs::remove_file(&tmp);
                }
                result?;
                written = Some(path);
            }
            // 文件删除在提交后执行，回滚不能破坏旧索引所引用的字节。
            let result = (|| -> Result<()> {
                for v in &victims {
                    store.execute(
                        "DELETE FROM media_contexts WHERE chat=? AND hash=?",
                        params![v["chat"].as_str(), v["hash"].as_str()],
                    )?;
                    store.execute(
                        "DELETE FROM media_senders WHERE chat=? AND hash=?",
                        params![v["chat"].as_str(), v["hash"].as_str()],
                    )?;
                    store.execute(
                        "DELETE FROM media_assets WHERE chat=? AND hash=?",
                        params![v["chat"].as_str(), v["hash"].as_str()],
                    )?;
                }
                Self::save(store, m, key, index, &hash, (kind, &file, bytes))?;
                store.update_media_source(
                    &m.chat,
                    &hash,
                    &m.sender,
                    self.public_hashes.contains(&hash),
                )?;
                tx.commit()?;
                Ok(())
            })();
            if result.is_err() {
                if let Some(path) = written {
                    let _ = fs::remove_file(path);
                }
            }
            result?;
            for v in victims {
                fs::remove_file(self.root.join(v["file"].as_str().unwrap()))?;
            }
        } else {
            Self::save(store, m, key, index, &hash, (kind, &file, bytes))?;
            store.update_media_source(
                &m.chat,
                &hash,
                &m.sender,
                self.public_hashes.contains(&hash),
            )?;
            tx.commit()?;
        }
        Ok(true)
    }
    fn save(
        store: &Store,
        m: &policy::Message,
        key: &str,
        index: usize,
        hash: &str,
        asset: (&str, &str, u64),
    ) -> Result<()> {
        let (kind, file, bytes) = asset;
        let (valence, rationality) = media_affect(store, &m.chat, m.ts)?;
        // 哈希去重使 N 次使用只有一份字节、一条素材，occurrences 仍准确计 N 次。
        store.execute("INSERT INTO media_assets(chat,hash,kind,file,occurrences,first_seen,last_seen,bytes,fitness) VALUES(?,?,?,?,1,?,?,?,'{}') ON CONFLICT(chat,hash) DO UPDATE SET occurrences=occurrences+1,first_seen=min(first_seen,excluded.first_seen),last_seen=max(last_seen,excluded.last_seen)",params![m.chat,hash,kind,file,m.ts,m.ts,bytes])?;
        store.execute(
            "INSERT INTO media_receipts VALUES(?,?,?)",
            params![m.chat, key, index as i64],
        )?;
        store.execute("INSERT OR IGNORE INTO media_contexts(chat,hash,message_id,role,valence,rationality) VALUES(?,?,?,'usage',?,?)", params![m.chat,hash,key,valence,rationality])?;
        for row in store.rows("SELECT message_key FROM messages WHERE chat=? AND message_key<>? AND ts<=? ORDER BY ts DESC,rowid DESC LIMIT 2",params![m.chat,key,m.ts])? {
            store.execute("INSERT OR IGNORE INTO media_contexts(chat,hash,message_id,role,valence,rationality) VALUES(?,?,?,'before',?,?)",params![m.chat,hash,row["message_key"].as_str(),valence,rationality])?;
        }
        Ok(())
    }
}
fn media_affect(store: &Store, chat: &str, now: f64) -> Result<(Option<f64>, Option<f64>)> {
    if store
        .first(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='affect_state'",
            [],
        )?
        .is_none()
    {
        return Ok((None, None));
    }
    let mut values = (0., 0.);
    for row in store.rows(
        "SELECT dimension,value,baseline,updated FROM affect_state WHERE chat=? AND subject='group' AND dimension IN ('mood','rationality')",
        [chat],
    )? {
        let updated = row["updated"].as_f64().unwrap_or(now);
        // 历史回填不能从后来写入的状态反推当时语气。
        if updated > now {
            return Ok((None, None));
        }
        let value = affect::decay(
            row["value"].as_f64().unwrap_or(0.),
            row["baseline"].as_f64().unwrap_or(0.),
            now - updated,
            4. * 3600.,
        );
        match row["dimension"].as_str() {
            Some("mood") => values.0 = value,
            Some("rationality") => values.1 = value,
            _ => {}
        }
    }
    Ok((Some(values.0), Some(values.1)))
}
fn private_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let mut b = fs::DirBuilder::new();
        b.recursive(true).mode(0o700).create(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(path)?;
    Ok(())
}
fn segments(event: &Value) -> Vec<Value> {
    if let Some(a) = event["message"].as_array() {
        return a.clone();
    }
    let mut result = Vec::new();
    policy::replace_cq(event["message"].as_str().unwrap_or(""), |body| {
        let mut parts = body.split(',');
        let kind = parts.next()?;
        let mut data = serde_json::Map::new();
        for part in parts {
            if let Some((k, v)) = part.split_once('=') {
                data.insert(
                    k.into(),
                    json!(v
                        .replace("&#44;", ",")
                        .replace("&#91;", "[")
                        .replace("&#93;", "]")
                        .replace("&amp;", "&")),
                );
            }
        }
        result.push(json!({"type":kind,"data":data}));
        Some(String::new())
    });
    result
}

/// Keep segment positions, including non-media parts, for media_receipts.
pub fn typed_segments(event: &Value) -> Vec<Segment> {
    segments(event).iter().map(|segment| {
        let data = &segment["data"];
        match segment["type"].as_str() {
            Some("image") => Segment::Image {
                src: MediaSrc::Opaque(data["url"].as_str().filter(|value| !value.is_empty())
                    .or_else(|| data["file"].as_str()).unwrap_or("").to_owned()),
                summary: data["summary"].as_str().map(str::to_owned),
            },
            Some("face") => Segment::Face { id: js_string(&data["id"]) },
            _ => Segment::Unknown { kind: data["type"].as_str().unwrap_or("other").to_owned() },
        }
    }).collect()
}

pub fn capture_face_frequency(store: &Store, chat: &str, id: &str, segments: &[Segment]) -> Result<()> {
    if !chat.starts_with("group:") { return Ok(()); }
    let Some(key) = store.resolve_message_key(chat, id)? else {
        store.execute("INSERT INTO message_identity_audit(kind,table_name,chat,raw_id,detail,created) VALUES('unresolved_reference','humanize_faces',?,?,'capture has no unique stored message',strftime('%s','now'))", rusqlite::params![chat,id])?;
        return Ok(());
    };
    let has_face = segments.iter().any(|segment| match segment {
        Segment::Face { id } => !id.is_empty() && id.len() <= 5 && id.bytes().all(|byte| byte.is_ascii_digit()),
        _ => false,
    });
    store.execute("INSERT OR IGNORE INTO humanize_faces VALUES(?,?,?)", rusqlite::params![chat,key,has_face])?;
    Ok(())
}
impl Store {
    pub fn enable_media(&self) -> Result<()> {
        let tx = self.immediate()?;
        self.connection()
            .execute_batch(include_str!("../store/media.sql"))?;
        // 和 store 初始化一样先探测后 ALTER；旧素材必须默认 unknown，不能猜公开。
        let columns = self.rows("PRAGMA table_info(media_assets)", [])?;
        let old_senders = !columns.iter().any(|r| r["name"] == "distinct_senders");
        for (name, definition) in [
            ("source_tier", "TEXT NOT NULL DEFAULT 'unknown' CHECK(source_tier IN ('public','private','unknown'))"),
            ("distinct_senders", "INTEGER NOT NULL DEFAULT 0"),
            ("source_override", "TEXT CHECK(source_override IN ('public','private'))"),
            ("public_corpus_match", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            if !columns.iter().any(|r| r["name"] == name) {
                self.connection().execute_batch(&format!("ALTER TABLE media_assets ADD COLUMN {name} {definition}"))?;
            }
        }
        if old_senders {
            // 只回填能由 usage 引用证实的历史人类；已清理的历史无法恢复，不伪造人数。
            self.execute("INSERT OR IGNORE INTO media_senders SELECT c.chat,c.hash,m.sender FROM media_contexts c JOIN messages m ON m.chat=c.chat AND m.message_key=c.message_id JOIN media_assets a ON a.chat=c.chat AND a.hash=c.hash WHERE c.role='usage' AND m.self=0 AND m.sender IS NOT NULL AND m.sender<>''", [])?;
            self.execute("UPDATE media_assets SET distinct_senders=(SELECT count(*) FROM media_senders s WHERE s.chat=media_assets.chat AND s.hash=media_assets.hash)", [])?;
        }
        let context_columns = self.rows("PRAGMA table_info(media_contexts)", [])?;
        for column in ["valence", "rationality"] {
            if !context_columns.iter().any(|r| r["name"] == column) {
                self.connection().execute_batch(&format!(
                    "ALTER TABLE media_contexts ADD COLUMN {column} REAL CHECK({column} BETWEEN -1 AND 1)"
                ))?;
            }
        }
        tx.commit()?;
        Ok(())
    }
    fn update_media_source(
        &self,
        chat: &str,
        hash: &str,
        sender: &str,
        corpus_match: bool,
    ) -> Result<()> {
        self.execute(
            "INSERT OR IGNORE INTO media_senders VALUES(?,?,?)",
            params![chat, hash, sender],
        )?;
        self.execute("UPDATE media_assets SET distinct_senders=(SELECT count(*) FROM media_senders WHERE chat=? AND hash=?), public_corpus_match=max(public_corpus_match,?) WHERE chat=? AND hash=?", params![chat,hash,corpus_match as i32,chat,hash])?;
        self.refresh_media_source(chat, hash)
    }
    fn refresh_media_source(&self, chat: &str, hash: &str) -> Result<()> {
        let row = self.first("SELECT distinct_senders,public_corpus_match,source_override FROM media_assets WHERE chat=? AND hash=?",params![chat,hash])?.ok_or_else(||anyhow::anyhow!("missing_media_asset"))?;
        let manual_override = match row["source_override"].as_str() {
            Some("public") => Some(Override::Public),
            Some("private") => Some(Override::Private),
            _ => None,
        };
        let result = media_source::classify(
            Evidence {
                distinct_senders: row["distinct_senders"].as_u64().unwrap_or(0),
                public_corpus_match: row["public_corpus_match"] == 1,
                manual_override,
            },
            3,
        );
        self.execute(
            "UPDATE media_assets SET source_tier=? WHERE chat=? AND hash=?",
            params![result.tier.as_str(), chat, hash],
        )?;
        Ok(())
    }
    /// 后台人工覆盖最高优先级；None 清除覆盖，按已记录的本地正面证据重算。
    pub fn set_media_source_override(
        &self,
        chat: &str,
        hash: &str,
        value: Option<Override>,
    ) -> Result<()> {
        let tx = self.immediate()?;
        self.execute(
            "UPDATE media_assets SET source_override=? WHERE chat=? AND hash=?",
            params![value.map(Override::as_str), chat, hash],
        )?;
        self.refresh_media_source(chat, hash)?;
        tx.commit()?;
        Ok(())
    }
    pub fn media_assets(&self, chat: &str) -> Result<Vec<Value>> {
        let mut rows = self.rows(
            "SELECT * FROM media_assets WHERE chat=? ORDER BY hash",
            [chat],
        )?;
        for r in &mut rows {
            crate::store::decode(r, &["fitness"])?;
        }
        Ok(rows)
    }
    pub fn media_stages(&self, chat: &str, limit: i64) -> Result<Vec<Value>> {
        let mut rows = self.rows(
            "SELECT coalesce(m.id,s.message_id) AS message_id,s.observed,s.classification FROM media_stages s LEFT JOIN messages m ON m.chat=s.chat AND m.message_key=s.message_id WHERE s.chat=? ORDER BY s.observed DESC,s.message_id DESC LIMIT ?",
            params![chat, limit],
        )?;
        for row in &mut rows {
            if let Some(raw) = row["classification"].as_str() {
                row["classification"] = serde_json::from_str(raw)?;
            }
        }
        Ok(rows)
    }
    pub fn record_media_stage(
        &self,
        chat: &str,
        id: &str,
        now: f64,
        config: &conversation::Config,
    ) -> Result<()> {
        let key = self.resolve_message_key(chat, id)?
            .ok_or_else(|| anyhow::anyhow!("missing_message"))?;
        let history = self.history(chat, Some(24))?;
        let messages = history
            .iter()
            .map(|v| {
                let mut v = v.clone();
                v["self"] = json!(v["self"].as_i64() == Some(1));
                v["hint"] = json!("open");
                serde_json::from_value(v)
            })
            .collect::<std::result::Result<Vec<policy::Message>, _>>()?;
        let target = history
            .iter()
            .position(|m| m["message_key"] == key)
            .ok_or_else(|| anyhow::anyhow!("missing_message"))?;
        let result = conversation::classify(&messages, target, now, config);
        self.execute("INSERT INTO media_stages VALUES(?,?,?,?) ON CONFLICT(chat,message_id) DO UPDATE SET observed=excluded.observed,classification=excluded.classification",params![chat,key,now,serde_json::to_string(&result)?])?;
        Ok(())
    }
}

/// Shared bounded image reader. OneBot commonly supplies an opaque file id plus a URL.
pub fn read_image(segment: &Value, max_bytes: u64, timeout_seconds: u64) -> Result<Vec<u8>> {
    let source = segment["data"]["url"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| segment["data"]["file"].as_str())
        .ok_or_else(|| anyhow::anyhow!("missing_file"))?;
    read_image_source(&MediaSrc::Opaque(source.to_owned()), max_bytes, timeout_seconds)
}
pub fn read_image_source(source: &MediaSrc, max_bytes: u64, timeout_seconds: u64) -> Result<Vec<u8>> {
    let source = match source {
        MediaSrc::Url(source) | MediaSrc::LocalPath(source) | MediaSrc::Opaque(source) => source,
    };
    ensure!(!source.is_empty(), "missing_file");
    let reader: Box<dyn Read> = if source.starts_with("http://") || source.starts_with("https://") {
        Box::new(
            ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(timeout_seconds.max(1)))
                .build()
                .get(source)
                .call()?
                .into_reader(),
        )
    } else {
        Box::new(fs::File::open(
            source.strip_prefix("file://").unwrap_or(source),
        )?)
    };
    let mut data = Vec::new();
    reader
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut data)?;
    ensure!(
        !data.is_empty() && data.len() as u64 <= max_bytes,
        "invalid_media_size"
    );
    Ok(data)
}
