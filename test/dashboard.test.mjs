import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import { once } from 'node:events';
import { defaults, merge, validate } from '../src/config.mjs';
import { publicSettings, saveSettings, recoverSettings, atomicJson, knownConfig } from '../src/settings.mjs';
import { createDashboard } from '../src/dashboard.mjs';
import { Store } from '../src/store.mjs';

function fixture() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'qq-dashboard-test-'));
  fs.mkdirSync(path.join(root, 'data')); fs.mkdirSync(path.join(root, 'web'));
  fs.writeFileSync(path.join(root, 'config.json'), JSON.stringify(defaults));
  fs.writeFileSync(path.join(root, 'secrets.json'), JSON.stringify({ apiKey: 'model-secret', onebotToken: 'qq-secret' }));
  fs.writeFileSync(path.join(root, 'web/index.html'), '<html>test dashboard</html>');
  return root;
}
test('settings validate, hide credentials, reject stale writes and prevent cross-provider key reuse', () => {
  const root = fixture();
  try {
    const s = publicSettings(root); assert.equal(s.hasApiKey, true); assert.ok(!JSON.stringify(s).includes('model-secret'));
    const c = structuredClone(s.config); c.agent.threshold = 3.9;
    const result = saveSettings(root, { revision: s.revision, config: c });
    assert.equal(result.config.agent.threshold, 3.9);
    assert.throws(() => saveSettings(root, { revision: s.revision, config: c }), /changed elsewhere/);
    c.agent.threshold = 8;
    assert.throws(() => saveSettings(root, { revision: result.revision, config: c }), /Invalid agent.threshold/);
    c.agent.threshold = 4; c.provider.baseUrl = 'https://other.example/v1';
    assert.throws(() => saveSettings(root, { revision: result.revision, config: c }), /key for the new provider/);
    c.provider.baseUrl = defaults.provider.baseUrl; c.agent.apiKey = 'injected';
    assert.throws(() => saveSettings(root, { revision: result.revision, config: c }), /Unknown setting/);
    assert.equal(publicSettings(root).config.agent.threshold, 3.9);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});
test('HTTP dashboard authentication, CSRF, validated save, redaction and fixed service actions', async () => {
  const root = fixture(), actions = [], server = http.createServer();
  server.listen(0, '127.0.0.1'); await once(server, 'listening');
  const origin = `http://127.0.0.1:${server.address().port}`;
  const { handler } = createDashboard({ root, settings: { origins: [origin] }, key: 'test-access-key', serviceControl: async action => actions.push(action), serviceStatus: async () => 'active' });
  server.on('request', handler);
  const request = (url, options = {}) => fetch(origin + url, options);
  try {
    assert.equal((await request('/')).status, 200);
    assert.equal((await request('/api/config')).status, 401);
    assert.equal((await request('/api/debug/receive')).status, 401);
    assert.equal((await request('/api/debug/send', { method: 'POST', headers: { Origin: origin } })).status, 401);
    assert.equal((await request('/secrets.json')).status, 401);
    assert.equal((await request('/api/login', { method: 'POST', headers: { 'Content-Type': 'application/json', Origin: 'https://evil.example' }, body: '{"key":"test-access-key"}' })).status, 403);
    const login = await request('/api/login', { method: 'POST', headers: { 'Content-Type': 'application/json', Origin: origin }, body: '{"key":"test-access-key"}' });
    assert.equal(login.status, 200); assert.ok(login.headers.get('set-cookie').includes('HttpOnly'));
    const cookie = login.headers.get('set-cookie').split(';')[0], { csrf } = await login.json();
    const headers = { Cookie: cookie, Origin: origin, 'Content-Type': 'application/json', 'X-CSRF-Token': csrf };
    const s = await (await request('/api/config', { headers })).json();
    assert.ok(!JSON.stringify(s).includes('model-secret'));
    const changed = structuredClone(s.config); changed.agent.threshold = 4.3;
    const noCsrf = { ...headers }; delete noCsrf['X-CSRF-Token'];
    assert.equal((await request('/api/debug/send', { method: 'POST', headers: noCsrf, body: '{}' })).status, 403);
    assert.equal((await request('/api/debug/receive', { method: 'POST', headers: noCsrf, body: '{}' })).status, 403);
    assert.equal((await request('/api/learning/reset', { method: 'POST', headers: noCsrf, body: '{"chat":"private:20"}' })).status, 403);
    assert.equal((await request('/api/learning/reset', { method: 'POST', headers, body: '{"chat":"private:20","subject":"person:21"}' })).status, 400);
    assert.equal((await request('/api/learning/reset', { method: 'POST', headers, body: '{"chat":"private:20","subject":"group"}' })).status, 400);
    const memoryStore = new Store(path.join(root, 'data/agent.sqlite'));
    memoryStore.memory.apply('private:20', [{ subject: 'person:20', layer: 'traits', key: '互动风格', operation: 'upsert', text: '短句接话', importance: 0.8, sources: [] }], Date.now()/1000, defaults.agent.memory);
    assert.equal((await (await request('/api/state', { headers })).json()).memories[0].text, '短句接话');
    assert.equal((await request('/api/learning/reset', { method: 'POST', headers, body: '{"chat":"private:20"}' })).status, 200);
    assert.equal(memoryStore.memory.context('private:20', '20', Date.now()/1000, defaults.agent.memory)[0].traits.length, 0); memoryStore.close();
    assert.equal((await (await request('/api/debug/receive', { headers })).json()).state, 'idle');
    assert.equal((await request('/api/debug/stop', { method: 'POST', headers, body: '{}' })).status, 200);
    assert.equal((await request('/api/config', { method: 'PUT', headers: noCsrf, body: JSON.stringify({ revision: s.revision, config: changed }) })).status, 403);
    const save = await request('/api/config', { method: 'PUT', headers, body: JSON.stringify({ revision: s.revision, config: changed }) });
    assert.equal(save.status, 200); assert.equal((await save.json()).config.agent.threshold, 4.3);
    assert.equal((await request('/api/service', { method: 'POST', headers, body: '{"action":"restart; arbitrary shell"}' })).status, 400);
    assert.equal((await request('/api/service', { method: 'POST', headers, body: '{"action":"restart"}' })).status, 200);
    assert.deepEqual(actions, ['restart']);
    fs.writeFileSync(path.join(root, 'data/status.json'), JSON.stringify({ updatedAt: new Date().toISOString(), telegramConnected: true, telegramOnline: true, telegramSelfId: '123456789', telegramUsername: 'example_bot', telegramLastError: '' }) + '\n');
    fs.writeFileSync(path.join(root, 'data/agent.log'), JSON.stringify({ time: new Date().toISOString(), event: 'accidental', info: 'model-secret qq-secret' }) + '\n');
    const state = await (await request('/api/state', { headers })).text();
    assert.ok(!state.includes('model-secret')); assert.ok(state.includes('[redacted]'));
    // Telegram 状态字段必须从 status.json 原样透传，不被裁剪。
    const stateJson = JSON.parse(state);
    assert.equal(stateJson.status.telegramConnected, true);
    assert.equal(stateJson.status.telegramOnline, true);
    assert.equal(stateJson.status.telegramSelfId, '123456789');
    assert.equal(stateJson.status.telegramUsername, 'example_bot');
    assert.equal(stateJson.status.telegramLastError, '');
    await request('/api/logout', { method: 'POST', headers, body: '{}' });
    assert.equal((await request('/api/config', { headers })).status, 401);
  } finally { server.closeAllConnections(); await new Promise(r => server.close(r)); fs.rmSync(root, { recursive: true, force: true }); }
});


test('telegram configuration validates gateway IDs, proxy and token handling', () => {
  const root = fixture();
  try {
    // 与 rust/src/defaults.json 的 telegram 段逐字段一致。
    assert.deepEqual(defaults.telegram, { enabled: false, proxy: '', pollTimeoutSeconds: 20, requestTimeoutSeconds: 30, reconnectMaxSeconds: 60, allowedGroups: [], allowedUsers: [] });
    const s = publicSettings(root);
    assert.equal(s.hasTelegramToken, false);
    const c = structuredClone(s.config);
    // 群 id 可负（超级群），数字元素按 String() 归一化。
    c.telegram = { ...c.telegram, enabled: true, proxy: 'http://127.0.0.1:8080', allowedGroups: [-1001234567890, '10'], allowedUsers: [123456789] };
    const saved = saveSettings(root, { revision: s.revision, config: c, telegramToken: 'bot-secret' });
    assert.equal(saved.hasTelegramToken, true);
    assert.deepEqual(saved.config.telegram.allowedGroups, ['-1001234567890', '10']);
    assert.deepEqual(saved.config.telegram.allowedUsers, ['123456789']);
    assert.ok(!JSON.stringify(saved).includes('bot-secret'));
    const cleared = saveSettings(root, { revision: saved.revision, config: saved.config, clearTelegramToken: true });
    assert.equal(cleared.hasTelegramToken, false);
    for (const bad of [
      { enabled: 'yes' },
      { proxy: 'ftp://example.com' },
      { proxy: 'http://user:pass@example.com' },
      { proxy: 'http://example.com/?key=secret' },
      { proxy: 'http://example.com/#fragment' },
      { allowedGroups: ['abc'] },
      { allowedGroups: 'secret' },
      { allowedGroups: null },
      { allowedUsers: ['-1'] },
      { pollTimeoutSeconds: 51 },
      { requestTimeoutSeconds: 0 },
      { reconnectMaxSeconds: 301 },
    ]) {
      const invalid = structuredClone(cleared.config);
      invalid.telegram = { ...invalid.telegram, ...bad };
      assert.throws(() => saveSettings(root, { revision: cleared.revision, config: invalid }), /telegram/);
    }
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test('interrupted two-file settings save rolls back before dashboard startup', () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'qq-settings-recover-'));
  try {
    const config = structuredClone(defaults), secrets = { apiKey: 'old-key' };
    atomicJson(path.join(root, 'config.json'), config);
    atomicJson(path.join(root, 'secrets.json'), secrets);
    atomicJson(path.join(root, '.settings-write'), { config, secrets });
    atomicJson(path.join(root, 'secrets.json'), { apiKey: 'new-key' });
    recoverSettings(root);
    assert.equal(JSON.parse(fs.readFileSync(path.join(root, 'secrets.json'))).apiKey, 'old-key');
    assert.equal(fs.existsSync(path.join(root, '.settings-write')), false);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});
