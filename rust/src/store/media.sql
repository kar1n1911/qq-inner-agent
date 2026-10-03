-- 显式启用才建新表；不改变默认迁移快照和任何已有表。
CREATE TABLE IF NOT EXISTS media_assets(chat TEXT NOT NULL, hash TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('image','face')), file TEXT NOT NULL, occurrences INTEGER NOT NULL, first_seen REAL NOT NULL, last_seen REAL NOT NULL, bytes INTEGER NOT NULL, fitness TEXT NOT NULL DEFAULT '{}', PRIMARY KEY(chat,hash));
CREATE TABLE IF NOT EXISTS media_contexts(chat TEXT NOT NULL, hash TEXT NOT NULL, message_id TEXT NOT NULL, role TEXT NOT NULL, PRIMARY KEY(chat,hash,message_id,role));
CREATE TABLE IF NOT EXISTS media_receipts(chat TEXT NOT NULL, message_id TEXT NOT NULL, segment INTEGER NOT NULL, PRIMARY KEY(chat,message_id,segment));
CREATE TABLE IF NOT EXISTS media_stages(chat TEXT NOT NULL, message_id TEXT NOT NULL, observed REAL NOT NULL, classification TEXT NOT NULL, PRIMARY KEY(chat,message_id));
