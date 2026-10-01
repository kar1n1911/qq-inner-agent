import { DatabaseSync } from 'node:sqlite';
import { randomUUID } from 'node:crypto';

export function terms(text) {
  const t = String(text).toLowerCase();
  const result = new Set(t.match(/[\p{L}\p{N}]{2,}/gu) || []);
  for (const run of t.match(/[\p{Script=Han}]+/gu) || []) for (let i = 0; i < run.length - 1; i++) result.add(run.slice(i, i + 2));
  return result;
}
export function similarity(a, b) {
  const x = terms(a), y = terms(b);
  if (!x.size || !y.size) return 0;
  return [...x].filter(t => y.has(t)).length / Math.sqrt(x.size * y.size);
}
export class Store {
  constructor(filename) {
    this.db = new DatabaseSync(filename);
    this.db.exec(`PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;
      CREATE TABLE IF NOT EXISTS messages(chat TEXT, id TEXT, sender TEXT, name TEXT, text TEXT, ts REAL, self INTEGER DEFAULT 0, PRIMARY KEY(chat,id));
      CREATE TABLE IF NOT EXISTS thoughts(id TEXT PRIMARY KEY, chat TEXT, text TEXT, kind TEXT, created REAL, used INTEGER DEFAULT 0, score REAL DEFAULT 0);
      CREATE TABLE IF NOT EXISTS notes(id TEXT PRIMARY KEY, chat TEXT, text TEXT, created REAL);
      CREATE TABLE IF NOT EXISTS decisions(id TEXT PRIMARY KEY, chat TEXT, ts REAL, action TEXT, score REAL, tags TEXT);
      CREATE TABLE IF NOT EXISTS deliveries(id TEXT PRIMARY KEY, chat TEXT, ts REAL, proactive INTEGER, status TEXT, message_id TEXT);
      CREATE TABLE IF NOT EXISTS calls(ts REAL);
      CREATE TABLE IF NOT EXISTS send_assessments(id TEXT PRIMARY KEY, chat TEXT, human_id TEXT, ts REAL, status TEXT, details TEXT, UNIQUE(chat,human_id));
      CREATE TABLE IF NOT EXISTS expectations(chat TEXT PRIMARY KEY, ts REAL, expires REAL, forecast TEXT, observation TEXT);
      CREATE TABLE IF NOT EXISTS handled(chat TEXT PRIMARY KEY, human_id TEXT, pause_done INTEGER DEFAULT 0);
      CREATE INDEX IF NOT EXISTS messages_chat_ts ON messages(chat,ts);
      CREATE INDEX IF NOT EXISTS deliveries_chat_ts ON deliveries(chat,ts);
      CREATE INDEX IF NOT EXISTS thoughts_chat ON thoughts(chat,created);`);
  }
  recoverDeliveries() { this.db.exec("UPDATE deliveries SET status='uncertain' WHERE status='pending'"); }
  close() { this.db.close(); }
  message(m) {
    return !!this.db.prepare('INSERT OR IGNORE INTO messages VALUES(?,?,?,?,?,?,?)').run(m.chat, m.id, m.sender, m.name, m.text, m.ts, m.self ? 1 : 0).changes;
  }
  history(chat, limit = 24) { return this.db.prepare('SELECT * FROM messages WHERE chat=? ORDER BY ts DESC,rowid DESC LIMIT ?').all(chat, limit).reverse(); }
  retrieve(chat, query, now) {
    const recent = this.history(chat, 200);
    const notes = this.db.prepare('SELECT text,created AS ts FROM notes WHERE chat=? ORDER BY created DESC LIMIT 50').all(chat);
    return [...notes.map(x => ({ ...x, type: 'owner_note' })), ...recent.map(x => ({ text: `${x.name}: ${x.text}`, ts: x.ts, type: 'past_utterance' }))]
      .map(x => ({ ...x, saliency: similarity(query, x.text) * Math.exp(-Math.max(0, now - x.ts) / 604800) + (x.type === 'owner_note' ? 0.15 : 0) }))
      .filter(x => x.saliency > 0.12).sort((a, b) => b.saliency - a.saliency).slice(0, 6);
  }
  note(chat, text, now) { this.db.prepare('INSERT INTO notes VALUES(?,?,?,?)').run(randomUUID(), chat, text, now); }
  reservoir(chat, now, ttl, limit) {
    return this.db.prepare('SELECT * FROM thoughts WHERE chat=? AND used=0 AND created>? ORDER BY created DESC LIMIT ?').all(chat, now - ttl, limit);
  }
  addThought(chat, thought, now) {
    const item = { id: randomUUID(), chat, text: thought.text, kind: thought.kind, created: now };
    this.db.prepare('INSERT INTO thoughts(id,chat,text,kind,created) VALUES(?,?,?,?,?)').run(item.id, chat, item.text, item.kind, now);
    return item;
  }
  score(id, score) { this.db.prepare('UPDATE thoughts SET score=? WHERE id=?').run(score, id); }
  use(id) { this.db.prepare('UPDATE thoughts SET used=1 WHERE id=?').run(id); }
  decision(chat, action, score, tags, now) { this.db.prepare('INSERT INTO decisions VALUES(?,?,?,?,?,?)').run(randomUUID(), chat, now, action, score, JSON.stringify(tags)); }
  callBudget(now, max) {
    this.db.prepare('DELETE FROM calls WHERE ts<?').run(now - 3600);
    if (this.db.prepare('SELECT count(*) AS n FROM calls').get().n >= max) return false;
    this.db.prepare('INSERT INTO calls VALUES(?)').run(now); return true;
  }
  delivery(chat, proactive, now) {
    const id = randomUUID();
    this.db.prepare('INSERT INTO deliveries VALUES(?,?,?,?,?,NULL)').run(id, chat, now, proactive ? 1 : 0, 'pending'); return id;
  }
  finishDelivery(id, status, messageId = null) { this.db.prepare('UPDATE deliveries SET status=?,message_id=? WHERE id=?').run(status, messageId == null ? null : String(messageId), id); }
  counts(chat, now) {
    return this.db.prepare("SELECT count(*) AS total, coalesce(sum(proactive),0) AS proactive, coalesce(max(ts),0) AS last FROM deliveries WHERE chat=? AND ts>? AND status IN ('sent','pending','uncertain')").get(chat, now - 3600);
  }
  markHandled(chat, id, pause = false) { this.db.prepare('INSERT INTO handled VALUES(?,?,?) ON CONFLICT(chat) DO UPDATE SET human_id=excluded.human_id,pause_done=excluded.pause_done').run(chat, id, pause ? 1 : 0); }
  handled(chat) { return this.db.prepare('SELECT * FROM handled WHERE chat=?').get(chat); }
  assessment(chat, humanId) { return this.db.prepare('SELECT * FROM send_assessments WHERE chat=? AND human_id=?').get(chat, humanId); }
  sendingTiming(chat, now, fallbackGap) {
    const last = this.db.prepare("SELECT max(ts) AS ts FROM deliveries WHERE chat=? AND status IN ('sent','pending','uncertain')").get(chat).ts;
    const recentHumans = this.db.prepare('SELECT count(*) AS n FROM messages WHERE chat=? AND self=0 AND ts>=?').get(chat, now - 60).n;
    return { gap: last === null ? fallbackGap : Math.max(0, now - last), recentHumans };
  }
  assess(chat, humanId, now, status, details) {
    this.db.prepare('INSERT INTO send_assessments VALUES(?,?,?,?,?,?)').run(randomUUID(), chat, humanId, now, status, JSON.stringify(details));
  }
  assessmentStatus(chat, humanId, status) { this.db.prepare('UPDATE send_assessments SET status=? WHERE chat=? AND human_id=?').run(status, chat, humanId); }
  expect(chat, now, seconds, forecast) {
    this.db.prepare('INSERT INTO expectations VALUES(?,?,?,?,NULL) ON CONFLICT(chat) DO UPDATE SET ts=excluded.ts,expires=excluded.expires,forecast=excluded.forecast,observation=NULL').run(chat, now, now + seconds, JSON.stringify(forecast));
  }
  observe(m, now) {
    this.db.prepare('UPDATE expectations SET observation=? WHERE chat=? AND observation IS NULL AND ts<=? AND expires>=?').run(JSON.stringify({ event: 'human_message', addressed: m.hint === 'self', at: now }), m.chat, now, now);
  }
  expectation(chat, now) {
    const r = this.db.prepare('SELECT * FROM expectations WHERE chat=? AND expires>?').get(chat, now);
    return r ? { forecast: JSON.parse(r.forecast), elapsedSeconds: Math.max(0, now - r.ts), observation: r.observation ? JSON.parse(r.observation) : { event: 'no_message_yet' } } : null;
  }
  activeChats(since) { return this.db.prepare('SELECT DISTINCT chat FROM messages WHERE self=0 AND ts>?').all(since).map(x => x.chat); }
  prune(now, retentionDays, maxPerChat) {
    this.db.prepare('DELETE FROM messages WHERE ts<?').run(now - retentionDays * 86400);
    for (const { chat } of this.db.prepare('SELECT DISTINCT chat FROM messages').all()) {
      this.db.prepare('DELETE FROM messages WHERE chat=? AND rowid NOT IN (SELECT rowid FROM messages WHERE chat=? ORDER BY ts DESC,rowid DESC LIMIT ?)').run(chat, chat, maxPerChat);
    }
    this.db.prepare('DELETE FROM thoughts WHERE created<?').run(now - 86400);
    for (const table of ['decisions', 'deliveries', 'send_assessments', 'expectations']) this.db.prepare(`DELETE FROM ${table} WHERE ts<?`).run(now - retentionDays * 86400);
    this.db.exec('DELETE FROM handled WHERE chat NOT IN (SELECT DISTINCT chat FROM messages)');
  }
}
