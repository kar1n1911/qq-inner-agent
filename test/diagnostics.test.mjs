import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { setTimeout as sleep } from 'node:timers/promises';
import { Diagnostics } from '../src/diagnostics.mjs';

function fixture(duration = 60000) {
  const bots = [], sends = [];
  class Bot extends EventEmitter {
    selfId = '99';
    async start(signal) {
      this.connected = true;
      queueMicrotask(() => this.emit('status', 'connected'));
      await new Promise(resolve => signal.addEventListener('abort', resolve, { once: true }));
      this.connected = false;
    }
    async send(chat, text) { sends.push({ chat, text }); return { message_id: 42 }; }
  }
  const d = new Diagnostics(() => ({ apiKey: 'secret-model', onebotToken: 'secret-token' }), () => { const b = new Bot(); bots.push(b); return b; }, duration);
  return { d, bots, sends };
}
test('debug send uses only connected account and closes its independent connection', async () => {
  const { d, bots, sends } = fixture();
  const result = await d.send();
  assert.equal(sends.length, 1); assert.equal(sends[0].chat, 'private:99');
  assert.match(sends[0].text, /diagnostic/); assert.equal(result.messageId, 42);
  assert.equal(bots[0].connected, false);
});
test('debug receiver captures multiple own message types, rejects other users/accounts, redacts and bounds data', async () => {
  const { d, bots } = fixture();
  await d.listen(); const b = bots[0];
  const base = { post_type: 'message_sent', message_type: 'private', self_id: 99, user_id: 99, message_id: 1 };
  try {
    for (const type of ['text', 'image', 'record', 'file', 'reply']) b.emit('event', { ...base, message: [{ type, data: { text: 'secret-token secret-model test', url: 'https://private.example' } }] });
    assert.equal(d.status().events.length, 5);
    assert.equal(d.status().events[2].types[0], 'record');
    assert.ok(!JSON.stringify(d.status()).includes('secret-token'));
    assert.ok(!JSON.stringify(d.status()).includes('private.example'));
    b.emit('event', { ...base, user_id: 20, message: 'other user' });
    b.emit('event', { ...base, self_id: 20, message: 'other account' });
    b.emit('event', { ...base, post_type: 'meta_event', message: 'heartbeat' });
    assert.equal(d.status().events.length, 5);
    b.emit('event', { ...base, post_type: 'message', message: 'hello [CQ:image,file=private-file]' });
    assert.deepEqual(d.status().events.at(-1).types, ['text', 'image']);
    assert.ok(!d.status().events.at(-1).text.includes('private-file'));
    await assert.rejects(d.listen(), /already_running/);
    for (let i = 0; i < 40; i++) b.emit('event', { ...base, message: 'x'.repeat(2000) });
    assert.equal(d.status().events.length, 30); assert.equal(d.status().events[0].text.length, 1000);
  } finally { await d.stop(); }
  assert.equal(b.connected, false); assert.equal(d.status().state, 'stopped');
});
test('debug receiver expires and a new test clears old captures', async () => {
  const { d, bots } = fixture(20);
  await d.listen(); await sleep(50);
  assert.equal(d.status().state, 'finished'); assert.equal(bots[0].connected, false);
  await d.listen(); assert.deepEqual(d.status().events, []); await d.stop();
});
