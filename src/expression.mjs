import { memorySubjects } from './memory.mjs';
import { rankMemories } from './memory-ranking.mjs';

export function parseExpressions(value, history, chat, sender) {
  if (!Array.isArray(value) || value.length > 4) throw Error('invalid_expressions');
  const subjects = new Set(memorySubjects(chat, sender));
  const evidence = new Map(history.filter(m => !m.self && m.chat === chat).map(m => [m.id,m]));
  return value.map(v => {
    if (!subjects.has(v?.subject) || !['jargon','expression'].includes(v.kind) ||
        !['term','meaning','situation','example'].every(k => typeof v[k] === 'string' && v[k].trim() && v[k].length <= (k === 'term' ? 40 : 160)) ||
        typeof v.confidence !== 'number' || !Number.isFinite(v.confidence) || v.confidence < 0 || v.confidence > 1 ||
        !Array.isArray(v.sourceIds) || !v.sourceIds.length || v.sourceIds.length > 6) throw Error('invalid_expressions');
    const sources = [...new Set(v.sourceIds)].map(id => evidence.get(id));
    if (sources.some(m => !m || (v.subject !== 'group' && `person:${m.sender}` !== v.subject))) throw Error('expression_author_mismatch');
    if (!sources.some(m => m.text.includes(v.example)) || sources.some(m => !m.text.includes(v.kind === 'jargon' ? v.term : v.example))) throw Error('expression_evidence_missing');
    return {...v, sources: sources.map(m => ({id:m.id,sender:m.sender,ts:m.ts}))};
  });
}
export class ExpressionMemory {
  constructor(db) {
    this.db=db;
    db.exec(`CREATE TABLE IF NOT EXISTS expressions(chat TEXT,subject TEXT,kind TEXT,term TEXT,meaning TEXT,situation TEXT,example TEXT,confidence REAL,sources TEXT,updated REAL,last_used REAL DEFAULT 0,PRIMARY KEY(chat,subject,kind,term));
      CREATE TABLE IF NOT EXISTS decoration_usage(chat TEXT PRIMARY KEY,ts REAL);`);
  }
  apply(chat, updates, now, settings) {
    for (const v of updates) {
      const old=this.db.prepare('SELECT * FROM expressions WHERE chat=? AND subject=? AND kind=? AND term=?').get(chat,v.subject,v.kind,v.term);
      const previous=old ? JSON.parse(old.sources) : [];
      if (old && Math.max(...v.sources.map(s=>s.ts)) < Math.max(...previous.map(s=>s.ts))) continue;
      const revised = old && old.meaning !== v.meaning;
      const merged=new Map((revised ? [] : previous).map(s=>[`${s.sender}:${s.id}`,s]));
      const fresh=v.sources.some(s=>!previous.some(p=>p.sender===s.sender&&p.id===s.id));
      if (old && !fresh) continue;
      for (const s of v.sources) merged.set(`${s.sender}:${s.id}`,s);
      this.db.prepare(`INSERT INTO expressions VALUES(?,?,?,?,?,?,?,?,?,?,0) ON CONFLICT(chat,subject,kind,term) DO UPDATE SET meaning=excluded.meaning,situation=excluded.situation,example=excluded.example,confidence=excluded.confidence,sources=excluded.sources,updated=excluded.updated`).run(chat,v.subject,v.kind,v.term,v.meaning,v.situation,v.example,v.confidence,JSON.stringify([...merged.values()].sort((a,b)=>b.ts-a.ts).slice(0,12)),now);
    }
    this.prune(now,settings);
  }
  context(chat,sender,query,now,settings,memorySettings) {
    if (!settings.useLearned) return [];
    const rows=memorySubjects(chat,sender).flatMap(subject=>this.db.prepare('SELECT * FROM expressions WHERE chat=? AND subject=? AND updated>? AND confidence>=? AND (last_used=0 OR last_used<=?)').all(chat,subject,now-settings.retentionDays*86400,settings.minConfidence,now-settings.reuseSeconds))
      .map(r=>({...r,sources:JSON.parse(r.sources),id:JSON.stringify([r.subject,r.kind,r.term]),layer:r.kind,text:`${r.term}：${r.meaning}；适用：${r.situation}`,importance:.5})).filter(r=>r.sources.length>=2 && (r.subject!=='group' || new Set(r.sources.map(s=>s.sender)).size>=2));
    return rankMemories(rows,query,now,{...memorySettings,minConfidence:settings.minConfidence},{requireMatch:true}).slice(0,settings.maxPerReply);
  }
  used(chat, rows, text, now) {
    for (const r of rows) if (text.includes(r.kind==='jargon'?r.term:r.example)) this.db.prepare('UPDATE expressions SET last_used=? WHERE chat=? AND subject=? AND kind=? AND term=?').run(now,chat,r.subject,r.kind,r.term);
  }
  prune(now,settings) {
    this.db.prepare('DELETE FROM expressions WHERE updated<=?').run(now-settings.retentionDays*86400);
    for (const {chat} of this.db.prepare('SELECT DISTINCT chat FROM expressions').all()) this.db.prepare('DELETE FROM expressions WHERE chat=? AND rowid NOT IN (SELECT rowid FROM expressions WHERE chat=? ORDER BY updated DESC,rowid DESC LIMIT ?)').run(chat,chat,settings.maxEntries);
  }
  reset(chat,subject) { if(subject==null)this.db.prepare('DELETE FROM expressions WHERE chat=?').run(chat); else this.db.prepare('DELETE FROM expressions WHERE chat=? AND subject=?').run(chat,subject); }
}
export function personalityContext(agent, random=Math.random) {
  const p=agent.personality;
  const variant=p.variants.length && random()<p.variantProbability ? p.variants[Math.floor(random()*p.variants.length)] : null;
  return { identity:agent.persona,behavior:p.behavior,replyStyle:p.replyStyle,interests:p.interests,variant };
}
export function decorationChoices(store,chat,now,settings,random=Math.random) {
  const last=store.db.prepare('SELECT ts FROM decoration_usage WHERE chat=?').get(chat)?.ts;
  if (!settings.enabled || (last!=null && now-last<settings.cooldownSeconds) || random()>=settings.probability) return {symbols:[],faceIds:[]};
  return {symbols:settings.symbols,faceIds:settings.faceIds};
}
export function decorate(response,choices,maxCharacters) {
  // Only an explicit bounded choice may become a OneBot face segment.
  const emoji=choices.symbols.includes(response.emoji) && [...response.emoji].length+1 < maxCharacters ? response.emoji : '';
  const faceId=!emoji && choices.faceIds.includes(response.faceId) ? response.faceId : null;
  const suffix=emoji && !response.text.includes(emoji) ? ` ${emoji}` : '';
  const text=[...response.text.trim()].slice(0,Math.max(0,maxCharacters-[...suffix].length)).join('')+suffix;
  return {text,faceId,decorated:!!(emoji||faceId)};
}
