import test from 'node:test';
import assert from 'node:assert/strict';
import { defaults, merge, validate, loadConfig } from '../src/config.mjs';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { Store, similarity } from '../src/store.mjs';
import { normalize, select, quiet, activeAt } from '../src/policy.mjs';
import { Provider, endpoint, parseObject, listModels } from '../src/provider.mjs';
import { Engine } from '../src/engine.mjs';

const now = 1_000_000;
test('activity schedule validates, uses local minutes, and supports overnight windows', () => {
  const schedule = { enabled: true, activeStart: '08:30', inactiveStart: '22:00', timezone: 'UTC' };
  const ts = t => Date.parse(`2026-01-01T${t}:00Z`) / 1000;
  assert.equal(activeAt(ts('08:29'), schedule), false);
  assert.equal(activeAt(ts('08:30'), schedule), true);
  assert.equal(activeAt(ts('22:00'), schedule), false);
  assert.equal(activeAt(ts('07:30'), { ...schedule, timezone: 'Europe/Stockholm' }), true);
  assert.equal(activeAt(ts('23:00'), { ...schedule, activeStart: '22:00', inactiveStart: '06:00' }), true);
  assert.equal(activeAt(ts('05:59'), { ...schedule, activeStart: '22:00', inactiveStart: '06:00' }), true);
  assert.equal(activeAt(ts('06:00'), { ...schedule, activeStart: '22:00', inactiveStart: '06:00' }), false);
  assert.equal(activeAt(ts('00:00'), { ...schedule, enabled: false }), true);
  assert.throws(() => validate(merge(defaults, { agent: { schedule: { activeStart: '24:00' } } })));
  assert.throws(() => validate(merge(defaults, { agent: { schedule: { activeStart: '23:00' } } })));
  assert.throws(() => validate(merge(defaults, { agent: { schedule: { timezone: 'not/a-zone' } } })));
});
test('inactive schedule suppresses direct requests and drops queued work at cutoff', async () => {
  const f = fixture();
  try {
    f.engine.ingest(event('scheduled', 'Hello', true));
    f.c.agent.schedule = { enabled: true, activeStart: '00:00', inactiveStart: '00:01', timezone: 'UTC' };
    f.engine.tick(); await f.engine.cycle('group:10');
    assert.equal(f.calls.length, 0); assert.equal(f.sent.length, 0);
    assert.equal(f.engine.chats.get('group:10').pending, false);
    f.engine.ingest(event('inactive-message', 'Do not queue me', true));
    assert.equal(f.engine.chats.get('group:10').lastId, 'scheduled');
  } finally { f.store.close(); }
});
test('crossing into inactive hours during generation prevents sending', async () => {
  const f = fixture(); let clock = now;
  f.engine.now = () => clock;
  f.c.agent.schedule = { enabled: true, activeStart: '13:00', inactiveStart: '14:00', timezone: 'UTC' };
  const original = f.provider.json;
  f.provider.json = async (...args) => { const value = await original(...args); if (args[0].includes('TASK: ARTICULATE')) clock += 3600; return value; };
  try { f.engine.ingest(event()); await f.engine.cycle('group:10'); assert.equal(f.calls.length, 3); assert.equal(f.sent.length, 0); }
  finally { f.store.close(); }
});
test('model listing uses saved provider paths and authentication without inference', async () => {
  for (const [kind, baseUrl, url, auth] of [
    ['openai', 'https://gateway.example/v1/chat/completions', 'https://gateway.example/v1/models', 'Authorization'],
    ['anthropic', 'https://gateway.example', 'https://gateway.example/v1/models', 'x-api-key'],
    ['anthropic', 'https://api.deepseek.com/anthropic', 'https://api.deepseek.com/models', 'Authorization'],
  ]) {
    const c = { ...defaults.provider, kind, baseUrl };
    const models = await listModels(c, 'test-key', async (actual, opts) => {
      assert.equal(actual, url); assert.ok(opts.headers[auth].includes('test-key')); assert.equal(opts.redirect, 'error');
      return new Response(JSON.stringify({ data: [{ id: 'b' }, { id: 'a' }, { id: 'b' }, {}] }));
    });
    assert.deepEqual(models, ['a', 'b']);
  }
  await assert.rejects(listModels(defaults.provider, ''), /save_api_key/);
  await assert.rejects(listModels(defaults.provider, 'key', async () => new Response('{}', { status: 404 })), /models_http_404/);
});
test('DeepSeek configuration never borrows an unrelated OpenAI/Anthropic API key', () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'qq-config-test-'));
  const keys = ['LLM_API_KEY', 'DEEPSEEK_API_KEY', 'OPENAI_API_KEY', 'ANTHROPIC_API_KEY'];
  const saved = Object.fromEntries(keys.map(k => [k, process.env[k]]));
  try {
    for (const k of keys) delete process.env[k];
    process.env.OPENAI_API_KEY = 'unrelated-openai-key';
    process.env.ANTHROPIC_API_KEY = 'unrelated-anthropic-key';
    fs.writeFileSync(path.join(root, 'config.json'), JSON.stringify({ provider: { baseUrl: 'https://api.deepseek.com' } }));
    assert.equal(loadConfig(root).apiKey, '');
    process.env.DEEPSEEK_API_KEY = 'deepseek-key';
    assert.equal(loadConfig(root).apiKey, 'deepseek-key');
  } finally {
    for (const k of keys) { if (saved[k] === undefined) delete process.env[k]; else process.env[k] = saved[k]; }
    fs.rmSync(root, { recursive: true, force: true });
  }
});
function config() {
  return merge(defaults, { apiKey: 'test-only', provider: { model: 'test' }, agent: { sending: { enabled: false }, allowedGroups: ['10'], allowedUsers: ['20'], quietHours: null } });
}
function event(id = '1', text = 'How can we improve our garden?', at = false) {
  return { post_type: 'message', message_type: 'group', self_id: 99, user_id: 20, group_id: 10,
    time: now, message_id: id, sender: { nickname: 'A' },
    message: [...(at ? [{ type: 'at', data: { qq: '99' } }] : []), { type: 'text', data: { text } }] };
}
function fixture(options = {}) {
  const c = merge(config(), options.config), store = new Store(':memory:');
  const sent = [], calls = [];
  const transport = { selfId: '99', connected: true, online: true, send: async (chat, text) => { sent.push({ chat, text }); return { message_id: 200 }; } };
  const provider = { json: async (system, payload) => {
    calls.push(system);
    if (system.includes('TASK: FORM')) return { allocation: 'open', candidates: [{ kind: 'system2', text: 'Suggest planting native flowers.' }] };
    if (system.includes('TASK: EVALUATE')) return { ratings: payload.candidates.map(x => ({ id: x.id, motivation: options.score ?? 4.6, relevance: 5, originality: 5, for: ['relevance'], against: [] })) };
    return { text: 'Native flowers could attract more pollinators.' };
  } };
  const engine = new Engine(c, store, provider, transport, { now: () => now });
  return { c, store, sent, calls, provider, transport, engine };
}

