import { rankMemories } from './memory-ranking.mjs';
import { randomUUID } from 'node:crypto';

export function memorySubjects(chat, sender) {
  if (!/^(group|private):[1-9]\d*$/.test(chat) || !/^[1-9]\d*$/.test(String(sender))) throw Error('invalid_memory_scope');
  if (chat.startsWith('private:') && chat.slice(8) !== String(sender)) throw Error('invalid_memory_scope');
  return chat.startsWith('group:') ? ['group', `person:${sender}`] : [`person:${sender}`];
}

export function parseMemoryUpdates(value, history, chat, sender, settings) {
  const allowed = new Set(memorySubjects(chat, sender));
  const humans = new Map(history.filter(m => !m.self && m.chat === chat).map(m => [m.id, m]));
  if (!Array.isArray(value) || value.length > 4) throw Error('invalid_memory_updates');
  const seen = new Set();
  return value.map(v => {
    if (!v || !allowed.has(v.subject) || !['long_term', 'traits'].includes(v.layer) ||
        !['upsert', 'forget'].includes(v.operation) || typeof v.key !== 'string' || !v.key.trim() || v.key.length > 64 ||
        !Array.isArray(v.sourceIds) || v.sourceIds.length < 1 || v.sourceIds.length > 6 ||
        v.sourceIds.some(id => typeof id !== 'string' || !humans.has(id))) throw Error('invalid_memory_updates');
    const sources = [...new Set(v.sourceIds)].map(id => ({ id, sender: humans.get(id).sender, ts: humans.get(id).ts }));
    // One member cannot define another member, or a whole group's characteristics.
    if (v.subject === 'group' ? new Set(sources.map(s => s.sender)).size < 2 : sources.some(s => `person:${s.sender}` !== v.subject)) throw Error('memory_author_mismatch');
    const key = v.key.trim(), identity = `${v.subject}/${v.layer}/${key}`;
    if (seen.has(identity)) throw Error('duplicate_memory_update');
    seen.add(identity);
    const max = v.layer === 'long_term' ? Math.min(500, settings.longChars) : Math.min(300, settings.traitChars);
    if (v.operation === 'upsert' && (typeof v.text !== 'string' || !v.text.trim() || v.text.length > max ||
        typeof v.importance !== 'number' || !Number.isFinite(v.importance) || v.importance < 0 || v.importance > 1)) throw Error('invalid_memory_updates');
    const keywords = v.keywords ?? [];
    if (!Array.isArray(keywords) || keywords.length > 8 || keywords.some(k => typeof k !== 'string' || !k.trim() || k.length > 32)) throw Error('invalid_memory_keywords');
    const confidence = v.confidence ?? .6;
    if (typeof confidence !== 'number' || !Number.isFinite(confidence) || confidence < 0 || confidence > 1) throw Error('invalid_memory_confidence');
    return { keywords: [...new Set(keywords.map(k => k.trim()))], confidence, subject: v.subject, layer: v.layer, key, operation: v.operation,
      text: v.operation === 'upsert' ? v.text.trim() : '', importance: v.operation === 'upsert' ? v.importance : 0, sources };
  });
}

