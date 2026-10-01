import test from 'node:test';
import assert from 'node:assert/strict';
import { defaults, merge, validate } from '../src/config.mjs';
import { Store } from '../src/store.mjs';
import { Engine } from '../src/engine.mjs';
import { observationSatisfied, cleanOrientationSource } from '../src/orientation.mjs';

function fixture() {
  let clock = 1000, sends = 0; const calls = [], reads = [], store = new Store(':memory:');
  const config = merge(defaults, { agent: { allowedGroups: ['10'], allowedUsers: ['20'], quietHours: null, sending: { enabled: false }, observation: { minSeconds: 30, minMessages: 2 } } });
  const transport = { selfId: '99', connected: true, online: true, call: async (action, params) => {
    reads.push({ action, params });
    if (action === 'get_group_info') return { group_id: 10, group_name: '园艺交流群', member_count: 8 };
    if (action === '_get_group_notice') return [{ message: { text: '欢迎分享种植经验，避免刷屏。' } }];
    return { messages: [{ group_id: 10, user_id: 20, message_id: 3, time: 500, message: '以前聊过番茄' }] };
  }, send: async () => ({ message_id: ++sends }) };
  const provider = { json: async (sys, payload) => {
    calls.push({ sys, payload });
    if (sys.includes('TASK: ORIENT')) return { style: '简短友好，围绕园艺接话，不连续追问。', summary: '主题是园艺，公告提醒不要刷屏。', topics: ['园艺'] };
    if (sys.includes('TASK: FORM')) return { allocation: 'self', candidates: [{ kind: 'system2', text: '给个园艺建议' }] };
    if (sys.includes('TASK: EVALUATE')) return { ratings: payload.candidates.map(c => ({ id: c.id, motivation: 5, relevance: 5, originality: 5 })) };
    return { text: '先看看土壤是否干燥。' };
  } };
  const engine = new Engine(config, store, provider, transport, { now: () => clock });
  const event = id => ({ post_type: 'message', message_type: 'group', self_id: 99, user_id: 20, group_id: 10, message_id: id, time: clock, message: '[CQ:at,qq=99] 聊聊园艺吧' });
  return { config, store, transport, provider, engine, calls, reads, event, advance: n => { clock += n; }, sends: () => sends };
}
test('observation supports both/either thresholds and validates bounds', () => {
  const row = { started: 100, message_count: 20 }, c = defaults.agent.observation;
  assert.equal(observationSatisfied(row, 399, c), false); assert.equal(observationSatisfied(row, 400, c), true);
  assert.equal(observationSatisfied({ ...row, message_count: 19 }, 400, c), false);
  assert.equal(observationSatisfied(row, 100, { ...c, thresholdMode: 'either' }), true);
  for (const observation of [{ thresholdMode: 'bad' }, { minSeconds: 0 }, { minMessages: 1.2 }, { historyLimit: 101 }]) assert.throws(() => validate(merge(defaults, { agent: { observation } })));
});
test('first group message waits even when addressed, then analyzes before formation and applies selected style', async () => {
  const f = fixture();
  try {
    f.engine.ingest(f.event('a')); await f.engine.cycle('group:10');
    assert.equal(f.reads.length, 3); assert.equal(f.calls.length, 0); assert.equal(f.sends(), 0);
    f.engine.ingest(f.event('a')); f.engine.ingest({ ...f.event('self'), user_id: 99 });
    assert.equal(f.engine.orientation.get('group:10').message_count, 1);
    f.engine.ingest(f.event('b')); f.advance(29); await f.engine.cycle('group:10'); assert.equal(f.calls.length, 0);
    f.advance(1); await f.engine.cycle('group:10');
    assert.equal(f.sends(), 1); assert.equal(f.calls.length, 4); assert.match(f.calls[0].sys, /TASK: ORIENT/);
    assert.equal(f.calls[0].payload.sources.info.name, '园艺交流群');
    assert.match(f.calls[3].payload.groupOrientation.style, /园艺/);
    assert.equal(f.engine.orientation.get('group:10').message_count, 2);
    assert.ok(!f.store.history('group:10').some(m => m.id === '3'));
    const restored = new Engine(f.config, f.store, f.provider, f.transport, { now: () => 1031 });
    assert.equal(restored.orientation.get('group:10').status, 'ready');
    assert.equal(await restored.orientation.beforeSpeak('group:10'), true); assert.equal(f.reads.length, 3);
    f.store.prune(1031 + 31 * 86400, 30, 500);
    assert.equal(JSON.parse(restored.orientation.get('group:10').sources).history, undefined);
    assert.equal(restored.orientation.get('group:10').status, 'ready');
  } finally { f.store.close(); }
});
test('unsupported announcements/history are explicit missing data and live messages can still establish style', async () => {
  const f = fixture(); f.transport.call = async () => { throw Error('unsupported secret token'); };
  try {
    f.engine.ingest(f.event('a')); f.engine.ingest(f.event('b')); f.advance(30); await f.engine.cycle('group:10');
    assert.equal(f.sends(), 1); assert.equal(f.calls[0].payload.sources.availability.notices, 'unavailable');
    assert.ok(!f.engine.orientation.get('group:10').sources.includes('secret'));
  } finally { f.store.close(); }
});
test('invalid analysis withholds all sending and uses a persisted retry delay', async () => {
  const f = fixture(); f.provider.json = async () => { f.calls.push(1); return { style: '' }; };
  try {
    f.engine.ingest(f.event('a')); f.engine.ingest(f.event('b')); f.advance(30);
    await f.engine.cycle('group:10'); await f.engine.cycle('group:10');
    assert.equal(f.calls.length, 1); assert.equal(f.sends(), 0);
    assert.equal(f.engine.orientation.get('group:10').error, 'orientation_analysis_failed');
    f.advance(60); await f.engine.cycle('group:10'); assert.equal(f.calls.length, 2);
  } finally { f.store.close(); }
});
test('private conversations bypass group orientation without collecting group data', async () => {
  const f = fixture();
  try {
    f.engine.ingest({ ...f.event('private'), message_type: 'private' }); await f.engine.cycle('private:20');
    assert.equal(f.sends(), 1); assert.equal(f.reads.length, 0); assert.equal(f.calls.length, 3);
    assert.equal(f.calls[0].payload.groupOrientation, null);
  } finally { f.store.close(); }
});
test('rejoin resets observation once, and cancels a reply generated against the previous orientation', async () => {
  const f = fixture(), original = f.provider.json;
  const notice = { post_type: 'notice', notice_type: 'group_increase', group_id: 10, user_id: 99, time: 1030 };
  f.provider.json = async (...args) => { const r = await original(...args); if (args[0].includes('TASK: ARTICULATE')) f.engine.ingest(notice); return r; };
  try {
    f.engine.ingest(f.event('a')); f.engine.ingest(f.event('b')); f.advance(30); await f.engine.cycle('group:10');
    assert.equal(f.sends(), 0); assert.equal(f.engine.orientation.get('group:10').status, 'observing');
    assert.equal(f.engine.orientation.get('group:10').message_count, 0);
    const epoch = f.engine.orientation.get('group:10').epoch; f.engine.ingest(notice);
    assert.equal(f.engine.orientation.get('group:10').epoch, epoch);
  } finally { f.store.close(); }
});
test('historical import is bounded, attributed, and rejects other groups/self/ignored users', () => {
  const m = { group_id: 10, user_id: 20, message: [{ type: 'text', data: { text: '园艺' } }, { type: 'image', data: { url: 'secret-url' } }] };
  const value = cleanOrientationSource('history', { messages: [m, { ...m, group_id: 11 }, { ...m, user_id: 99 }, { ...m, user_id: 21 }] }, '10', defaults.agent.observation, '99', ['21']);
  assert.equal(value.length, 1); assert.match(value[0].text, /园艺/); assert.ok(!JSON.stringify(value).includes('secret-url'));
  assert.throws(() => cleanOrientationSource('info', { group_id: 11, group_name: '错误群' }, '10', defaults.agent.observation));
});
