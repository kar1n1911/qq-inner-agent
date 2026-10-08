//! Optional OCR boundary: replace `build` dispatch to add another recognition engine.
use crate::{engine::Logger, settings::sha256, store::Store};
use anyhow::{bail, ensure, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub engine: String,
    pub languages: String,
    pub binary: String,
    pub timeout_seconds: u64,
    pub max_chars: usize,
    pub max_bytes: u64,
    pub min_confidence: f64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            engine: "tesseract".into(),
            languages: "chi_sim+eng".into(),
            binary: "tesseract".into(),
            timeout_seconds: 20,
            max_chars: 800,
            max_bytes: 4 * 1024 * 1024,
            min_confidence: 60.0,
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.engine == "tesseract", "unsupported engine");
        ensure!(
            !self.binary.trim().is_empty() && !self.binary.contains('\0'),
            "invalid OCR binary"
        );
        ensure!(
            !self.languages.is_empty()
                && self
                    .languages
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_+-".contains(&b)),
            "invalid OCR languages"
        );
        ensure!(
            (1..=300).contains(&self.timeout_seconds),
            "invalid OCR timeoutSeconds"
        );
        ensure!(
            (1..=100_000).contains(&self.max_chars),
            "invalid OCR maxChars"
        );
        ensure!(
            (1..=100 * 1024 * 1024).contains(&self.max_bytes),
            "invalid OCR maxBytes"
        );
        ensure!(
            (0.0..=100.0).contains(&self.min_confidence),
            "invalid OCR minConfidence"
        );
        Ok(())
    }
}
/// Confidence is a recognition score, not a guarantee that the image is understood.
#[derive(Clone, Debug, PartialEq)]
pub struct OcrOutput {
    pub text: String,
    pub confidence: f64,
    pub reliable: bool,
    /// Fraction of recognized words below the configured confidence threshold.
    pub low_confidence_ratio: f64,
}
pub trait Engine {
    fn recognize(&self, image: &[u8]) -> Result<OcrOutput>;
}
pub struct Tesseract {
    pub min_confidence: f64,
    pub binary: String,
    pub languages: String,
    pub timeout_seconds: u64,
}
// RAII guards cover spawn, write, wait, timeout and decoding failures alike.
struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Engine for Tesseract {
    fn recognize(&self, image: &[u8]) -> Result<OcrOutput> {
        let run = || -> Result<OcrOutput> {
            let path = std::env::temp_dir().join(format!("qq-ocr-{}", crate::store::uuid()));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(&path)?;
            let tmp = Temporary(path);
            let input = tmp.0.join("input");
            File::create(&input)?.write_all(image)?;
            let output = tmp.0.join("stdout");
            // No shell interpolation; neither stderr nor temporary paths escape this module.
            let mut child = Process(
                Command::new(&self.binary)
                    .arg(&input)
                    .args(["stdout", "-l", &self.languages, "tsv"])
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .stdout(File::create(&output)?)
                    .spawn()?,
            );
            let start = Instant::now();
            loop {
                if let Some(status) = child.0.try_wait()? {
                    ensure!(status.success(), "ocr_failed");
                    break;
                }
                ensure!(
                    start.elapsed() < Duration::from_secs(self.timeout_seconds),
                    "ocr_failed"
                );
                // Bound output even for a broken replacement binary.
                ensure!(
                    fs::metadata(&output)?.len() <= 4 * 1024 * 1024,
                    "ocr_failed"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            ensure!(
                fs::metadata(&output)?.len() <= 4 * 1024 * 1024,
                "ocr_failed"
            );
            let mut bytes = Vec::new();
            File::open(output)?
                .take(4 * 1024 * 1024)
                .read_to_end(&mut bytes)?;
            parse_tsv(&String::from_utf8_lossy(&bytes), self.min_confidence)
        };
        run().map_err(|_| anyhow::anyhow!("ocr_failed"))
    }
}
// Tesseract TSV has conf in column 11 and text in column 12 (not the last
// column for confidence). Ignore structural rows, whose confidence is -1.
fn parse_tsv(tsv: &str, min_confidence: f64) -> Result<OcrOutput> {
    let mut lines = tsv.lines();
    ensure!(lines.next() == Some("level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext"), "invalid TSV header");
    let mut text = String::new();
    let mut previous_line = None;
    let mut sum = 0.0;
    let mut count = 0usize;
    let mut low = 0usize;
    for row in lines {
        let columns: Vec<_> = row.splitn(12, '\t').collect();
        ensure!(columns.len() == 12, "invalid TSV row");
        if columns[0] != "5" || columns[11].trim().is_empty() {
            continue;
        }
        let conf: f64 = columns[10].parse()?;
        ensure!((0.0..=100.0).contains(&conf), "invalid word confidence");
        let line = (columns[1], columns[2], columns[3], columns[4]);
        if let Some(previous) = previous_line {
            text.push(if previous == line { ' ' } else { '\n' });
        }
        text.push_str(columns[11].trim());
        previous_line = Some(line);
        sum += conf;
        count += 1;
        low += usize::from(conf < min_confidence);
    }
    let confidence = if count == 0 {
        0.0
    } else {
        sum / count as f64 / 100.0
    };
    Ok(OcrOutput {
        text,
        confidence,
        reliable: count > 0 && confidence * 100.0 >= min_confidence,
        low_confidence_ratio: if count == 0 {
            0.0
        } else {
            low as f64 / count as f64
        },
    })
}
struct Limited {
    engine: Tesseract,
    max_chars: usize,
}
impl Engine for Limited {
    fn recognize(&self, image: &[u8]) -> Result<OcrOutput> {
        let mut output = self.engine.recognize(image)?;
        output.text = output.text.chars().take(self.max_chars).collect();
        Ok(output)
    }
}
pub fn build(settings: &Settings) -> Result<Box<dyn Engine>> {
    match settings.engine.as_str() {
        "tesseract" => {
            settings.validate()?;
            Ok(Box::new(Limited {
                engine: Tesseract {
                    min_confidence: settings.min_confidence,
                    binary: settings.binary.clone(),
                    languages: settings.languages.clone(),
                    timeout_seconds: settings.timeout_seconds,
                },
                max_chars: settings.max_chars,
            }))
        }
        _ => bail!("unsupported engine"),
    }
}
struct Job {
    chat: String,
    id: String,
    images: Vec<Value>,
    created: f64,
}
/// A bounded dedicated blocking task; ingest only submits, never waits for OCR.
pub(crate) struct Worker {
    sender: SyncSender<Job>,
    stopped: Arc<AtomicBool>,
    log: Logger,
}
impl Worker {
    pub(crate) fn start(
        settings: &Settings,
        store: Arc<Mutex<Store>>,
        log: Logger,
    ) -> Result<Option<Self>> {
        store
            .lock()
            .map_err(|_| anyhow::anyhow!("store_poisoned"))?
            .set_ocr_enabled(settings.enabled)?;
        if !settings.enabled {
            return Ok(None);
        }
        settings.validate()?;
        let settings = settings.clone();
        let (sender, receiver) = mpsc::sync_channel::<Job>(32);
        let logger = log.clone();
        let stopped = Arc::new(AtomicBool::new(false));
        let cancelled = stopped.clone();
        std::thread::Builder::new().name("media-ocr".into()).spawn(move || {
            for job in receiver {
                if cancelled.load(Ordering::Acquire) { break; }
                let result = || -> Result<()> {
                    let engine = build(&settings)?;
                    let mut outputs = Vec::new();
                    let mut hashes = Vec::new();
                    for segment in job.images {
                        if cancelled.load(Ordering::Acquire) { return Ok(()); }
                        let bytes = super::read_image(&segment, settings.max_bytes, settings.timeout_seconds)?;
                        if cancelled.load(Ordering::Acquire) { return Ok(()); }
                        hashes.push(sha256(&bytes));
                        outputs.push(engine.recognize(&bytes)?);
                    }
                    let text: String = outputs.iter().map(|o| o.text.as_str()).collect::<Vec<_>>().join("\n").chars().take(settings.max_chars).collect();
                    // A clear image must not hide uncertainty in another image in the message.
                    let confidence = outputs.iter().map(|o| o.confidence).fold(1.0, f64::min);
                    let reliable = outputs.iter().all(|o| o.reliable);
                    if !text.trim().is_empty() {
                        let db = store.lock().map_err(|_| anyhow::anyhow!("store_poisoned"))?;
                        // A reload disabling OCR or retention deletion must not resurrect results.
                        if db.ocr_enabled.get() && !cancelled.load(Ordering::Acquire) {
                            db.execute("INSERT OR REPLACE INTO media_ocr(chat,message_id,hash,text,engine,created,confidence,reliable) SELECT ?,?,?,?,?,?,?,? WHERE EXISTS(SELECT 1 FROM messages WHERE chat=? AND id=?)",
                                params![job.chat,job.id,if hashes.len() == 1 { hashes[0].clone() } else { sha256(hashes.join(":").as_bytes()) },text,settings.engine,job.created,confidence,reliable,job.chat,job.id])?;
                        }
                    }
                    Ok(())
                };
                if result().is_err() { logger("ocr_failed", json!({})); }
            }
        })?;
        Ok(Some(Self {
            sender,
            stopped,
            log,
        }))
    }
    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
    }
    pub(crate) fn enqueue(&self, chat: &str, id: &str, event: &Value, created: f64) {
        if self.stopped.load(Ordering::Acquire) {
            return;
        }
        let images: Vec<_> = super::segments(event)
            .into_iter()
            .filter(|s| s["type"] == "image")
            .collect();
        if images.is_empty() {
            return;
        }
        if self
            .sender
            .try_send(Job {
                chat: chat.into(),
                id: id.into(),
                images,
                created,
            })
            .is_err()
        {
            (self.log)("ocr_failed", json!({}));
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}
impl Store {
    pub fn set_ocr_enabled(&self, enabled: bool) -> Result<()> {
        if enabled {
            self.connection().execute_batch("CREATE TABLE IF NOT EXISTS media_ocr(chat TEXT NOT NULL,message_id TEXT NOT NULL,hash TEXT NOT NULL,text TEXT NOT NULL,engine TEXT NOT NULL,created REAL NOT NULL,confidence REAL,reliable INTEGER,PRIMARY KEY(chat,message_id));
                CREATE TRIGGER IF NOT EXISTS media_ocr_cleanup AFTER DELETE ON messages BEGIN DELETE FROM media_ocr WHERE chat=OLD.chat AND message_id=OLD.id; END;")?;
            // Upgrade databases created before quality signals existed. NULL means unknown.
            let columns = self.rows("PRAGMA table_info(media_ocr)", [])?;
            for (name, kind) in [("confidence", "REAL"), ("reliable", "INTEGER")] {
                if !columns.iter().any(|column| column["name"] == name) {
                    self.execute(
                        &format!("ALTER TABLE media_ocr ADD COLUMN {name} {kind}"),
                        [],
                    )?;
                }
            }
        }
        self.ocr_enabled.set(enabled);
        Ok(())
    }
    pub fn enhance_ocr(&self, rows: &mut [Value]) -> Result<()> {
        if !self.ocr_enabled.get() {
            return Ok(());
        }
        for row in rows {
            if let Some(ocr) = self.first(
                "SELECT text, reliable FROM media_ocr WHERE chat=? AND message_id=?",
                params![row["chat"].as_str(), row["id"].as_str()],
            )? {
                if let (Some(original), Some(text)) = (row["text"].as_str(), ocr["text"].as_str()) {
                    // One row represents all images in a message; insert the aggregate only once.
                    if let Some((pos, token)) = ["[image]", "[图片]"]
                        .into_iter()
                        .filter_map(|token| original.find(token).map(|p| (p, token)))
                        .min_by_key(|(p, _)| *p)
                    {
                        let mut enhanced = original.to_owned();
                        let replacement = if ocr["reliable"].as_i64() == Some(1) {
                            format!("[image: {text}]")
                        } else {
                            format!("[图片(文字识别可信度低,仅供参考): {text}]")
                        };
                        enhanced.replace_range(pos..pos + token.len(), &replacement);
                        row["text"] = json!(enhanced);
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn stub(body: &str) -> (Temporary, Settings) {
        let dir = std::env::temp_dir().join(format!("ocr-test-{}", crate::store::uuid()));
        fs::create_dir(&dir).unwrap();
        let binary = dir.join("tesseract");
        fs::write(&binary, format!("#!/bin/sh\nprintf '%s' \"$1\" > \"$0.input\"\n[ \"$2\" = stdout ] && [ \"$3\" = -l ] && [ \"$4\" = chi_sim+eng ] && [ \"$5\" = tsv ] || exit 7\n{body}\n")).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        (
            Temporary(dir),
            Settings {
                enabled: true,
                binary: binary.to_string_lossy().into(),
                ..Settings::default()
            },
        )
    }
    const HEADER: &str = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext";
    fn tsv_stub(text: &str, confidence: f64) -> String {
        format!("cat <<'TSV'\n{HEADER}\n5\t1\t1\t1\t1\t1\t0\t0\t10\t10\t{confidence}\t{text}\nTSV")
    }
    #[test]
    fn tsv_word_quality_and_line_reconstruction() {
        let body = format!(
            "cat <<'TSV'\n{HEADER}
1\t1\t0\t0\t0\t0\t0\t0\t100\t100\t-1\t
5\t1\t1\t1\t1\t1\t0\t0\t10\t10\t90\t中文
5\t1\t1\t1\t1\t2\t0\t0\t10\t10\t30\t梗图
5\t1\t1\t1\t2\t1\t0\t0\t10\t10\t60\t下一行
5\t1\t1\t1\t2\t2\t0\t0\t10\t10\t0\t\x20\x20\x20
TSV"
        );
        let (_dir, settings) = stub(&body);
        let output = build(&settings).unwrap().recognize(b"image").unwrap();
        assert_eq!(output.text, "中文 梗图\n下一行");
        assert_eq!(output.confidence, 0.6);
        assert_eq!(output.low_confidence_ratio, 1.0 / 3.0);
        assert!(output.reliable);
        cleaned(&settings);
    }
    #[test]
    fn confidence_threshold_boundaries_and_invalid_tsv() {
        for (conf, threshold, reliable) in [
            (59.9, 60.0, false),
            (60.0, 60.0, true),
            (60.1, 60.0, true),
            (0.0, 0.0, true),
            (100.0, 100.0, true),
        ] {
            let (_dir, mut settings) = stub(&tsv_stub("字", conf));
            settings.min_confidence = threshold;
            let output = build(&settings).unwrap().recognize(b"image").unwrap();
            assert_eq!(output.reliable, reliable);
        }
        let empty = parse_tsv(HEADER, 0.0).unwrap();
        assert_eq!(empty.confidence, 0.0);
        assert!(!empty.reliable);
        for conf in ["NaN", "inf", "-1", "101", "broken"] {
            assert!(parse_tsv(
                &format!("{HEADER}\n5\t1\t1\t1\t1\t1\t0\t0\t10\t10\t{conf}\t字"),
                60.0
            )
            .is_err());
        }
        assert!(parse_tsv("plain text", 60.0).is_err());
        assert!(Settings {
            min_confidence: f64::NAN,
            ..Settings::default()
        }
        .validate()
        .is_err());
    }
    #[test]
    fn legacy_schema_and_low_quality_context() {
        let db = Store::in_memory().unwrap();
        db.execute("CREATE TABLE media_ocr(chat TEXT,message_id TEXT,hash TEXT,text TEXT,engine TEXT,created REAL,PRIMARY KEY(chat,message_id))", []).unwrap();
        db.execute(
            "INSERT INTO media_ocr VALUES('a','1','hash','深文峰','tesseract',1)",
            [],
        )
        .unwrap();
        db.set_ocr_enabled(true).unwrap();
        db.set_ocr_enabled(true).unwrap();
        for token in ["[image]", "[图片]"] {
            db.message(&json!({"chat":"a","id":"1","text":format!("look {token} [image]"),"ts":1}))
                .unwrap();
            db.execute(
                "UPDATE messages SET text=? WHERE chat='a' AND id='1'",
                params![format!("look {token} [image]")],
            )
            .unwrap();
            assert_eq!(
                db.history("a", None).unwrap()[0]["text"],
                "look [图片(文字识别可信度低,仅供参考): 深文峰] [image]"
            );
        }
        db.execute("UPDATE media_ocr SET confidence=0.3,reliable=0", [])
            .unwrap();
        assert_eq!(
            crate::engine::backlog::full_history(&db, "a", 24).unwrap()[0]["text"],
            "look [图片(文字识别可信度低,仅供参考): 深文峰] [image]"
        );
        db.set_ocr_enabled(false).unwrap();
        assert_eq!(
            db.history("a", None).unwrap()[0]["text"],
            "look [图片] [image]"
        );
    }
    fn cleaned(settings: &Settings) {
        let input = fs::read_to_string(format!("{}.input", settings.binary)).unwrap();
        assert!(!std::path::Path::new(&input).parent().unwrap().exists());
    }
    #[test]
    fn dispatch_validation_and_camel_case() {
        assert!(build(&Settings::default()).is_ok());
        let unsupported = Settings {
            engine: "paddle".into(),
            ..Settings::default()
        };
        assert_eq!(
            build(&unsupported).err().unwrap().to_string(),
            "unsupported engine"
        );
        let mut c = crate::config::defaults();
        assert_eq!(
            c["agent"]["ocr"],
            serde_json::to_value(Settings::default()).unwrap()
        );
        for bad in [
            json!({"minConfidence":-0.1}),
            json!({"minConfidence":100.1}),
            json!({"maxChars":0}),
            json!({"timeoutSeconds":0}),
            json!({"maxBytes":0}),
            json!({"max_chars":42}),
            json!({"engine":"unknown"}),
            json!({"binary":""}),
        ] {
            c["agent"]["ocr"] = bad;
            assert!(crate::config::validate(&c).is_err());
        }
    }
    #[test]
    fn unicode_clipping_and_cleanup_on_success_failure_timeout() {
        let (_dir, mut settings) = stub(&tsv_stub("中文🙂abcdef", 90.0));
        settings.max_chars = 3;
        assert_eq!(
            build(&settings).unwrap().recognize(b"image").unwrap().text,
            "中文🙂"
        );
        cleaned(&settings);
        let (_dir, settings) = stub("echo secret-path >&2; exit 3");
        assert_eq!(
            build(&settings)
                .unwrap()
                .recognize(b"image")
                .unwrap_err()
                .to_string(),
            "ocr_failed"
        );
        cleaned(&settings);
        let (_dir, mut settings) = stub("exec sleep 10");
        settings.timeout_seconds = 1;
        let start = Instant::now();
        assert!(build(&settings).unwrap().recognize(b"image").is_err());
        assert!(start.elapsed() < Duration::from_secs(3));
        cleaned(&settings);
    }
    #[test]
    fn disabled_does_not_start_binary_download_or_schema() {
        let (_dir, mut settings) = stub("exit 1");
        settings.enabled = false;
        let db = Arc::new(Mutex::new(Store::in_memory().unwrap()));
        let log: Logger = Arc::new(|_, _| panic!("disabled OCR must do nothing"));
        assert!(Worker::start(&settings, db.clone(), log).unwrap().is_none());
        assert!(!std::path::Path::new(&format!("{}.input", settings.binary)).exists());
        assert!(db
            .lock()
            .unwrap()
            .first("SELECT 1 FROM sqlite_master WHERE name='media_ocr'", [])
            .unwrap()
            .is_none());
    }
    #[test]
    fn background_download_storage_context_and_disable() {
        use std::net::TcpListener;
        let (_dir, settings) = stub(&tsv_stub("文字🙂", 90.0));
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/image", server.local_addr().unwrap());
        let serving = std::thread::spawn(move || {
            let (mut stream, _) = server.accept().unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                assert!(request.len() < 8192);
            }
            // Hold the network response while verifying enqueue returns immediately.
            std::thread::sleep(Duration::from_millis(200));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nimage",
                )
                .unwrap();
        });
        let db = Arc::new(Mutex::new(Store::in_memory().unwrap()));
        db.lock()
            .unwrap()
            .message(&json!({"chat":"group:1","id":"a","text":"look [图片]","ts":1}))
            .unwrap();
        let worker = Worker::start(
            &settings,
            db.clone(),
            Arc::new(|_, _| panic!("unexpected OCR failure")),
        )
        .unwrap()
        .unwrap();
        let event = json!({"message":[{"type":"image","data":{"file":"opaque-id","url":url}}]});
        let start = Instant::now();
        worker.enqueue("group:1", "a", &event, 1.);
        assert!(start.elapsed() < Duration::from_millis(100));
        loop {
            if db
                .lock()
                .unwrap()
                .first("SELECT * FROM media_ocr", [])
                .unwrap()
                .is_some()
            {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(10));
        }
        serving.join().unwrap();
        let db = db.lock().unwrap();
        let stored = db
            .first("SELECT confidence,reliable FROM media_ocr", [])
            .unwrap()
            .unwrap();
        assert_eq!(stored["confidence"], 0.9);
        assert_eq!(stored["reliable"], 1);
        assert_eq!(
            db.history("group:1", None).unwrap()[0]["text"],
            "look [image: 文字🙂]"
        );
        assert_eq!(
            crate::engine::backlog::full_history(&db, "group:1", 24).unwrap()[0]["text"],
            "look [image: 文字🙂]"
        );
        assert_eq!(
            db.first("SELECT text FROM messages", []).unwrap().unwrap()["text"],
            "look [图片]"
        );
        db.set_ocr_enabled(false).unwrap();
        assert_eq!(
            db.history("group:1", None).unwrap()[0]["text"],
            "look [图片]"
        );
        db.execute("DELETE FROM messages", []).unwrap();
        assert!(db.first("SELECT * FROM media_ocr", []).unwrap().is_none());
    }
    #[test]
    fn mixed_image_quality_is_conservative_in_storage_and_context() {
        let body = format!(
            "if [ \"$(cat \"$1\")\" = clear ]; then\n{}\nelse\n{}\nfi",
            tsv_stub("清晰", 90.0),
            tsv_stub("深文峰", 30.0)
        );
        let (dir, settings) = stub(&body);
        let clear = dir.0.join("clear");
        let blurry = dir.0.join("blurry");
        fs::write(&clear, b"clear").unwrap();
        fs::write(&blurry, b"blurry").unwrap();
        let db = Arc::new(Mutex::new(Store::in_memory().unwrap()));
        db.lock()
            .unwrap()
            .message(&json!({"chat":"a","id":"1","text":"[image] [图片]","ts":1}))
            .unwrap();
        let worker = Worker::start(
            &settings,
            db.clone(),
            Arc::new(|_, _| panic!("unexpected OCR failure")),
        )
        .unwrap()
        .unwrap();
        worker.enqueue(
            "a",
            "1",
            &json!({"message":[
                {"type":"image","data":{"file":clear}},
                {"type":"image","data":{"file":blurry}}
            ]}),
            1.0,
        );
        let start = Instant::now();
        loop {
            let db = db.lock().unwrap();
            if let Some(row) = db.first("SELECT * FROM media_ocr", []).unwrap() {
                assert_eq!(row["confidence"], 0.3);
                assert_eq!(row["reliable"], 0);
                assert_eq!(
                    db.history("a", None).unwrap()[0]["text"],
                    "[图片(文字识别可信度低,仅供参考): 清晰\n深文峰] [图片]"
                );
                break;
            }
            drop(db);
            assert!(start.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn background_failure_only_logs_and_keeps_original_message() {
        let (dir, settings) = stub("exit 2");
        let file = dir.0.join("image");
        fs::write(&file, b"image").unwrap();
        let db = Arc::new(Mutex::new(Store::in_memory().unwrap()));
        db.lock()
            .unwrap()
            .message(&json!({"chat":"a","id":"1","text":"[image]","ts":1}))
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let worker = Worker::start(
            &settings,
            db.clone(),
            Arc::new(move |code, payload| {
                tx.send((code.to_owned(), payload)).unwrap();
            }),
        )
        .unwrap()
        .unwrap();
        worker.enqueue(
            "a",
            "1",
            &json!({"message":[{"type":"image","data":{"file":file}}]}),
            1.,
        );
        let (code, payload) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(code, "ocr_failed");
        assert_eq!(payload, json!({}));
        assert_eq!(
            db.lock().unwrap().history("a", None).unwrap()[0]["text"],
            "[image]"
        );
        assert!(db
            .lock()
            .unwrap()
            .first("SELECT 1 FROM media_ocr", [])
            .unwrap()
            .is_none());
        cleaned(&settings);
    }
    #[test]
    fn bounded_shared_reader_and_no_cross_chat_enrichment() {
        let (dir, _) = stub("exit 0");
        let file = dir.0.join("image");
        fs::write(&file, b"12345").unwrap();
        let segment = json!({"data":{"file":file}});
        assert!(super::super::read_image(&segment, 4, 1).is_err());
        assert_eq!(super::super::read_image(&segment, 5, 1).unwrap(), b"12345");
        let db = Store::in_memory().unwrap();
        db.set_ocr_enabled(true).unwrap();
        for chat in ["a", "b"] {
            db.message(&json!({"chat":chat,"id":"1","text":"[image]","ts":1}))
                .unwrap();
        }
        db.execute(
            "INSERT INTO media_ocr VALUES('a','1','hash','hello','tesseract',1,0.9,1)",
            [],
        )
        .unwrap();
        assert_eq!(db.history("a", None).unwrap()[0]["text"], "[image: hello]");
        assert_eq!(db.history("b", None).unwrap()[0]["text"], "[image]");
    }
}
