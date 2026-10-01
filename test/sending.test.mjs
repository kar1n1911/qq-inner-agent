import test from 'node:test';
import assert from 'node:assert/strict';
import { defaults, merge, validate } from '../src/config.mjs';
import { forecastResult, sendingProbability } from '../src/sending.mjs';
import { Store } from '../src/store.mjs';
import { Engine } from '../src/engine.mjs';

const prediction = { shouldSend: true, outcomes: { reply: 0.6, silence: 0.3, negative: 0.1 }, responseMode: 'answer', plan: '简洁回答；有新问题再接续，沉默时等待。' };
function fixture({ direct = false, draw = 0, forecast = prediction, dryRun = false } = {}) {
  let now = 1_000_000, sent = 0, rolls = 0;
  const store = new Store(':memory:'), calls = [];
  const c = merge(defaults, { apiKey: 'test', provider: { model: 'mock' }, agent: { observation: { enabled: false }, allowedGroups: ['10'], quietHours: null, dryRun } });
  const provider = { json: async (system, payload) => {
    calls.push({ system, payload });
    if (system.includes('TASK: FORM')) return { allocation: 'open', candidates: [{ kind: 'system2', text: '可以试试这个方法' }] };
    if (system.includes('TASK: EVALUATE')) return { ratings: payload.candidates.map(x => ({ id: x.id, motivation: 5, relevance: 5, originality: 5 })) };
    if (system.includes('TASK: FORECAST')) return structuredClone(forecast);
    return { text: '先试试这个简单方法。' };
  } };
  const transport = { selfId: '99', online: true, connected: true, send: async () => ({ message_id: ++sent }) };
  const engine = new Engine(c, store, provider, transport, { now: () => now, random: () => { rolls++; return draw; } });
  const ingest = id => engine.ingest({ post_type: 'message', message_type: 'group', self_id: 99, user_id: 20, group_id: 10, time: now, message_id: id, message: `${direct ? '[CQ:at,qq=99]' : ''}有什么建议？` });
  ingest('first'); now += 30;
  return { store, calls, engine, provider, transport, c, ingest, advance: seconds => { now += seconds; }, sent: () => sent, rolls: () => rolls };
}
test('time, pace, motivation and forecast combine into a bounded admission probability', () => {
  const s = defaults.agent.sending;
  const t = { proactive: true, age: 15, gap: 300, recentHumans: 6, score: 5 };
  assert.ok(Math.abs(sendingProbability(s, t, prediction).probability - 0.36) < 1e-12);
  assert.equal(sendingProbability(s, { ...t, age: 0 }, prediction).probability, 0);
  assert.ok(Math.abs(sendingProbability(s, { ...t, gap: 150 }, prediction).probability - 0.18) < 1e-12);
  assert.equal(sendingProbability(s, { ...t, proactive: false }, prediction).probability, 1);
  assert.equal(sendingProbability(s, t, { ...prediction, shouldSend: false }).probability, 0);
});
test('configuration and forecasts reject malformed numbers, distributions and conflicting plans', () => {
  for (const sending of [{ proactiveProbability: 2 }, { enabled: 'true' }, { settleSeconds: 0 }, { maxNegativeProbability: NaN }]) assert.throws(() => validate(merge(defaults, { agent: { sending } })));
  for (const f of [{}, { ...prediction, responseMode: 'wait' }, { ...prediction, outcomes: { reply: 1, silence: 1, negative: 0 } }, { ...prediction, plan: '' }]) assert.throws(() => forecastResult(f));
  assert.deepEqual(forecastResult(prediction), prediction);
});
test('probability rejection consumes one attempt, including a later pause and engine recreation', async () => {
  const f = fixture({ draw: 0.99 });
  try {
    await f.engine.cycle('group:10'); f.advance(60); await f.engine.cycle('group:10', 'pause');
    assert.equal(f.sent(), 0); assert.equal(f.rolls(), 1); assert.equal(f.calls.length, 3);
    assert.equal(f.engine.state('group:10').pauseDone, true);
    const second = new Engine(f.c, f.store, f.provider, f.transport, { now: f.engine.now });
    second.restore(); await second.cycle('group:10', 'pause'); assert.equal(f.calls.length, 3);
    assert.equal(f.store.assessment('group:10', 'first').status, 'withheld');
  } finally { f.store.close(); }
});
test('forecast veto and risk threshold also apply to direct questions', async () => {
  for (const forecast of [{ ...prediction, shouldSend: false, responseMode: 'wait' }, { ...prediction, outcomes: { reply: 0.2, silence: 0.2, negative: 0.6 } }]) {
    const f = fixture({ direct: true, forecast });
    try { await f.engine.cycle('group:10'); assert.equal(f.sent(), 0); assert.equal(f.calls.length, 3); }
    finally { f.store.close(); }
  }
});
test('successful forecast guides articulation and the next turn, with isolated expiring observations', async () => {
  const f = fixture({ direct: true });
  try {
    await f.engine.cycle('group:10'); assert.equal(f.sent(), 1); assert.equal(f.calls.length, 4);
    assert.deepEqual(f.calls[3].payload.responsePlan, prediction);
    assert.equal(f.store.assessment('group:10', 'first').status, 'sent');
    assert.equal(f.store.expectation('private:20', f.engine.now()), null);
    assert.equal(f.store.expectation('group:10', f.engine.now()).observation.event, 'no_message_yet');
    f.advance(5); f.ingest('second'); await f.engine.cycle('group:10');
    assert.equal(f.calls[4].payload.priorExpectation.observation.event, 'human_message');
    assert.equal(f.calls[4].payload.priorExpectation.elapsedSeconds, 5);
    f.advance(301); assert.equal(f.store.expectation('group:10', f.engine.now()), null);
  } finally { f.store.close(); }
});
test('dry runs and uncertain delivery never establish expectations', async () => {
  for (const dryRun of [true, false]) {
    const f = fixture({ direct: true, dryRun });
    if (!dryRun) f.transport.send = async () => { throw Object.assign(Error('timeout'), { uncertain: true }); };
    try {
      await f.engine.cycle('group:10'); assert.equal(f.store.expectation('group:10', f.engine.now()), null);
      assert.equal(f.store.assessment('group:10', 'first').status, dryRun ? 'dry_run' : 'uncertain');
    } finally { f.store.close(); }
  }
});
test('new messages during forecasting discard the stale forecast without a random draw', async () => {
  const f = fixture(), original = f.provider.json;
  f.provider.json = async (sys, payload) => { const result = await original(sys, payload); if (sys.includes('TASK: FORECAST')) f.ingest('new'); return result; };
  try { await f.engine.cycle('group:10'); assert.equal(f.rolls(), 0); assert.equal(f.sent(), 0); assert.equal(f.engine.state('group:10').pending, true); }
  finally { f.store.close(); }
});
test('invalid forecasts fail closed before articulation or transmission', async () => {
  const f = fixture({ forecast: { shouldSend: true } });
  try { await assert.rejects(f.engine.cycle('group:10'), /Invalid sending forecast/); assert.equal(f.calls.length, 3); assert.equal(f.sent(), 0); }
  finally { f.store.close(); }
});