export class LayeredMemory {
  constructor(db) {
    this.db = db;
    db.exec(`CREATE TABLE IF NOT EXISTS memory_layers(
      id TEXT PRIMARY KEY, chat TEXT NOT NULL, subject TEXT NOT NULL, layer TEXT NOT NULL,
      slot TEXT NOT NULL, text TEXT NOT NULL, sources TEXT NOT NULL, importance REAL,
      created REAL, updated REAL, expires REAL, revision INTEGER DEFAULT 1,
      UNIQUE(chat,subject,layer,slot));
      CREATE INDEX IF NOT EXISTS memory_layers_scope ON memory_layers(chat,subject,layer,expires);`);
    const columns = new Set(db.prepare('PRAGMA table_info(memory_layers)').all().map(r => r.name));
    if (!columns.has('keywords')) db.exec("ALTER TABLE memory_layers ADD COLUMN keywords TEXT NOT NULL DEFAULT '[]'");
    if (!columns.has('confidence')) db.exec('ALTER TABLE memory_layers ADD COLUMN confidence REAL NOT NULL DEFAULT 0.6');
    db.exec(`CREATE TABLE IF NOT EXISTS memory_revisions (
      memory_id TEXT, revision INTEGER, text TEXT, sources TEXT, updated REAL, replaced REAL,
      PRIMARY KEY(memory_id,revision));
      CREATE TRIGGER IF NOT EXISTS memory_revision_cleanup AFTER DELETE ON memory_layers BEGIN
        DELETE FROM memory_revisions WHERE memory_id=OLD.id;
      END;`);
  }
  put(chat, subject, layer, slot, text, sources, importance, now, expires, metadata = {}, revisionLimit = 3) {
    const old = this.db.prepare('SELECT * FROM memory_layers WHERE chat=? AND subject=? AND layer=? AND slot=?').get(chat, subject, layer, slot);
    const previous = old ? JSON.parse(old.sources) : [];
    const identity = source => `${source.sender}:${source.id}`;
    const known = new Set(previous.map(identity));
    if (old && layer !== 'short_term' && sources.length && Math.max(...sources.map(s => s.ts)) < Math.max(0, ...previous.map(s => s.ts))) return;
    const fresh = sources.some(source => !known.has(identity(source)));
    // Re-reading the same evidence cannot keep an old claim alive forever.
    if (old && !fresh && old.text === text) return;
    if (old && layer !== 'short_term' && old.text !== text) {
      this.db.prepare('INSERT OR REPLACE INTO memory_revisions VALUES(?,?,?,?,?,?)').run(old.id, old.revision, old.text, old.sources, old.updated, now);
      this.db.prepare('DELETE FROM memory_revisions WHERE memory_id=? AND revision NOT IN (SELECT revision FROM memory_revisions WHERE memory_id=? ORDER BY revision DESC LIMIT ?)').run(old.id, old.id, revisionLimit);
    }
    const evidence = new Map(previous.map(source => [identity(source), source]));
    for (const source of sources) evidence.set(identity(source), source);
    const merged = [...evidence.values()].sort((a,b) => b.ts-a.ts).slice(0,12);
    const updated = old && !fresh ? old.updated : now;
    const expiry = old && !fresh ? old.expires : expires;
    this.db.prepare(`INSERT INTO memory_layers(id,chat,subject,layer,slot,text,sources,importance,created,updated,expires,revision,keywords,confidence)
      VALUES(?,?,?,?,?,?,?,?,?,?,?,1,?,?) ON CONFLICT(chat,subject,layer,slot) DO UPDATE SET
      text=excluded.text,sources=excluded.sources,importance=excluded.importance,updated=excluded.updated,
      expires=excluded.expires,revision=revision+1,keywords=excluded.keywords,confidence=excluded.confidence`).run(
      randomUUID(),chat,subject,layer,slot,text,JSON.stringify(merged),importance,now,updated,expiry,
      JSON.stringify(metadata.keywords || []),metadata.confidence ?? .6);
  }
  capture(message, now, settings) {
    if (message.self) return;
    const subjects = memorySubjects(message.chat, message.sender);
    for (const subject of subjects) {
      this.put(message.chat, subject, 'short_term', message.id,
        `${message.name} (${message.sender}): ${message.text}`.slice(0, settings.shortChars),
        [{ id: message.id, sender: message.sender, ts: message.ts }], 0.5, now, now + settings.shortHours * 3600, { confidence: 1 });
    }
    this.enforce(message.chat, now, settings, subjects);
  }
  apply(chat, updates, now, settings) {
    for (const v of updates) {
      if (v.operation === 'forget') this.db.prepare('DELETE FROM memory_layers WHERE chat=? AND subject=? AND layer=? AND slot=?').run(chat, v.subject, v.layer, v.key);
      else this.put(chat, v.subject, v.layer, v.key, v.text, v.sources, v.importance, now,
        now + (v.layer === 'long_term' ? settings.longDays : settings.traitDays) * 86400, v, settings.revisionLimit);
    }
    this.enforce(chat, now, settings, [...new Set(updates.map(v => v.subject))]);
  }
  rows(chat, subject, layer, now) {
    return this.db.prepare('SELECT * FROM memory_layers WHERE chat=? AND subject=? AND layer=? AND expires>? ORDER BY importance DESC,updated DESC,rowid DESC').all(chat, subject, layer, now)
      .map(r => ({ ...r, sources: JSON.parse(r.sources), keywords: JSON.parse(r.keywords) }));
  }
  context(chat, sender, now, settings, query = '') {
    const subjects = memorySubjects(chat, sender), output = subjects.map(subject => ({ subject, long_term: [], traits: [] }));
    const rows = subjects.flatMap(subject => ['long_term','traits'].flatMap(layer => this.rows(chat, subject, layer, now)
      .filter(r => r.updated + (layer === 'long_term' ? settings.longDays : settings.traitDays) * 86400 > now)));
    let used = 0;
    for (const row of rankMemories(rows, query, now, settings)) {
      const target = output.find(s => s.subject === row.subject)[row.layer];
      const limit = row.layer === 'long_term' ? settings.longChars : settings.traitChars;
      if (used + row.text.length > settings.recallChars || target.length >= 24 || target.reduce((n,r) => n+r.text.length,0) + row.text.length > limit) continue;
      target.push(row); used += row.text.length;
    }
    return output;
  }
  bounded(rows, chars) {
    let used = 0, count = 0;
    return rows.filter(r => { if (count >= 24 || used + r.text.length > chars) return false; used += r.text.length; count++; return true; });
  }
  short(chat, sender, now, settings, excluded = []) {
    const seen = new Set(excluded), result = [];
    // Personal entries take priority when the group view includes the same message.
    for (const subject of memorySubjects(chat, sender).reverse()) {
      for (const r of this.rows(chat, subject, 'short_term', now).filter(r => r.updated + settings.shortHours * 3600 > now).slice(0, settings.shortLimit)) {
        if (seen.has(r.slot)) continue;
        seen.add(r.slot); result.push(r);
      }
    }
    return result;
  }
  enforce(chat, now, settings, subjectsToCheck = null) {
    this.db.prepare(`UPDATE memory_layers SET expires=min(expires,updated+CASE layer WHEN 'short_term' THEN ? WHEN 'long_term' THEN ? ELSE ? END) WHERE chat=?`).run(settings.shortHours * 3600, settings.longDays * 86400, settings.traitDays * 86400, chat);
    this.db.prepare('DELETE FROM memory_layers WHERE chat=? AND expires<=?').run(chat, now);
    for (const r of this.db.prepare('SELECT id FROM memory_layers WHERE chat=?').all(chat)) this.db.prepare('DELETE FROM memory_revisions WHERE memory_id=? AND revision NOT IN (SELECT revision FROM memory_revisions WHERE memory_id=? ORDER BY revision DESC LIMIT ?)').run(r.id,r.id,settings.revisionLimit);
    for (const { subject } of this.db.prepare('SELECT DISTINCT subject FROM memory_layers WHERE chat=?').all(chat)) {
      if (subjectsToCheck && !subjectsToCheck.includes(subject)) continue;
      for (const layer of ['short_term', 'long_term', 'traits']) {
        const rows = this.rows(chat, subject, layer, now);
        const keep = layer === 'short_term' ? rows.slice(0, settings.shortLimit) : this.bounded(rows, layer === 'long_term' ? settings.longChars : settings.traitChars);
        const ids = new Set(keep.map(r => r.id));
        for (const r of rows) if (!ids.has(r.id)) this.db.prepare('DELETE FROM memory_layers WHERE id=?').run(r.id);
      }
    }
    const subjects = this.db.prepare("SELECT subject,max(updated) AS latest FROM memory_layers WHERE chat=? AND subject<>'group' GROUP BY subject ORDER BY latest DESC,subject").all(chat);
    for (const r of subjects.slice(settings.maxPeople)) this.db.prepare('DELETE FROM memory_layers WHERE chat=? AND subject=?').run(chat, r.subject);
  }
  configure(now, settings) {
    for (const { chat } of this.db.prepare('SELECT DISTINCT chat FROM memory_layers').all()) this.enforce(chat, now, settings);
  }
  reset(chat, subject = null) {
    if (subject === null) this.db.prepare('DELETE FROM memory_layers WHERE chat=?').run(chat);
    else this.db.prepare('DELETE FROM memory_layers WHERE chat=? AND subject=?').run(chat, subject);
  }
}
