PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;
      CREATE TABLE IF NOT EXISTS messages(chat TEXT, id TEXT, sender TEXT, name TEXT, text TEXT, ts REAL, self INTEGER DEFAULT 0, PRIMARY KEY(chat,id));
      CREATE TABLE IF NOT EXISTS thoughts(id TEXT PRIMARY KEY, chat TEXT, text TEXT, kind TEXT, created REAL, used INTEGER DEFAULT 0, score REAL DEFAULT 0);
      CREATE TABLE IF NOT EXISTS notes(id TEXT PRIMARY KEY, chat TEXT, text TEXT, created REAL);
      CREATE TABLE IF NOT EXISTS decisions(id TEXT PRIMARY KEY, chat TEXT, ts REAL, action TEXT, score REAL, tags TEXT);
      CREATE TABLE IF NOT EXISTS deliveries(id TEXT PRIMARY KEY, chat TEXT, ts REAL, proactive INTEGER, status TEXT, message_id TEXT);
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
      CREATE TABLE IF NOT EXISTS decoration_usage(chat TEXT PRIMARY KEY,ts REAL);;
CREATE TABLE IF NOT EXISTS activity_rhythm (
      id INTEGER PRIMARY KEY CHECK(id=1), signature TEXT, started REAL, until REAL,
      active INTEGER, probability REAL, draw REAL);
CREATE TABLE IF NOT EXISTS group_orientation(chat TEXT PRIMARY KEY, started REAL, message_count INTEGER DEFAULT 0,
      status TEXT DEFAULT 'observing', collected INTEGER DEFAULT 0, sources TEXT DEFAULT '{}', analysis TEXT DEFAULT '{}', retry_at REAL DEFAULT 0,
      error TEXT, epoch INTEGER DEFAULT 0, joined_at REAL DEFAULT 0);;