import test from 'node:test';
import assert from 'node:assert/strict';
import { defaults, merge } from '../src/config.mjs';
import { Store } from '../src/store.mjs';
import { Engine } from '../src/engine.mjs';
import { pickLengthTarget } from '../src/policy.mjs';
import { articulationFor, formation, composePrompt } from '../src/prompts.mjs';

const prediction = { shouldSend: true, outcomes: { reply: 0.6, silence: 0.3, negative: 0.1 }, responseMode: 'answer', plan: '简洁回答。' };
const BUCKETS = ['tiny', 'short', 'medium', 'long'];

test('every draw yields a valid bucket in both modes', () => {
  for (let i = 0; i <= 1000; i++) {
    const draw = i / 1000;
    assert.ok(BUCKETS.includes(pickLengthTarget('open', () => draw)));
    assert.ok(BUCKETS.includes(pickLengthTarget('self', () => draw)));
  }
});

test('bucket boundaries match the declared weights', () => {
  // open turns: tiny < .35, short < .80, medium < .98, else long
  assert.equal(pickLengthTarget('open', () => 0.34), 'tiny');
  assert.equal(pickLengthTarget('open', () => 0.35), 'short');
  assert.equal(pickLengthTarget('open', () => 0.79), 'short');
  assert.equal(pickLengthTarget('open', () => 0.80), 'medium');
  assert.equal(pickLengthTarget('open', () => 0.979), 'medium');
  assert.equal(pickLengthTarget('open', () => 0.98), 'long');
  // addressed turns: short < .70, medium < .98, else long — never tiny
  assert.equal(pickLengthTarget('self', () => 0.0), 'short');
  assert.equal(pickLengthTarget('self', () => 0.69), 'short');
  assert.equal(pickLengthTarget('self', () => 0.70), 'medium');
  assert.equal(pickLengthTarget('self', () => 0.979), 'medium');
  assert.equal(pickLengthTarget('self', () => 0.98), 'long');
});

test('a direct request can never be answered with the throwaway bucket', () => {
  for (let i = 0; i <= 1000; i++) assert.notEqual(pickLengthTarget('self', () => i / 1000), 'tiny');
});

test('the articulation prompt carries the length contract and the AI-tell bans', () => {
  const prompt = articulationFor('zh-CN');
  assert.match(prompt, /lengthTarget/);
  assert.match(prompt, /tiny 不超过 12 字/);
  for (const banned of ['不是…而是…', '希望对你有帮助', '作为 AI']) assert.ok(prompt.includes(banned), `prompt should ban: ${banned}`);
  // The length contract must survive every language variant.
  for (const language of ['auto', 'zh-CN', 'en']) assert.match(articulationFor(language), /lengthTarget/);
});

test('the articulation prompt carries the sticker and face guidance', () => {
  const prompt = articulationFor('zh-CN');
  for (const clue of ['优先 face', '只在情绪节拍上使用', '位置固定在消息末尾', '不得编造表情编号', '不要在同一段混用']) {
    assert.ok(prompt.includes(clue), `prompt should mention: ${clue}`);
  }
  // The JSON contract must stay untouched by prompt wording changes.
  assert.match(prompt, /"emoji":null,"faceId":null/);
  // Until the engine can send a decoration-only message, the prompt must not invite one.
  assert.ok(!prompt.includes('空文本'), 'prompt must not ask for empty text before the engine supports it');
});

test('formation lets candidates carry a mild opinion', () => {
  assert.match(composePrompt(formation), /不同看法/);
});

function fixture({ direct, lengthDraw, logs }) {
  let now = 1_000_000, sent = 0;
  const store = new Store(':memory:'), calls = [];
  const c = merge(defaults, {
    apiKey: 'test',
    provider: { model: 'mock' },
    agent: { observation: { enabled: false }, allowedGroups: ['10'], quietHours: null },
  });
  const provider = { json: async (system, payload) => {
    calls.push({ system, payload });
    if (system.includes('TASK: FORM')) return { allocation: 'open', candidates: [{ kind: 'system2', text: '可以试试这个方法' }] };
    if (system.includes('TASK: EVALUATE')) return { ratings: payload.candidates.map(x => ({ id: x.id, motivation: 5, relevance: 5, originality: 5 })) };
    if (system.includes('TASK: FORECAST')) return structuredClone(prediction);
    return { text: '先试试这个简单方法。' };
  } };
  const transport = { selfId: '99', online: true, connected: true, send: async () => ({ message_id: ++sent }) };
  // A constant expressionRandom keeps the sampled bucket deterministic no matter how
  // many draws decoration selection consumes first.
  const engine = new Engine(c, store, provider, transport, {
    now: () => now, random: () => 0, expressionRandom: () => lengthDraw,
    log: (event, data) => logs?.push({ event, data }),
  });
  engine.ingest({
    post_type: 'message', message_type: 'group', self_id: 99, user_id: 20, group_id: 10, time: now, message_id: 'first',
    message: `${direct ? '[CQ:at,qq=99]' : ''}有什么建议？`,
  });
  now += 30;
  return { store, calls, engine };
}

test('the sampled bucket is recorded in the send log so variation stays auditable', async () => {
  const logs = [];
  const f = fixture({ direct: false, lengthDraw: 0.5, logs }); // open turn -> short
  try {
    await f.engine.cycle('group:10');
    const sent = logs.find(entry => entry.event === 'message_sent');
    assert.ok(sent, 'the send should be logged');
    assert.equal(sent.data.lengthTarget, 'short');
  } finally { f.store.close(); }
});

test('a cycle threads the sampled length target into the articulation payload', async () => {
  const f = fixture({ direct: false, lengthDraw: 0.1 }); // open turn -> tiny
  try {
    await f.engine.cycle('group:10');
    const articulation = f.calls.find(c => c.system.includes('TASK: ARTICULATE'));
    assert.ok(articulation, 'the cycle should reach articulation');
    assert.equal(articulation.payload.lengthTarget, 'tiny');
  } finally { f.store.close(); }
});

test('an addressed turn threads the non-throwaway bucket instead', async () => {
  const f = fixture({ direct: true, lengthDraw: 0.0 }); // addressed -> short
  try {
    await f.engine.cycle('group:10');
    const articulation = f.calls.find(c => c.system.includes('TASK: ARTICULATE'));
    assert.ok(articulation, 'the cycle should reach articulation');
    assert.equal(articulation.payload.lengthTarget, 'short');
  } finally { f.store.close(); }
});
