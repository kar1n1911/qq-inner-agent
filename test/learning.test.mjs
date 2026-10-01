import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { defaults, merge, validate } from '../src/config.mjs';
import { Engine } from '../src/engine.mjs';
import { Store } from '../src/store.mjs';
import { parseLearning } from '../src/learning.mjs';

const message = { chat: 'private:20', id: '1', sender: '20', name: '小明', text: '我喜欢园艺，回答短一点就好', ts: 1000, self: false };
const result = { style: { text: '接话简短，以园艺话题自然联想。', sourceIds: ['1'] }, memories: [{ text: '用户20明确说喜欢园艺。', sourceIds: ['1'] }], forgetIds: [] };
function fixture(enabled = true) {
  const store = new Store(':memory:'), calls = [];
  const c = merge(defaults, { agent: { allowedUsers: ['20'], quietHours: null, sending: { enabled: false }, learning: { enabled, minMessages: 1, intervalSeconds: 30 } } });
  const provider = { json: async (sys, payload) => {
    calls.push({ sys, payload: structuredClone(payload) });
    if (sys.includes('TASK: FORM')) return { allocation: 'self', candidates: [{ kind: 'system2', text: '聊聊园艺' }], learning: { layers: [{ subject: 'person:20', layer: 'traits', key: '互动风格', operation: 'upsert', text: result.style.text, importance: 0.8, sourceIds: ['1'] }] } };
    if (sys.includes('TASK: EVALUATE')) return { ratings: payload.candidates.map(m => ({ id: m.id, motivation: 5, relevance: 5, originality: 5 })) };
    return { text: '你最近种了什么？' };
  } };
  const engine = new Engine(c, store, provider, { selfId: '99', connected: true, online: true, send: async () => ({ message_id: 90 }) }, { now: () => 1000 });
  engine.ingest({ post_type: 'message', message_type: 'private', self_id: 99, user_id: 20, message_id: '1', time: 1000, message: message.text });
  return { store, c, calls, provider, engine };
}
test('social persona migrates exact old defaults but preserves user persona', () => {
  assert.match(defaults.agent.persona, /接话/);
  assert.equal(validate(merge(defaults, { agent: { persona: '我的自定义角色' } })).agent.persona, '我的自定义角色');
  for (const learning of [{ enabled: 'yes' }, { minMessages: 0 }, { maxMemories: 9999 }, { retrievalLimit: 0 }]) assert.throws(() => validate(merge(defaults, { agent: { learning } })));
});
test('learning accepts actual human sources and rejects missing, invented or self sources', () => {
  const update = parseLearning(result, [message]);
  assert.equal(update.memories[0].sources[0].sender, '20');
  for (const history of [[], [{ ...message, id: 'other' }], [{ ...message, self: true }]]) assert.throws(() => parseLearning(result, history));
  assert.throws(() => parseLearning({ ...result, style: { text: 'a'.repeat(601), sourceIds: ['1'] } }, [message]));
});
test('learning updates effective chat style and grounds articulation without an extra API request', async () => {
  const f = fixture();
  try {
    await f.engine.cycle('private:20');
    assert.equal(f.calls.length, 3); assert.equal(f.calls[0].payload.learning.requested, true);
    assert.equal(f.calls[2].payload.chatStyle[0].traits[0].text, result.style.text);
    assert.equal(f.calls[2].payload.memoryContext[0].traits[0].subject, 'person:20');
    assert.equal(f.store.memory.context('private:20', '20', 1000, f.c.agent.memory)[0].traits[0].text, result.style.text);
    assert.equal(f.store.learningState('private:21').style, '');
  } finally { f.store.close(); }
});
test('disabled and premature learning do not write model-supplied updates', async () => {
  for (const enabled of [false, true]) {
    const f = fixture(enabled); if (enabled) f.c.agent.learning.minMessages = 8;
    try { await f.engine.cycle('private:20'); assert.equal(f.calls[0].payload.learning.requested, false); assert.equal(f.store.learningState('private:20').style, ''); }
    finally { f.store.close(); }
  }
});
test('RAG retrieves older Chinese memories, excludes active context, isolates chats and expires entries', () => {
  const store = new Store(':memory:'), settings = { ...defaults.agent.learning, maxMemories: 1 };
  try {
    store.message(message);
    store.learn('private:20', parseLearning(result, [message]), 1000, '1', settings, 0);
    const found = store.retrieve('private:20', '园艺', 1010, { excludeIds: ['1'] });
    assert.equal(found.length, 1); assert.equal(found[0].type, 'learned_memory');
    assert.equal(store.retrieve('private:21', '园艺', 1010).length, 0);
    assert.equal(store.retrieve('private:20', '园艺', 1010, { excludeIds: ['1'], learned: false }).length, 0);
    const replacement = { ...result, memories: [{ text: '用户20喜欢番茄园艺', sourceIds: ['1'] }] };
    store.learn('private:20', parseLearning(replacement, [message]), 1020, '1', settings, 0);
    assert.equal(store.db.prepare('SELECT count(*) AS n FROM learned_memories').get().n, 1);
    assert.equal(store.retrieve('private:20', '园艺', 1020 + 31 * 86400, { excludeIds: ['1'] }).length, 0);
  } finally { store.close(); }
});
test('reset persists across reopen and prevents in-flight updates from restoring erased learning', () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'qq-learning-')), file = path.join(root, 'db');
  let store = new Store(file);
  try {
    store.message(message); store.learn('private:20', parseLearning(result, [message]), 1000, '1', defaults.agent.learning, 0);
    store.close(); store = new Store(file); assert.equal(store.learningState('private:20').style, result.style.text);
    store.resetLearning('private:20', 1010);
    assert.equal(store.learn('private:20', parseLearning(result, [message]), 1020, '1', defaults.agent.learning, 0), false);
    assert.equal(store.learningState('private:20').style, ''); assert.equal(store.history('private:20').length, 1);
  } finally { store.close(); fs.rmSync(root, { recursive: true, force: true }); }
});
test('reset while model is generating discards old context instead of relearning it', async () => {
  const f = fixture(), original = f.provider.json;
  f.provider.json = async (...args) => { const r = await original(...args); f.store.resetLearning('private:20', 1000); return r; };
  try { await f.engine.cycle('private:20'); assert.equal(f.calls.length, 1); assert.equal(f.store.learningState('private:20').style, ''); }
  finally { f.store.close(); }
});