test('configuration rejects insecure remote endpoints and bad limits', () => {
  assert.throws(() => validate(merge(defaults, { onebot: { url: 'ws://remote.example/' } })));
  assert.throws(() => validate(merge(defaults, { agent: { threshold: 9 } })));
  assert.throws(() => validate(merge(defaults, { provider: { baseUrl: 'https://api.example/v1?key=secret' } })));
  assert.equal(validate(config()).agent.allowedGroups[0], '10');
});
test('allowlist, self messages, ignored users and old history are filtered', () => {
  const a = config().agent;
  assert.equal(normalize({ ...event(), group_id: 11 }, '99', a, now), null);
  assert.equal(normalize({ ...event(), user_id: 99 }, '99', a, now), null);
  assert.equal(normalize({ ...event(), time: now - 1000 }, '99', a, now), null);
  assert.equal(normalize(event(), '99', { ...a, ignoredUsers: ['20'] }, now), null);
  assert.equal(normalize(event('1', 'Hello', true), '99', a, now).hint, 'self');
  assert.equal(normalize({ ...event(), message: '[CQ:at,qq=30] Hi &#91;test&#93;' }, '99', a, now).hint, 'other');
});
test('open turn, interruption threshold, direct allocation and System 1 fallback', () => {
  const a = config().agent;
  const thought = { kind: 'system2', motivation: 4.2, relevance: 5, originality: 5 };
  assert.ok(select([thought], 'open', a));
  assert.equal(select([thought], 'other', a), null);
  assert.ok(select([{ ...thought, motivation: 2 }], 'self', a));
  assert.equal(select([{ ...thought, originality: 1 }], 'open', a), null);
  assert.ok(select([{ ...thought, kind: 'system1', motivation: 2 }], 'open', { ...a, system1Probability: 1 }, 0, () => 0));
});
test('quiet hours cross midnight in configured timezone', () => {
  const hours = { start: 23, end: 8, timezone: 'UTC' };
  assert.equal(quiet(Date.parse('2026-01-01T23:00:00Z') / 1000, hours), true);
  assert.equal(quiet(Date.parse('2026-01-01T07:59:00Z') / 1000, hours), true);
  assert.equal(quiet(Date.parse('2026-01-01T08:00:00Z') / 1000, hours), false);
});
test('store deduplicates and isolates chat memories, handles Chinese similarity', () => {
  const s = new Store(':memory:');
  const m = { chat: 'group:10', id: '1', sender: '20', name: 'A', text: 'native flowers garden', ts: now };
  assert.equal(s.message(m), true); assert.equal(s.message(m), false);
  s.note('private:20', 'secret garden plan', now);
  assert.ok(s.retrieve('group:10', 'garden', now).every(x => !x.text.includes('secret')));
  assert.ok(similarity('今天去公园散步', '我们去公园散步') > 0.2);
  assert.equal(s.callBudget(now, 1), true); assert.equal(s.callBudget(now, 1), false);
  assert.equal(s.callBudget(now + 3601, 1), true); s.close();
});
test('full decision pipeline sends once and ignores replay/self echoes', async () => {
  const f = fixture(); f.engine.ingest(event()); f.engine.ingest(event());
  assert.equal(f.engine.chats.get('group:10').version, 1);
  await f.engine.cycle('group:10');
  assert.equal(f.sent.length, 1); assert.equal(f.calls.length, 3);
  assert.equal(f.store.counts('group:10', now).proactive, 1);
  f.engine.ingest({ ...event('200'), user_id: 99 });
  assert.equal(f.engine.chats.get('group:10').pending, false); f.store.close();
});
test('low motivation is withheld and retained for future reevaluation', async () => {
  const f = fixture({ score: 2 }); f.engine.ingest(event());
  await f.engine.cycle('group:10');
  assert.equal(f.sent.length, 0); assert.equal(f.calls.length, 2);
  assert.equal(f.store.reservoir('group:10', now, 1800, 12).length, 1);
  f.store.close();
});
test('new input while articulating discards stale response and stays pending', async () => {
  const f = fixture(); const original = f.provider.json;
  f.provider.json = async (sys, payload) => {
    const result = await original(sys, payload);
    if (sys.includes('ARTICULATE')) f.engine.ingest(event('2', 'Actually we cancelled the garden project.'));
    return result;
  };
  f.engine.ingest(event()); await f.engine.cycle('group:10');
  assert.equal(f.sent.length, 0); assert.equal(f.engine.chats.get('group:10').pending, true); f.store.close();
});
test('ambiguous send is recorded and not automatically retried', async () => {
  const f = fixture(); f.transport.send = async () => { throw Object.assign(Error('timeout'), { uncertain: true, code: 'action_timeout' }); };
  f.engine.ingest(event()); await f.engine.cycle('group:10');
  assert.equal(f.store.db.prepare('SELECT status FROM deliveries').get().status, 'uncertain');
  assert.equal(f.engine.chats.get('group:10').pauseDone, true);
  assert.equal(f.engine.chats.get('group:10').pending, false); f.store.close();
});
test('proactive cooldown blocks another send but permits direct questions', async () => {
  const f = fixture(); const delivery = f.store.delivery('group:10', true, now - 20); f.store.finishDelivery(delivery, 'sent');
  f.engine.ingest(event()); await f.engine.cycle('group:10'); assert.equal(f.calls.length, 0);
  f.engine.ingest(event('2', 'Tell me more', true)); await f.engine.cycle('group:10');
  assert.equal(f.sent.length, 1); f.store.close();
});
test('dry run never sends and all-chat empty configuration never triggers API', async () => {
  const f = fixture({ config: { agent: { dryRun: true } } });
  f.engine.ingest(event()); await f.engine.cycle('group:10'); assert.equal(f.sent.length, 0); f.store.close();
  const g = fixture({ config: { agent: { allowedGroups: [], allowedUsers: [] } } });
  g.engine.ingest(event()); g.engine.tick(); assert.equal(g.calls.length, 0); g.store.close();
});
test('invalid model ratings fail closed', async () => {
  const f = fixture({ score: 8 }); f.engine.ingest(event());
  await assert.rejects(() => f.engine.cycle('group:10'), /Invalid ratings/);
  assert.equal(f.sent.length, 0); f.store.close();
});
test('provider URL normalization and strict JSON extraction', () => {
  assert.equal(endpoint('https://api.deepseek.com', 'openai'), 'https://api.deepseek.com/chat/completions');
  assert.equal(endpoint('https://api.deepseek.com/anthropic', 'anthropic'), 'https://api.deepseek.com/anthropic/v1/messages');
  assert.equal(endpoint('https://example.test/v1/', 'anthropic'), 'https://example.test/v1/messages');
  assert.equal(endpoint('https://example.test/v1/messages', 'anthropic'), 'https://example.test/v1/messages');
  assert.deepEqual(parseObject('```json\n{"ok":true}\n```'), { ok: true });
  assert.throws(() => parseObject('some explanation {"ok":true}'));
});
test('both API formats, auth headers and extraction (never exposes reasoning blocks)', async () => {
  for (const kind of ['openai', 'anthropic']) {
    const s = new Store(':memory:'); let request;
    const c = { ...defaults.provider, kind, model: 'test', thinking: 'disabled' };
    const p = new Provider(c, 'fake-key', s, { fetch: async (url, init) => {
      request = { url, ...init, body: JSON.parse(init.body) };
      return new Response(JSON.stringify(kind === 'openai' ? { choices: [{ message: { content: '{"ok":true}', reasoning_content: 'private' } }] } : { content: [{ type: 'thinking', thinking: 'private' }, { type: 'text', text: '{"ok":true}' }] }));
    } });
    assert.deepEqual(await p.json('system', { input: 'hello' }), { ok: true });
    assert.equal(request.headers[kind === 'openai' ? 'Authorization' : 'x-api-key'], kind === 'openai' ? 'Bearer fake-key' : 'fake-key');
    assert.equal(request.body.thinking.type, 'disabled');
    if (kind === 'anthropic') { assert.equal(request.body.system, 'system'); assert.equal(request.headers['anthropic-version'], '2023-06-01'); }
    s.close();
  }
});
test('429 retries with budget; authentication failure is not retried or leaked', async () => {
  const s = new Store(':memory:'); let attempts = 0;
  const p = new Provider({ ...defaults.provider, model: 'test' }, 'SECRET', s, { sleep: async () => {}, fetch: async () => {
    attempts++; return attempts === 1 ? new Response('', { status: 429, headers: { 'retry-after': '1' } }) : new Response('{"choices":[{"message":{"content":"ok"}}]}');
  } });
  assert.equal(await p.complete('s', 'u'), 'ok'); assert.equal(attempts, 2);
  const q = new Provider(defaults.provider, 'SECRET', s, { fetch: async () => new Response('SECRET', { status: 401 }) });
  await assert.rejects(() => q.complete('s', 'u'), e => e.code === 'http_401_check_provider_config' && !e.message.includes('SECRET'));
  await assert.rejects(() => q.complete('s', 'u'), /provider_backoff/); s.close();
});
