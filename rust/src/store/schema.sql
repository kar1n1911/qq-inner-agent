-- 集中保留 JS 五个模块的建表定义；所有 REAL 时间列均为秒。
PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;
      CREATE TABLE IF NOT EXISTS message_sequence(id INTEGER PRIMARY KEY AUTOINCREMENT);
      CREATE TABLE IF NOT EXISTS messages(message_key TEXT NOT NULL PRIMARY KEY, chat TEXT, id TEXT, sender TEXT, name TEXT, text TEXT, ts REAL, self INTEGER DEFAULT 0, received_at REAL NOT NULL DEFAULT -1);
      CREATE INDEX IF NOT EXISTS messages_chat_id ON messages(chat,id);
      CREATE TABLE IF NOT EXISTS inbound_reply_relations(source_message_key TEXT PRIMARY KEY,raw_target_id TEXT,target_message_key TEXT,status TEXT NOT NULL CHECK(status IN ('resolved','unresolved')),reason TEXT NOT NULL);
      CREATE INDEX IF NOT EXISTS inbound_reply_target ON inbound_reply_relations(target_message_key);
      CREATE VIEW IF NOT EXISTS inbound_reply_status AS SELECT m.message_key AS source_message_key,r.raw_target_id,r.target_message_key,COALESCE(r.status,'unknown') AS status,r.reason FROM messages m LEFT JOIN inbound_reply_relations r ON r.source_message_key=m.message_key;
      CREATE TRIGGER IF NOT EXISTS inbound_reply_cleanup AFTER DELETE ON messages BEGIN DELETE FROM inbound_reply_relations WHERE source_message_key=OLD.message_key; UPDATE inbound_reply_relations SET target_message_key=NULL,status='unresolved',reason='target_pruned' WHERE target_message_key=OLD.message_key; END;
      CREATE TABLE IF NOT EXISTS topic_revisit_topics(chat TEXT NOT NULL,topic_key TEXT NOT NULL,topic TEXT NOT NULL,stage TEXT NOT NULL,category_version INTEGER NOT NULL,category TEXT NOT NULL,category_evidence TEXT NOT NULL,first_observed REAL NOT NULL,PRIMARY KEY(chat,topic_key),UNIQUE(chat,topic));
      CREATE INDEX IF NOT EXISTS topic_revisit_label ON topic_revisit_topics(chat,topic);
      CREATE TABLE IF NOT EXISTS topic_revisit_sends(message_key TEXT PRIMARY KEY,chat TEXT NOT NULL,topic_key TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS topic_revisit_feedback(source_message_key TEXT PRIMARY KEY,chat TEXT NOT NULL,topic_key TEXT NOT NULL,stage TEXT NOT NULL,category_version INTEGER NOT NULL,category TEXT NOT NULL,signal REAL NOT NULL,weight REAL NOT NULL,kind TEXT NOT NULL,observed_at REAL NOT NULL);
      CREATE VIEW IF NOT EXISTS topic_revisit_status AS SELECT t.chat,t.topic,t.topic_key,t.stage,t.category_version,t.category,(1.0+COALESCE(SUM(f.signal*f.weight),0.0))/(3.0+COALESCE(SUM(f.weight),0.0)) AS revisitability,COALESCE(SUM(CASE WHEN f.kind='positive' THEN 1 ELSE 0 END),0) AS positives FROM topic_revisit_topics t LEFT JOIN topic_revisit_feedback f ON f.chat=t.chat AND f.topic_key=t.topic_key AND f.stage=t.stage GROUP BY t.chat,t.topic_key;
      CREATE VIEW IF NOT EXISTS topic_revisit_category_status AS SELECT chat,category_version,category,stage,(1.0+SUM(signal*weight))/(3.0+SUM(weight)) AS revisitability,SUM(CASE WHEN kind='positive' THEN 1 ELSE 0 END) AS positives FROM topic_revisit_feedback WHERE category<>'unknown' GROUP BY chat,category_version,category,stage;
      CREATE TABLE IF NOT EXISTS message_identity_audit(id INTEGER PRIMARY KEY,kind TEXT NOT NULL,table_name TEXT NOT NULL,chat TEXT,raw_id TEXT,message_key TEXT,detail TEXT,created REAL);
      CREATE TABLE IF NOT EXISTS migration_message_key_v1(completed INTEGER PRIMARY KEY CHECK(completed=1));
      INSERT OR IGNORE INTO migration_message_key_v1(completed) VALUES(1);
      CREATE TABLE IF NOT EXISTS thoughts(id TEXT PRIMARY KEY, chat TEXT, text TEXT, kind TEXT, created REAL, used INTEGER DEFAULT 0, score REAL DEFAULT 0);
      CREATE TABLE IF NOT EXISTS notes(id TEXT PRIMARY KEY, chat TEXT, text TEXT, created REAL);
      CREATE TABLE IF NOT EXISTS decisions(id TEXT PRIMARY KEY, chat TEXT, ts REAL, action TEXT, score REAL, tags TEXT);
      CREATE TABLE IF NOT EXISTS deliveries(id TEXT PRIMARY KEY, chat TEXT, ts REAL, proactive INTEGER, status TEXT, message_id TEXT);
      CREATE TABLE IF NOT EXISTS outbound_message_facts(chat TEXT NOT NULL, id TEXT NOT NULL, delivery_id TEXT NOT NULL, time_source TEXT NOT NULL CHECK(time_source IN ('local_estimate','server')), PRIMARY KEY(chat,id));
      CREATE TABLE IF NOT EXISTS calls(ts REAL);
      CREATE TABLE IF NOT EXISTS send_assessments(id TEXT PRIMARY KEY, chat TEXT, human_id TEXT, ts REAL, status TEXT, details TEXT, UNIQUE(chat,human_id));
      CREATE TABLE IF NOT EXISTS expectations(chat TEXT PRIMARY KEY, ts REAL, expires REAL, forecast TEXT, observation TEXT);
      CREATE TABLE IF NOT EXISTS handled(chat TEXT PRIMARY KEY, human_id TEXT, pause_done INTEGER DEFAULT 0);
      CREATE TABLE IF NOT EXISTS chat_learning(chat TEXT PRIMARY KEY, style TEXT, sources TEXT, updated REAL, last_id TEXT, epoch INTEGER DEFAULT 0);
      CREATE TABLE IF NOT EXISTS learned_memories(id TEXT PRIMARY KEY, chat TEXT, text TEXT, sources TEXT, created REAL, expires REAL);
      CREATE INDEX IF NOT EXISTS learned_memories_chat ON learned_memories(chat,expires);
      CREATE INDEX IF NOT EXISTS messages_chat_ts ON messages(chat,ts);
      CREATE INDEX IF NOT EXISTS deliveries_chat_ts ON deliveries(chat,ts);
      CREATE INDEX IF NOT EXISTS thoughts_chat ON thoughts(chat,created);
CREATE TABLE IF NOT EXISTS memory_layers(
      id TEXT PRIMARY KEY, chat TEXT NOT NULL, subject TEXT NOT NULL, layer TEXT NOT NULL,
      slot TEXT NOT NULL, text TEXT NOT NULL, sources TEXT NOT NULL, importance REAL,
      created REAL, updated REAL, expires REAL, revision INTEGER DEFAULT 1,
      UNIQUE(chat,subject,layer,slot));
      CREATE INDEX IF NOT EXISTS memory_layers_scope ON memory_layers(chat,subject,layer,expires);
CREATE TABLE IF NOT EXISTS memory_revisions (
      memory_id TEXT, revision INTEGER, text TEXT, sources TEXT, updated REAL, replaced REAL,
      PRIMARY KEY(memory_id,revision));
      CREATE TRIGGER IF NOT EXISTS memory_revision_cleanup AFTER DELETE ON memory_layers BEGIN
        DELETE FROM memory_revisions WHERE memory_id=OLD.id;
      END;
CREATE TABLE IF NOT EXISTS expressions(chat TEXT,subject TEXT,kind TEXT,term TEXT,meaning TEXT,situation TEXT,example TEXT,confidence REAL,sources TEXT,updated REAL,last_used REAL DEFAULT 0,PRIMARY KEY(chat,subject,kind,term));
      CREATE TABLE IF NOT EXISTS decoration_usage(chat TEXT PRIMARY KEY,ts REAL);
CREATE TABLE IF NOT EXISTS activity_rhythm (
      id INTEGER PRIMARY KEY CHECK(id=1), signature TEXT, started REAL, until REAL,
      active INTEGER, probability REAL, draw REAL);
CREATE TABLE IF NOT EXISTS group_orientation(chat TEXT PRIMARY KEY, started REAL, message_count INTEGER DEFAULT 0,
      status TEXT DEFAULT 'observing', collected INTEGER DEFAULT 0, sources TEXT DEFAULT '{}', analysis TEXT DEFAULT '{}', retry_at REAL DEFAULT 0,
      error TEXT, epoch INTEGER DEFAULT 0, joined_at REAL DEFAULT 0);
