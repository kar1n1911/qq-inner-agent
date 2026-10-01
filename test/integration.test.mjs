import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { setTimeout as sleep } from 'node:timers/promises';
import { defaults, merge } from '../src/config.mjs';
import { Store } from '../src/store.mjs';
import { Provider } from '../src/provider.mjs';
import { OneBot } from '../src/onebot.mjs';
import { Engine } from '../src/engine.mjs';

async function until(predicate, ms = 5000) {
  const start = Date.now();
  while (!predicate()) { if (Date.now() - start > ms) throw Error('Timed out'); await sleep(10); }
}
function frame(data) {
  const body = Buffer.from(JSON.stringify(data));
  if (body.length < 126) return Buffer.concat([Buffer.from([0x81, body.length]), body]);
  const header = Buffer.alloc(4); header[0] = 0x81; header[1] = 126; header.writeUInt16BE(body.length, 2);
  return Buffer.concat([header, body]);
}
async function mockServer(kind = 'openai', napcat = false) {
  const sockets = new Set(), requests = [], sends = [];
  let connections = 0, statusRequests = 0, ignoreHeartbeat = false;
  const server = http.createServer(async (req, res) => {
    const chunks = []; for await (const chunk of req) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks).toString());
    requests.push({ path: req.url, headers: req.headers, body });
    const system = body.system || body.messages[0].content;
    const payload = JSON.parse(body.messages.at(-1).content);
    let value;
    if (system.includes('TASK: FORM')) value = { allocation: 'open', candidates: [{ kind: 'system2', text: 'Suggest a shaded walking route.' }] };
    else if (system.includes('TASK: EVALUATE')) value = { ratings: payload.candidates.map(c => ({ id: c.id, motivation: 4.8, relevance: 5, originality: 5, for: ['relevance'], against: [] })) };
    else if (system.includes('TASK: FORECAST')) value = { shouldSend: true, outcomes: { reply: 0.6, silence: 0.3, negative: 0.1 }, responseMode: 'answer', plan: '简洁回答，等待反馈。' };
    else value = { text: 'A shaded route would be more comfortable. [CQ:at,qq=all]' };
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify(kind === 'openai' ? { choices: [{ finish_reason: 'stop', message: { content: JSON.stringify(value) } }] } : { stop_reason: 'end_turn', content: [{ type: 'text', text: JSON.stringify(value) }] }));
  });
  server.on('upgrade', (req, socket) => {
    const authorized = new URL(req.url, 'http://localhost').searchParams.get('access_token') === 'local-test-token';
    if (!authorized && !napcat) { socket.end('HTTP/1.1 401 Unauthorized\r\n\r\n'); return; }
    const accept = createHash('sha1').update(req.headers['sec-websocket-key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
    socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
    connections++; sockets.add(socket); socket.on('error', () => {}); socket.on('close', () => sockets.delete(socket));
    if (!authorized) {
      socket.write(frame({ status: 'failed', retcode: 1403, data: null, message: 'token validation failed' }));
      socket.end(Buffer.from([0x88, 0])); return;
    }
    if (napcat) socket.write(frame({ post_type: 'meta_event', meta_event_type: 'lifecycle', sub_type: 'connect', self_id: 99, time: Math.floor(Date.now() / 1000) }));
    let buffer = Buffer.alloc(0);
    socket.on('data', chunk => {
      buffer = Buffer.concat([buffer, chunk]);
      while (buffer.length >= 2) {
        const opcode = buffer[0] & 15; let len = buffer[1] & 127, offset = 2;
        if (len === 126) { if (buffer.length < 4) return; len = buffer.readUInt16BE(2); offset = 4; }
        if (len === 127) { socket.destroy(); return; }
        const masked = !!(buffer[1] & 128), maskBytes = masked ? 4 : 0;
        if (buffer.length < offset + maskBytes + len) return;
        const mask = buffer.subarray(offset, offset + maskBytes); offset += maskBytes;
        const data = Buffer.from(buffer.subarray(offset, offset + len)); buffer = buffer.subarray(offset + len);
        if (masked) for (let i = 0; i < data.length; i++) data[i] ^= mask[i % 4];
        if (opcode === 8) { socket.end(Buffer.from([0x88, 0])); return; }
        if (opcode !== 1) continue;
        const message = JSON.parse(data.toString());
        let result;
        if (message.action === 'get_login_info') result = { user_id: 99, nickname: 'Bot' };
        else if (message.action === 'get_status') { statusRequests++; if (ignoreHeartbeat) continue; result = { online: true, good: true }; }
        else if (message.action === 'get_group_list') result = [{ group_id: 10, group_name: 'Test group' }];
        else if (message.action === 'get_friend_list') result = [{ user_id: 20, nickname: 'Test friend' }];
        else if (message.action.startsWith('send_')) { sends.push(message); result = { message_id: 300 }; }
        else result = null;
        socket.write(frame({ status: 'ok', retcode: 0, data: result, echo: message.echo }));
      }
    });
  });
  server.listen(0, '127.0.0.1'); await once(server, 'listening');
  const port = server.address().port;
  return { port, requests, sends, get connections() { return connections; }, get statusRequests() { return statusRequests; },
    push: event => { for (const s of sockets) s.write(frame(event)); },
    drop: () => { for (const s of sockets) s.destroy(); },
    ignoreHeartbeat: () => { ignoreHeartbeat = true; },
    close: async () => { for (const s of sockets) s.destroy(); server.closeAllConnections(); await new Promise(r => server.close(r)); },
  };
}

for (const kind of ['openai', 'anthropic']) test(`real HTTP + WebSocket integration: ${kind}, reconnect, duplicate suppression`, async () => {
  const server = await mockServer(kind), store = new Store(':memory:');
  const c = merge(defaults, { apiKey: 'local-model-key', provider: { kind, baseUrl: `http://127.0.0.1:${server.port}/v1`, model: 'mock', retries: 0 },
    onebot: { url: `ws://127.0.0.1:${server.port}/`, selfId: '99', reconnectMaxSeconds: 1, heartbeatSeconds: 1, requestTimeoutSeconds: 1 },
    agent: { observation: { enabled: false }, allowedGroups: ['10'], quietHours: null } });
  const bot = new OneBot(c.onebot, 'local-test-token'), stop = new AbortController();
  const provider = new Provider(c.provider, c.apiKey, store), engine = new Engine(c, store, provider, bot);
  bot.on('event', e => engine.ingest(e));
  const running = bot.start(stop.signal);
  try {
    await until(() => bot.connected && bot.online);
    const event = { post_type: 'message', message_type: 'group', group_id: 10, user_id: 20, self_id: 99,
      time: Date.now() / 1000, message_id: 100, message: [{ type: 'at', data: { qq: '99' } }, { type: 'text', data: { text: 'It is hot. Where can we walk?' } }] };
    server.push(event); await until(() => engine.chats.has('group:10'));
    await engine.cycle('group:10');
    assert.equal(server.sends.length, 1); assert.equal(server.requests.length, 4);
    const send = server.sends[0];
    assert.equal(send.action, 'send_group_msg'); assert.equal(send.params.group_id, 10);
    assert.equal(send.params.message[0].type, 'text'); assert.ok(send.params.message[0].data.text.includes('[CQ:at,qq=all]'));
    assert.equal(server.requests[0].path, kind === 'openai' ? '/v1/chat/completions' : '/v1/messages');
    assert.equal(server.requests[0].headers[kind === 'openai' ? 'authorization' : 'x-api-key'], kind === 'openai' ? 'Bearer local-model-key' : 'local-model-key');
    server.drop(); await until(() => server.connections >= 2 && bot.connected);
    server.push(event); await sleep(50);
    assert.equal(engine.chats.get('group:10').pending, false); assert.equal(server.sends.length, 1);
    await until(() => server.statusRequests >= 3);
    server.ignoreHeartbeat(); await until(() => !bot.connected, 4000);
  } finally { stop.abort(); await engine.stop(); await running; store.close(); await server.close(); }
});

test('WebSocket authentication rejection stays disconnected without recursion', async () => {
  const server = await mockServer(); const stop = new AbortController();
  const bot = new OneBot({ ...defaults.onebot, url: `ws://127.0.0.1:${server.port}/`, requestTimeoutSeconds: 1 }, 'wrong-token');
  const running = bot.start(stop.signal);
  try { await sleep(200); assert.equal(bot.connected, false); await assert.rejects(() => bot.send('group:10', 'must not send')); }
  finally { stop.abort(); await running; await server.close(); }
});

for (const format of ['array', 'string']) test(`NapCat forward WebSocket: ${format} events, contacts, private replies and self-message filtering`, async () => {
  const server = await mockServer('openai', true), store = new Store(':memory:');
  const c = merge(defaults, { apiKey: 'local-model-key',
    provider: { baseUrl: `http://127.0.0.1:${server.port}/v1`, model: 'mock', retries: 0 },
    onebot: { url: `ws://127.0.0.1:${server.port}/`, requestTimeoutSeconds: 1 },
    agent: { observation: { enabled: false }, sending: { enabled: false }, allowedGroups: ['10'], allowedUsers: ['20'], quietHours: null } });
  const bot = new OneBot(c.onebot, 'local-test-token'), stop = new AbortController();
  const engine = new Engine(c, store, new Provider(c.provider, c.apiKey, store), bot);
  bot.on('event', event => engine.ingest(event));
  const running = bot.start(stop.signal);
  try {
    await until(() => bot.connected && bot.online);
    assert.equal(bot.selfId, '99');
    assert.equal((await bot.call('get_group_list', {}))[0].group_id, 10);
    assert.equal((await bot.call('get_friend_list', {}))[0].user_id, 20);
    const base = { time: Math.floor(Date.now() / 1000), self_id: 99, post_type: 'message', user_id: 20, sender: { nickname: 'Friend', card: '' } };
    const group = { ...base, message_type: 'group', sub_type: 'normal', group_id: 10, message_id: -101,
      message: format === 'array' ? [{ type: 'at', data: { qq: '99' } }, { type: 'text', data: { text: 'Hi' } }] : '[CQ:at,qq=99] Hi' };
    server.push(group); await until(() => engine.chats.has('group:10'));
    assert.equal(engine.chats.get('group:10').hint, 'self');
    server.push({ ...base, message_type: 'private', sub_type: 'friend', message_id: -102,
      message: format === 'array' ? [{ type: 'text', data: { text: 'Where can we walk?' } }] : 'Where can we walk?' });
    await until(() => engine.chats.has('private:20'));
    await engine.cycle('private:20');
    assert.equal(server.sends.length, 1);
    assert.equal(server.sends[0].action, 'send_private_msg');
    assert.equal(server.sends[0].params.user_id, 20);
    assert.equal(server.sends[0].params.message[0].type, 'text');
    const version = engine.chats.get('group:10').version;
    server.push({ ...group, user_id: 99, message_id: -103 });
    server.push({ ...group, post_type: 'message_sent', message_id: -104 });
    server.push({ post_type: 'meta_event', meta_event_type: 'heartbeat', status: { online: true, good: true }, interval: 30000 });
    await sleep(50);
    assert.equal(engine.chats.get('group:10').version, version);
  } finally { stop.abort(); await engine.stop(); await running; store.close(); await server.close(); }
});

test('NapCat token rejection after WebSocket upgrade remains disconnected', async () => {
  const server = await mockServer('openai', true), stop = new AbortController();
  const bot = new OneBot({ ...defaults.onebot, url: `ws://127.0.0.1:${server.port}/`, requestTimeoutSeconds: 1 }, 'wrong-token');
  const running = bot.start(stop.signal);
  try {
    await until(() => server.connections > 0); await sleep(100);
    assert.equal(bot.connected, false);
    await assert.rejects(() => bot.send('private:20', 'must not send'));
    assert.equal(server.sends.length, 0);
  } finally { stop.abort(); await running; await server.close(); }
});
