import test from 'node:test';
import assert from 'node:assert/strict';
import { defaults, merge, validate } from '../src/config.mjs';
import { Store } from '../src/store.mjs';
import { Engine } from '../src/engine.mjs';
import { ActivityRhythm, activityProbability } from '../src/activity.mjs';

const schedule = { enabled: true, activeStart: '08:00', inactiveStart: '22:00', timezone: 'UTC' };
const rhythm = { ...defaults.agent.rhythm, enabled: true };
const ts = time => Date.parse(`2026-01-01T${time}:00Z`) / 1000;
test('Gaussian inactivity peaks in the middle and falls symmetrically toward both edges', () => {
  const p = t => activityProbability(ts(t), schedule, rhythm);
  assert.equal(p('22:00'), rhythm.edgeProbability);
  assert.ok(Math.abs(p('03:00') - rhythm.centerProbability) < 1e-12);
  assert.ok(p('23:00') > p('01:00') && p('01:00') > p('03:00'));
  assert.ok(Math.abs(p('23:00') - p('07:00')) < 1e-12);
  assert.ok(Math.abs(p('01:00') - p('05:00')) < 1e-12);
  assert.equal(p('08:00'), rhythm.dayProbability);
  assert.equal(activityProbability(ts('03:00'), { ...schedule, enabled: false }, rhythm), rhythm.dayProbability);
  const overnight = { ...schedule, activeStart: '22:00', inactiveStart: '08:00' };
  assert.ok(Math.abs(activityProbability(ts('15:00'), overnight, rhythm) - rhythm.centerProbability) < 1e-12);
  assert.equal(activityProbability(ts('02:00'), { ...schedule, timezone: 'Europe/Stockholm' }, rhythm), p('03:00'));
});
test('rhythm config rejects inverted durations, invalid probabilities and nonfinite widths', () => {
  for (const r of [{ enabled: 'yes' }, { dayProbability: 1.1 }, { centerProbability: .9 }, { sigma: NaN }, { activeMinSeconds: 1300 }, { restMaxSeconds: 20 }, { restMinSeconds: 31.5 }]) {
    assert.throws(() => validate(merge(defaults, { agent: { rhythm: r } })));
  }
});
test('account-wide blocks persist across reconstruction, do not reroll, and reload on changed settings', () => {
  const store = new Store(':memory:'); let draws = 0;
  const agent = merge(defaults.agent, { schedule, rhythm });
  const random = () => { draws++; return .5; };
  try {
    let clock = ts('03:00');
    const activity = new ActivityRhythm(store, agent, random);
    const first = activity.snapshot(clock);
    assert.equal(first.active, false); assert.equal(draws, 2);
    assert.deepEqual(activity.snapshot(clock), first);
    const restored = new ActivityRhythm(store, agent, random);
    assert.equal(restored.snapshot(first.until - 1).until, first.until); assert.equal(draws, 2);
    clock = first.until;
    assert.equal(restored.snapshot(clock).started, clock); assert.equal(draws, 4);
    const changed = new ActivityRhythm(store, merge(agent, { rhythm: { centerProbability: .1 } }), random);
    changed.snapshot(clock + 1); assert.equal(draws, 6);
    changed.snapshot(clock - 1); assert.equal(draws, 8); // clock correction
  } finally { store.close(); }
});
test('active blocks continue across the nominal schedule cutoff; disabled rhythm keeps strict scheduling', () => {
  const store = new Store(':memory:');
  try {
    const agent = merge(defaults.agent, { schedule, rhythm });
    const activity = new ActivityRhythm(store, agent, () => 0);
    const first = activity.snapshot(ts('21:59'));
    assert.equal(first.active, true);
    assert.equal(activity.snapshot(ts('22:01')).until, first.until);
    const strict = new ActivityRhythm(store, merge(agent, { rhythm: { enabled: false } }));
    assert.equal(strict.snapshot(ts('22:01')).active, false);
  } finally { store.close(); }
});

function fixture() {
  let clock = ts('12:00'), sends = 0;
  const store = new Store(':memory:'), calls = [];
  const config = merge(defaults, { provider: { model: 'mock' }, apiKey: 'mock', agent: {
    allowedUsers: ['20'], quietHours: null, sending: { enabled: false },
    rhythm: { enabled: true, dayProbability: .5, activeMinSeconds: 30, activeMaxSeconds: 30, restMinSeconds: 30, restMaxSeconds: 30 }
  } });
  const draws = [0, 0, .9, 0, 0, 0];
  const provider = { json: async (prompt, payload) => {
    calls.push(prompt);
    if (prompt.includes('TASK: FORM')) return { allocation: 'self', candidates: [{ kind: 'system2', text: '你好' }] };
    if (prompt.includes('TASK: EVALUATE')) return { ratings: payload.candidates.map(c => ({ id: c.id, motivation: 5, relevance: 5, originality: 5 })) };
    return { text: '你好！' };
  } };
  const transport = { selfId: '99', connected: true, online: true, send: async () => ({ message_id: ++sends }) };
  const engine = new Engine(config, store, provider, transport, { now: () => clock, activityRandom: () => draws.shift() ?? 0 });
  const event = id => ({ post_type: 'message', message_type: 'private', user_id: 20, self_id: 99, message_id: id, time: clock, message: '你好' });
  return { store, engine, event, calls, provider, sends: () => sends, advance: seconds => { clock += seconds; } };
}
test('rest blocks suppress direct messages and queued work without model calls, then allow fresh input', async () => {
  const f = fixture();
  try {
    f.engine.ingest(f.event('before'));
    f.advance(30); f.engine.tick();
    f.engine.ingest(f.event('rest'));
    await f.engine.cycle('private:20');
    assert.equal(f.calls.length, 0);
    assert.equal(f.engine.chats.get('private:20').pending, false);
    assert.equal(f.store.history('private:20').length, 1);
    f.advance(30); f.engine.ingest(f.event('awake')); await f.engine.cycle('private:20');
    assert.equal(f.sends(), 1);
  } finally { f.store.close(); }
});
test('a block ending during generation cancels delivery', async () => {
  const f = fixture();
  try {
    const original = f.provider.json;
    f.provider.json = async (prompt, payload) => { const result = await original(prompt, payload); if (prompt.includes('TASK: ARTICULATE')) f.advance(30); return result; };
    f.engine.ingest(f.event('crossing')); await f.engine.cycle('private:20');
    assert.equal(f.sends(), 0);
    assert.equal(f.engine.available(ts('12:00') + 30), false);
  } finally { f.store.close(); }
});
