import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { Store } from '../src/store.mjs';
import { defaults, merge, validate } from '../src/config.mjs';
import { memorySubjects, parseMemoryUpdates } from '../src/memory.mjs';
import { Engine } from '../src/engine.mjs';
const settings = defaults.agent.memory;
const a = { chat: 'group:10', id: 'a', sender: '20', name: '同名', text: '我喜欢园艺', ts: 1000, self: false };
const b = { ...a, id: 'b', sender: '21', text: '我喜欢音乐' };
const update = (subject, layer, key, text, sourceIds = ['a']) => ({ subject, layer, key, text, sourceIds, operation: 'upsert', importance: 0.8 });

test('scope selection separates people, groups and private contexts using IDs rather than names', () => {
  assert.deepEqual(memorySubjects('group:10', '20'), ['group', 'person:20']);
  assert.deepEqual(memorySubjects('private:20', '20'), ['person:20']);
  assert.throws(() => memorySubjects('private:20', '21'));
  for (const memory of [{ shortLimit: 0 }, { longChars: 1 }, { maxPeople: 1.1 }, { shortHours: Infinity }]) assert.throws(() => validate(merge(defaults, { agent: { memory } })));
});
test('personal updates reject other speakers and group claims need multiple authors', () => {
  const parse = v => parseMemoryUpdates([v], [a, b], 'group:10', '20', settings);
  assert.equal(parse(update('person:20', 'traits', '兴趣', '园艺'))[0].sources[0].sender, '20');
  assert.throws(() => parse(update('person:20', 'traits', '兴趣', '音乐', ['b'])), /author/);
  assert.throws(() => parse(update('person:21', 'traits', '兴趣', '音乐', ['b'])));
  assert.throws(() => parse(update('group', 'traits', '主题', '园艺')));
  assert.equal(parse(update('group', 'traits', '主题', '有人讨论园艺，也有人讨论音乐', ['a', 'b']))[0].sources.length, 2);
  assert.throws(() => parseMemoryUpdates([update('person:20', 'traits', '兴趣', '园艺')], [{ ...a, chat: 'group:11' }], 'group:10', '20', settings));
  assert.throws(() => parseMemoryUpdates([update('person:20', 'traits', '兴趣', '园艺')], [{ ...a, self: true }], 'group:10', '20', settings));
});
test('retrieval cannot load another person notebook or a notebook from another group/private chat', () => {
  const s = new Store(':memory:');
  try {
    for (const [chat, subject, text] of [['group:10','person:20','甲的园艺'],['group:10','person:21','乙的音乐'],['group:11','person:20','别群历史'],['private:20','person:20','私聊秘密'],['group:10','group','当前群体主题']]) {
      s.memory.apply(chat, [{ ...update(subject, 'long_term', '笔记', text), sources: [] }], 1000, settings);
    }
    const value = JSON.stringify(s.memory.context('group:10', '20', 1001, settings));
    assert.match(value, /甲的园艺/); assert.match(value, /当前群体主题/);
    for (const forbidden of ['乙的音乐','别群历史','私聊秘密']) assert.ok(!value.includes(forbidden));
    assert.ok(!JSON.stringify(s.memory.context('private:20', '20', 1001, settings)).includes('甲的园艺'));
  } finally { s.close(); }
});
test('short-term memory retains detailed attributed messages, is deduplicated and expires independently', () => {
  const s = new Store(':memory:');
  try {
    s.memory.capture(a, 1000, settings); s.memory.capture(b, 1001, settings);
    const rows = s.memory.short('group:10', '20', 1002, settings);
    assert.equal(rows.length, 2); assert.equal(rows.find(r => r.slot === 'b').sources[0].sender, '21');
    assert.equal(rows.find(r => r.slot === 'a').subject, 'person:20');
    assert.equal(s.memory.short('group:10', '20', 1002, settings, ['a']).length, 1);
    assert.equal(s.memory.short('group:10', '20', 1002 + settings.shortHours * 3600, settings).length, 0);
    assert.equal(s.memory.short('group:11', '20', 1002, settings).length, 0);
  } finally { s.close(); }
});
test('selective notebook updates retain untouched topics, revise in place, forget and enforce character budgets', () => {
  const s = new Store(':memory:');
  const apply = (v, now, options = settings) => s.memory.apply('group:10', [{ ...v, sources: [{ id: 'a', sender: '20', ts: now }] }], now, options);
  try {
    apply(update('person:20', 'long_term', '约定', '周日种花'), 1000);
    apply(update('person:20', 'long_term', '进展', '花已经发芽'), 1001);
    apply(update('person:20', 'long_term', '约定', '改为周六种花'), 1002);
    let rows = s.memory.context('group:10', '20', 1003, settings)[1].long_term;
    assert.equal(rows.length, 2); assert.equal(rows.find(r => r.slot === '约定').revision, 2);
    assert.equal(rows.find(r => r.slot === '进展').text, '花已经发芽');
    apply({ ...update('person:20', 'long_term', '进展', ''), operation: 'forget' }, 1004);
    assert.equal(s.memory.context('group:10', '20', 1005, settings)[1].long_term.length, 1);
    apply({ ...update('person:20', 'long_term', '闲聊', 'x'.repeat(200)), importance: 0.1 }, 1006, { ...settings, longChars: 200 });
    rows = s.memory.context('group:10', '20', 1007, settings)[1].long_term;
    assert.equal(rows.length, 1); assert.equal(rows[0].slot, '约定');
  } finally { s.close(); }
});
test('long and trait memory outlive raw-history retention but honor their own expiry', () => {
  const s = new Store(':memory:');
  try {
    s.message(a); s.memory.capture(a, 1000, settings);
    s.memory.apply('group:10', ['long_term','traits'].map(layer => ({ ...update('person:20', layer, '园艺', '喜欢园艺'), sources: [] })), 1000, settings);
    s.prune(1000 + 31 * 86400, 30, 500);
    assert.equal(s.history('group:10').length, 0);
    assert.equal(s.memory.context('group:10', '20', 1000 + 31 * 86400, settings)[1].long_term.length, 1);
    s.prune(1000 + 181 * 86400, 30, 500);
    assert.equal(s.memory.context('group:10', '20', 1000 + 181 * 86400, settings)[1].traits.length, 0);
    assert.equal(s.memory.context('group:10', '20', 1000 + 366 * 86400, settings)[1].long_term.length, 0);
  } finally { s.close(); }
});
test('scope reset and reopen preserve other people and invalidate in-flight learning', () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'layered-memory-')); let s = new Store(path.join(root, 'db'));
  try {
    s.memory.capture(a, 1000, settings); s.memory.capture(b, 1001, settings);
    s.close(); s = new Store(path.join(root, 'db'));
    s.resetLearning('group:10', 1002, 'person:20');
    assert.equal(s.memory.rows('group:10','person:20','short_term',1003).length, 0);
    assert.equal(s.memory.rows('group:10','person:21','short_term',1003).length, 1);
    assert.equal(s.learn('group:10', { style: null, memories: [], forgetIds: [] }, 1003, 'b', defaults.agent.learning, 0, { updates: [], settings }), false);
  } finally { s.close(); fs.rmSync(root, { recursive: true, force: true }); }
});
test('live limits cap short details and people, and shortening retention applies to existing entries', () => {
  const s = new Store(':memory:'), small = { ...settings, shortLimit: 1, maxPeople: 1 };
  try {
    s.memory.capture(a, 1000, small); s.memory.capture({ ...a, id: 'a2' }, 1001, small);
    assert.equal(s.memory.short('group:10', '20', 1002, small).length, 1);
    s.memory.capture(b, 1003, small);
    assert.equal(s.memory.rows('group:10', 'person:20', 'short_term', 1004).length, 0);
    s.memory.configure(1003 + 3601, { ...small, shortHours: 1 });
    assert.equal(s.db.prepare('SELECT count(*) AS n FROM memory_layers').get().n, 0);
  } finally { s.close(); }
});
test('engine uses only current speaker notebooks, and retained candidate ideas cannot cross speakers', async () => {
  const s = new Store(':memory:'), calls = [];
  const c = merge(defaults, { agent: { observation: { enabled: false }, allowedGroups: ['10'], quietHours: null, sending: { enabled: false } } });
  const engine = new Engine(c, s, { json: async (system, payload) => {
    calls.push(payload);
    if (system.includes('TASK: FORM')) return { allocation: 'open', candidates: [] };
    throw Error('Unexpected evaluation of another speaker candidate');
  } }, { selfId: '99', online: true, connected: true }, { now: () => 1001 });
  try {
    s.addThought('group:10', { text: '甲的私人候选', kind: 'system2', subject: '20' }, 1000);
    s.memory.apply('group:10', [{ ...update('person:20', 'traits', '兴趣', '甲喜欢园艺'), sources: [] }], 1000, settings);
    engine.ingest({ post_type: 'message', message_type: 'group', self_id: 99, group_id: 10, user_id: 21, message_id: 'b', time: 1001, message: '说说音乐' });
    await engine.cycle('group:10');
    assert.equal(calls.length, 1); assert.equal(calls[0].retainedIdeas.length, 0);
    assert.ok(!JSON.stringify(calls[0].memoryContext).includes('甲喜欢园艺'));
  } finally { s.close(); }
});

