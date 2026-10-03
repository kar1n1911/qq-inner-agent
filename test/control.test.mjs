import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import net from 'node:net';
import http from 'node:http';
import { once } from 'node:events';
import { setTimeout as sleep } from 'node:timers/promises';
import { ControlClient, MAX_LINE } from '../src/control.mjs';
import { createDashboard } from '../src/dashboard.mjs';
import { Store } from '../src/store.mjs';

async function until(predicate) {
  for (let i = 0; i < 200; i++) { if (predicate()) return; await sleep(10); }
  assert.fail('condition did not become true');
}
function fixture(t) {
  // macOS Unix socket 路径长度有限，使用短目录。
  const root = fs.mkdtempSync('/tmp/qc-');
  fs.mkdirSync(path.join(root, 'data'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  return root;
}
async function mock(t, dir, handler) {
  const sockets = new Set();
  const server = net.createServer(socket => {
    sockets.add(socket); socket.on('error', () => {}); socket.on('close', () => sockets.delete(socket));
    let buffer = '';
    socket.on('data', chunk => {
      buffer += chunk;
      for (;;) {
        const end = buffer.indexOf('\n'); if (end < 0) break;
        const value = JSON.parse(buffer.slice(0, end)); buffer = buffer.slice(end + 1);
        handler(value, socket);
      }
    });
  });
  server.listen(path.join(dir, 'control.sock')); await once(server, 'listening');
  t.after(async () => { for (const socket of sockets) socket.destroy(); await new Promise(r => server.close(r)); });
  return { server, sockets };
}
const reply = (socket, req, result) => socket.write(JSON.stringify({ id: req.id, ok: true, result }) + '\n');

test('control correlates out-of-order responses, UTF-8 split frames, events and errors', async t => {
  const dir = path.join(fixture(t), 'data'), requests = [];
  await mock(t, dir, (req, socket) => {
    requests.push(req);
    if (requests.length === 2) {
      const bytes = Buffer.from(JSON.stringify({ id: req.id, ok: true, result: '中文' }) + '\n' + JSON.stringify({ event: 'decision', data: { score: 4 } }) + '\n');
      const split = bytes.indexOf(Buffer.from('中')) + 1;
      socket.write(bytes.subarray(0, split));
      setImmediate(() => { socket.write(bytes.subarray(split)); reply(socket, requests[0], 'first'); });
    } else if (requests.length === 3) socket.write(JSON.stringify({ id: req.id, ok: false, error: { code: 'qq_offline', message: 'offline' } }) + '\n');
  });
  const client = new ControlClient(dir); t.after(() => client.close()); await until(() => client.available);
  const events = [], unsubscribe = client.subscribe('decision', data => events.push(data));
  assert.deepEqual(await Promise.all([client.request('first'), client.request('second')]), ['first', '中文']);
  assert.deepEqual(events, [{ score: 4 }]); unsubscribe();
  await assert.rejects(client.request('error'), { code: 'qq_offline', message: 'offline' });
});

test('control missing socket, timeout, disconnect and reconnect never replay requests', async t => {
  const dir = path.join(fixture(t), 'data');
  const client = new ControlClient(dir, { timeoutMs: 80, retryMs: 10, maxRetryMs: 40 });
  t.after(() => client.close()); await sleep(30);
  assert.equal(client.available, false); await assert.rejects(client.request('missing'), { code: 'control_unavailable' });
  const requests = [];
  const { sockets } = await mock(t, dir, (req, socket) => { requests.push(req.method); if (req.method === 'ok') reply(socket, req, 42); });
  await until(() => client.available);
  await assert.rejects(client.request('timeout'), { code: 'control_timeout' });
  assert.equal(client.available, true);
  const pending = client.request('disconnect'); const rejected = assert.rejects(pending, { code: 'control_disconnected' });
  await until(() => requests.includes('disconnect'));
  for (const socket of sockets) socket.destroy(); await rejected;
  await until(() => client.available); assert.equal(await client.request('ok'), 42);
  assert.deepEqual(requests, ['timeout', 'disconnect', 'ok']);
});

test('control bounds frames and pending requests and discards malformed/unfinished responses', async t => {
  const dir = path.join(fixture(t), 'data');
  await mock(t, dir, (req, socket) => {
    if (req.method === 'oversized') socket.write(Buffer.alloc(MAX_LINE + 1, 120));
    if (req.method === 'invalid') socket.write('invalid\n');
    if (req.method === 'partial') socket.end(JSON.stringify({ id: req.id, ok: true, result: 'unterminated' }));
  });
  const client = new ControlClient(dir, { retryMs: 10 }); t.after(() => client.close());
  for (const method of ['oversized', 'invalid', 'partial']) {
    await until(() => client.available);
    await assert.rejects(client.request(method), { code: 'control_disconnected' });
  }
  await until(() => client.available);
  await assert.rejects(client.request('large', { text: '中'.repeat(MAX_LINE/2) }), { code: 'control_line_too_long' });
  const pending = Array.from({ length: 32 }, () => assert.rejects(client.request('wait'), { code: 'control_closed' }));
  await assert.rejects(client.request('overflow'), { code: 'control_busy' });
  client.close(); await Promise.all(pending); await sleep(30); assert.equal(client.available, false);
});

test('dashboard HTTP state survives absent socket, uses live reads, then falls back on failure', async t => {
  const root = fixture(t), dir = path.join(root, 'data');
  fs.writeFileSync(path.join(dir, 'status.json'), JSON.stringify({ mode: 'dry_run', selfId: '42' }));
  const store = new Store(path.join(dir, 'agent.sqlite'));
  store.decision('group:1', 'skip', 2, [], 123); store.close();
  const server = http.createServer(); server.listen(0, '127.0.0.1'); await once(server, 'listening');
  t.after(async () => { server.closeAllConnections(); await new Promise(r => server.close(r)); });
  const origin = `http://127.0.0.1:${server.address().port}`;
  const dashboard = createDashboard({ root, settings: { origins: [origin] }, key: 'secret', serviceStatus: async () => 'active', controlOptions: { timeoutMs: 80, retryMs: 10 } });
  t.after(() => dashboard.close()); server.on('request', dashboard.handler);
  const login = await fetch(origin + '/api/login', { method: 'POST', headers: { Origin: origin, 'Content-Type': 'application/json' }, body: '{"key":"secret"}' });
  const headers = { Cookie: login.headers.get('set-cookie').split(';')[0], Origin: origin, 'X-CSRF-Token': (await login.json()).csrf, 'Content-Type': 'application/json' };
  const get = async url => { const res = await fetch(origin + url, { headers }); assert.equal(res.status, 200); return res.json(); };
  const post = async url => { const res = await fetch(origin + url, { method: 'POST', headers, body: '{}' }); assert.equal(res.status, 200); return res.json(); };
  const fallback = await get('/api/state');
  assert.equal(dashboard.control.available, false); assert.equal(fallback.status.mode, 'dry_run'); assert.equal(fallback.decisions[0].score, 2);
  let fail = false, listening = false, saturated = false;
  await mock(t, dir, (req, socket) => {
    if (fail) return;
    const results = {
      'state.get': { mode: 'active', selfId: '42', learningCounts: { memories: 1 } },
      'learning.list': { entries: [{ chat: 'group:1', layer: 'traits', expires: 9999999999, text: 'secret' }, { chat: 'group:1', term: 'hello', updated: Date.now()/1000 }] },
      'logs.tail': { lines: ['invalid', '{"event":"secret"}'] },
      'models.list': { models: ['model'] }, 'contacts.list': { groups: [], friends: [] },
      'debug.send': { ok: true, messageId: 123 },
      'debug.receive.start': { listening: true, until: 12345 }, 'debug.receive.stop': { stopped: true }
    };
    if (req.method === 'debug.receive.start') listening = true;
    if (req.method === 'debug.receive.stop') listening = false;
    if (saturated && req.method === 'learning.list') results['learning.list'].entries = Array.from({ length: 1000 }, () => ({ chat: 'group:1', layer: 'traits', expires: 9999999999 }));
    reply(socket, req, req.method === 'debug.receive.status' ? { listening, events: [] } : results[req.method]);
  });
  await until(() => dashboard.control.available);
  const live = await get('/api/state');
  assert.deepEqual(Object.keys(live).sort(), Object.keys(fallback).sort());
  assert.deepEqual(live.status, { mode: 'active', selfId: '42' }); assert.equal(live.memories[0].text, '[redacted]');
  assert.equal(live.expressions[0].term, 'hello'); assert.deepEqual(live.logs, [{ event: '[redacted]' }]);
  assert.deepEqual(live.decisions, fallback.decisions);
  assert.deepEqual(await post('/api/models'), { models: ['model'] });
  assert.deepEqual(await get('/api/contacts'), { groups: [], friends: [] });
  assert.deepEqual(await get('/api/debug/receive'), { state: 'idle', events: [] });
  assert.deepEqual(await post('/api/debug/receive'), { state: 'listening', account: '42', until: 12345, events: [], error: null });
  assert.equal((await get('/api/debug/receive')).state, 'listening');
  assert.equal((await post('/api/debug/stop')).state, 'stopped');
  assert.deepEqual(Object.keys(await post('/api/debug/send')).sort(), ['account', 'message', 'messageId', 'text']);
  saturated = true;
  const truncated = await get('/api/state');
  assert.equal(truncated.memories.length, 200); assert.deepEqual(truncated.expressions, fallback.expressions);
  fail = true;
  assert.deepEqual(await get('/api/state'), fallback);
});

test('dashboard does not replay an uncertain diagnostic send through the fallback', async t => {
  const root = fixture(t), dir = path.join(root, 'data');
  let sends = 0, fallbackConnections = 0;
  await mock(t, dir, (req, socket) => {
    if (req.method === 'state.get') reply(socket, req, { selfId: '42' });
    if (req.method === 'debug.send') { sends++; socket.destroy(); }
  });
  const server = http.createServer(); server.listen(0, '127.0.0.1'); await once(server, 'listening');
  t.after(async () => { server.closeAllConnections(); await new Promise(r => server.close(r)); });
  const origin = `http://127.0.0.1:${server.address().port}`;
  const dashboard = createDashboard({ root, settings: { origins: [origin] }, key: 'key', makeBot: () => { fallbackConnections++; throw Error('unexpected_fallback'); } });
  t.after(() => dashboard.close()); server.on('request', dashboard.handler);
  await until(() => dashboard.control.available);
  const login = await fetch(origin + '/api/login', { method: 'POST', headers: { Origin: origin, 'Content-Type': 'application/json' }, body: '{"key":"key"}' });
  const headers = { Cookie: login.headers.get('set-cookie').split(';')[0], Origin: origin, 'X-CSRF-Token': (await login.json()).csrf };
  const res = await fetch(origin + '/api/debug/send', { method: 'POST', headers });
  assert.equal(res.status, 502); assert.equal((await res.json()).error, 'control_disconnected');
  assert.equal(sends, 1); assert.equal(fallbackConnections, 0);
});
