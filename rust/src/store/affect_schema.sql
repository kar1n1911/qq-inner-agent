-- 开关启用后建表，保持默认共享数据库 schema parity。
CREATE TABLE IF NOT EXISTS message_ratings(message_id TEXT,chat TEXT,mood REAL CHECK(mood BETWEEN -1 AND 1),agreement REAL CHECK(agreement BETWEEN -1 AND 1),confidence REAL CHECK(confidence BETWEEN 0 AND 1),rated_at REAL,PRIMARY KEY(chat,message_id));
    CREATE TABLE IF NOT EXISTS affect_state(chat TEXT,subject TEXT,dimension TEXT CHECK(dimension IN ('mood','rationality','affinity')),value REAL CHECK(value BETWEEN -1 AND 1),baseline REAL CHECK(baseline BETWEEN -1 AND 1),confidence REAL CHECK(confidence BETWEEN 0 AND 1),updated REAL,sources TEXT,PRIMARY KEY(chat,subject,dimension));
    CREATE TRIGGER IF NOT EXISTS affect_cleanup AFTER DELETE ON messages BEGIN DELETE FROM message_ratings WHERE chat=OLD.chat AND message_id=OLD.id; END;
    CREATE TABLE IF NOT EXISTS affect_bursts(chat TEXT PRIMARY KEY,human_id TEXT,count INTEGER);