test('memory evidence accumulates, stale evidence cannot overwrite, and duplicate reads do not extend expiry', () => {
  const s = new Store(':memory:');
  const write = (text, id, ts, at) => s.memory.apply('group:10', [{ ...update('person:20','long_term','约定',text), keywords: ['园艺'], confidence: .9, sources: [{ id, sender: '20', ts }] }], at, settings);
  try {
    write('周日种花','a',1000,1000);
    const original = s.memory.rows('group:10','person:20','long_term',1001)[0];
    write('周日种花','a',1000,2000);
    const duplicate = s.memory.rows('group:10','person:20','long_term',2001)[0];
    assert.equal(duplicate.expires, original.expires); assert.equal(duplicate.revision, 1);
    write('改成周六','b',2001,2001);
    write('旧的周日','old',999,2002);
    const revised = s.memory.rows('group:10','person:20','long_term',2003)[0];
    assert.equal(revised.text,'改成周六'); assert.equal(revised.sources.length,2); assert.equal(revised.revision,2);
    assert.deepEqual(revised.keywords,['园艺']); assert.equal(revised.confidence,.9);
    const archived = s.db.prepare('SELECT * FROM memory_revisions').all();
    assert.equal(archived.length,1); assert.equal(archived[0].text,'周日种花');
    assert.ok(!JSON.stringify(s.memory.context('group:10','20',2003,settings)).includes('周日种花'));
    s.memory.reset('group:10','person:20');
    assert.equal(s.db.prepare('SELECT count(*) AS n FROM memory_revisions').get().n,0);
  } finally { s.close(); }
});
test('revision capacity is bounded, reduced limits apply live, and expiry removes archives', () => {
  const s = new Store(':memory:');
  try {
    for (let i=0;i<8;i++) s.memory.apply('group:10',[{ ...update('person:20','traits','兴趣',`兴趣${i}`), sources:[{ id:`id${i}`,sender:'20',ts:1000+i }] }],1000+i,settings);
    assert.equal(s.db.prepare('SELECT count(*) AS n FROM memory_revisions').get().n,3);
    s.memory.configure(1010,{...settings,revisionLimit:1});
    assert.equal(s.db.prepare('SELECT count(*) AS n FROM memory_revisions').get().n,1);
    s.prune(1000+366*86400,30,500);
    assert.equal(s.db.prepare('SELECT count(*) AS n FROM memory_revisions').get().n,0);
  } finally { s.close(); }
});
test('query-aware notebook recall respects confidence, character budget and strict scopes', () => {
  const s = new Store(':memory:');
  try {
    for (const [subject,key,text,confidence,keywords] of [
      ['person:20','约定','周末参加花卉展览',.9,['园艺']],
      ['person:20','音乐','喜欢古典音乐',.9,[]],
      ['person:20','猜测','园艺园艺不确定',.1,['园艺']],
      ['person:21','园艺','乙的秘密花园',1,['园艺']]]) {
      s.memory.apply('group:10',[{...update(subject,'long_term',key,text),confidence,keywords,sources:[]}],1000,settings);
    }
    const context=s.memory.context('group:10','20',1001,{...settings,recallChars:10},'园艺');
    assert.equal(context[1].long_term[0].slot,'约定');
    const text=JSON.stringify(context);
    assert.ok(!text.includes('乙的秘密')); assert.ok(!text.includes('不确定'));
    assert.ok(context.flatMap(s=>[...s.long_term,...s.traits]).reduce((n,r)=>n+r.text.length,0)<=10);
  } finally { s.close(); }
});
test('sparse retrieval handles Chinese and rare terms without unrelated short-term filler', () => {
  const s=new Store(':memory:');
  try {
    s.memory.capture({...a,text:'周末参加园艺展览'},1000,settings);
    s.memory.capture({...b,text:'今天讨论音乐和电影'},1001,settings);
    const result=s.retrieveScoped('group:10','20','园艺展览',1002,settings);
    assert.equal(result.length,1); assert.match(result[0].text,/园艺/); assert.ok(result[0].recall.lexical>0);
    assert.equal(s.retrieveScoped('group:11','20','园艺展览',1002,settings).length,0);
    assert.equal(s.retrieveScoped('group:10','20','unrelatedxyz',1002,settings).length,0);
    assert.equal(s.retrieveScoped('group:10','20','园艺',1002,settings,{enabled:false}).length,0);
  } finally { s.close(); }
});
test('model memory metadata is optional for compatibility and strictly validated when present', () => {
  const v=update('person:20','traits','兴趣','园艺');
  const parse = v => parseMemoryUpdates([v],[a],'group:10','20',settings)[0];
  assert.equal(parse(v).confidence,.6); assert.deepEqual(parse(v).keywords,[]);
  assert.deepEqual(parse({...v,keywords:['花卉','花卉']}).keywords,['花卉']);
  for (const extra of [{confidence:NaN},{confidence:2},{keywords:['x'.repeat(33)]},{keywords:'园艺'}]) assert.throws(()=>parse({...v,...extra}));
});

