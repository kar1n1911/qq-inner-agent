use qq_inner_core::store::Store;
use serde_json::{json, Value};
use std::{fs, path::PathBuf, process::Command};
struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn node(f: &Fixture, mode: &str) -> Value {
    let output = Command::new("node").current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap())
        .args(["--input-type=module", "-e", r#"
import {Store} from './src/store.mjs';
import {ActivityRhythm} from './src/activity.mjs';
import {GroupOrientation} from './src/orientation.mjs';
import {defaults} from './src/config.mjs';
const s=new Store(process.argv[1]);
new ActivityRhythm(s,defaults.agent);
new GroupOrientation(s,{agent:{...defaults.agent,observation:{enabled:false}}},null,null,()=>100,null);
if(process.argv[2]==='write') {
 s.message({chat:'g',id:'js',sender:'1',name:'测试',text:'hello🙂',ts:100.25});
 s.decision('g','send',3,['中文',{nested:null}],100.25);
 s.assess('g','js',100.25,'ready',{ok:true,list:[1,null]});
 s.expect('g',100.25,60,{reply:true});
 s.db.prepare('INSERT INTO chat_learning VALUES(?,?,?,?,?,?)').run('g','style','[{"id":"js"}]',100.25,'js',0);
}
const objects=s.db.prepare("SELECT type,name,tbl_name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name").all();
const tables={};
for(const {type,name} of objects) if(type==='table') {
 const indexes=s.db.prepare(`PRAGMA index_list("${name}")`).all().map(i=>{
  delete i.seq;i.columns=s.db.prepare(`PRAGMA index_info("${i.name}")`).all();return i;
 }).sort((a,b)=>a.name<b.name?-1:a.name>b.name?1:0);
 tables[name]={columns:s.db.prepare(`PRAGMA table_info("${name}")`).all(),indexes};
}
console.log(JSON.stringify({schema:{objects,tables},messages:s.history('g',24),decisions:s.db.prepare('SELECT * FROM decisions ORDER BY ts').all().map(r=>({...r,tags:JSON.parse(r.tags)})),assessment:s.assessment('g','rust'),expectation:s.expectation('g',101)}));
s.close();
"#]).arg(f.0.join("agent.sqlite")).arg(mode).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn normalize(mut schema: Value) -> Value {
    for row in schema["objects"].as_array_mut().unwrap() {
        if let Some(sql) = row["sql"].as_str() {
            row["sql"] = json!(sql
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>());
        }
    }
    schema
}
#[test]
fn shared_js_database_schema_and_json_roundtrip() {
    if !Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("SKIP: node unavailable");
        return;
    }
    let f = Fixture(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "store-fixture-{}-{}",
                std::process::id(),
                rand::random::<u64>()
            )),
    );
    fs::create_dir_all(&f.0).unwrap();
    let js = node(&f, "write");
    let s = Store::open(f.0.join("agent.sqlite")).unwrap();
    assert_eq!(
        normalize(s.schema().unwrap()),
        normalize(js["schema"].clone())
    );
    assert_eq!(s.schema().unwrap()["tables"].as_object().unwrap().len(), 17);
    assert_eq!(
        normalize(Store::in_memory().unwrap().schema().unwrap()),
        normalize(js["schema"].clone())
    );
    let db = s.connection();
    let text: String = db
        .prepare_cached("SELECT text FROM messages WHERE id='js'")
        .unwrap()
        .query_row([], |r| r.get(0))
        .unwrap();
    assert_eq!(text, "hello🙂");
    let tags: String = db
        .prepare_cached("SELECT tags FROM decisions")
        .unwrap()
        .query_row([], |r| r.get(0))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&tags).unwrap(),
        json!(["中文",{"nested":null}])
    );
    assert_eq!(s.history("g", None).unwrap()[0]["text"], "hello🙂");
    assert_eq!(
        s.assessment("g", "js").unwrap().unwrap()["details"],
        json!({"ok":true,"list":[1,null]})
    );
    assert_eq!(
        s.learning_state("g").unwrap()["sources"],
        json!([{"id":"js"}])
    );
    assert_eq!(
        s.expectation("g", 101.).unwrap().unwrap()["forecast"],
        json!({"reply":true})
    );
    s.message(&json!({"chat":"g","id":"rust","sender":"2","name":"Rust","text":"回应","ts":101.}))
        .unwrap();
    s.decision("g", "wait", 2., &json!({"a":[true,null,"中文"]}), 101.)
        .unwrap();
    s.assess("g", "rust", 101., "ready", &json!({"rust":["中文",null]}))
        .unwrap();
    s.expect("g", 101., 60., &json!({"rust":true})).unwrap();
    s.observe(&json!({"chat":"g","hint":"self"}), 101.).unwrap();
    let read = node(&f, "read");
    assert_eq!(read["messages"][1]["text"], "回应");
    assert_eq!(
        read["decisions"][1]["tags"],
        json!({"a":[true,null,"中文"]})
    );
    assert_eq!(
        serde_json::from_str::<Value>(read["assessment"]["details"].as_str().unwrap()).unwrap(),
        json!({"rust":["中文",null]})
    );
    assert_eq!(read["expectation"]["forecast"], json!({"rust":true}));
    assert_eq!(
        read["expectation"]["observation"],
        json!({"event":"human_message","addressed":true,"at":101})
    );
    let mode: String = db
        .prepare_cached("PRAGMA journal_mode")
        .unwrap()
        .query_row([], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    let timeout: i64 = db
        .prepare_cached("PRAGMA busy_timeout")
        .unwrap()
        .query_row([], |r| r.get(0))
        .unwrap();
    assert_eq!(timeout, 5000);
}
