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
    return { subject: v.subject, layer: v.layer, key, operation: v.operation,
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
  }
  put(chat, subject, layer, slot, text, sources, importance, now, expires) {
    this.db.prepare(`INSERT INTO memory_layers VALUES(?,?,?,?,?,?,?,?,?,?,?,1)
      ON CONFLICT(chat,subject,layer,slot) DO UPDATE SET text=excluded.text,sources=excluded.sources,
      importance=excluded.importance,updated=excluded.updated,expires=excluded.expires,revision=revision+1`).run(
      randomUUID(), chat, subject, layer, slot, text, JSON.stringify(sources), importance, now, now, expires);
  }
  capture(message, now, settings) {
    if (message.self) return;
    const subjects = memorySubjects(message.chat, message.sender);
    for (const subject of subjects) {
      this.put(message.chat, subject, 'short_term', message.id,
        `${message.name} (${message.sender}): ${message.text}`.slice(0, settings.shortChars),
        [{ id: message.id, sender: message.sender, ts: message.ts }], 0.5, now, now + settings.shortHours * 3600);
    }
    this.enforce(message.chat, now, settings, subjects);
  }
  apply(chat, updates, now, settings) {
    for (const v of updates) {
      if (v.operation === 'forget') this.db.prepare('DELETE FROM memory_layers WHERE chat=? AND subject=? AND layer=? AND slot=?').run(chat, v.subject, v.layer, v.key);
      else this.put(chat, v.subject, v.layer, v.key, v.text, v.sources, v.importance, now,
        now + (v.layer === 'long_term' ? settings.longDays : settings.traitDays) * 86400);
    }
    this.enforce(chat, now, settings, [...new Set(updates.map(v => v.subject))]);
  }
  rows(chat, subject, layer, now) {
    return this.db.prepare('SELECT * FROM memory_layers WHERE chat=? AND subject=? AND layer=? AND expires>? ORDER BY importance DESC,updated DESC,rowid DESC').all(chat, subject, layer, now)
      .map(r => ({ ...r, sources: JSON.parse(r.sources) }));
  }
  context(chat, sender, now, settings) {
    return memorySubjects(chat, sender).map(subject => ({ subject,
      long_term: this.bounded(this.rows(chat, subject, 'long_term', now).filter(r => r.updated + settings.longDays * 86400 > now), settings.longChars),
      traits: this.bounded(this.rows(chat, subject, 'traits', now).filter(r => r.updated + settings.traitDays * 86400 > now), settings.traitChars),
    }));
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