test('legacy SQLite memory schema migrates without losing scoped records', async () => {
  const { DatabaseSync } = await import('node:sqlite');
  const { LayeredMemory } = await import('../src/memory.mjs');
  const db = new DatabaseSync(':memory:');
  try {
    db.exec(`CREATE TABLE memory_layers(id TEXT PRIMARY KEY,chat TEXT NOT NULL,subject TEXT NOT NULL,layer TEXT NOT NULL,slot TEXT NOT NULL,text TEXT NOT NULL,sources TEXT NOT NULL,importance REAL,created REAL,updated REAL,expires REAL,revision INTEGER DEFAULT 1,UNIQUE(chat,subject,layer,slot));
      INSERT INTO memory_layers VALUES('legacy','group:10','person:20','long_term','旧笔记','保留园艺约定','[]',0.8,1000,1000,999999,1);`);
    const memory = new LayeredMemory(db);
    const row = memory.context('group:10','20',1001,settings,'园艺')[1].long_term[0];
    assert.equal(row.id,'legacy'); assert.equal(row.text,'保留园艺约定');
    assert.equal(row.confidence,.6); assert.deepEqual(row.keywords,[]);
    new LayeredMemory(db); // repeated startup is safe
    assert.equal(memory.context('group:11','20',1001,settings).flatMap(s=>s.long_term).length,0);
  } finally { db.close(); }
});
