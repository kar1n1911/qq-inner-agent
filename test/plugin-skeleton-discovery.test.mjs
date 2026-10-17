import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import { createDashboard } from '../src/dashboard.mjs';

test('dashboard projects skeleton status and refuses its activation', async t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'plugin-skeleton-'));
  fs.mkdirSync(path.join(root, 'plugins', 'unfinished'), { recursive: true });
  fs.mkdirSync(path.join(root, 'data'));
  fs.writeFileSync(path.join(root, 'config.json'), '{}\n');
  fs.writeFileSync(path.join(root, 'secrets.json'), '{}\n');
  fs.writeFileSync(path.join(root, 'plugins', 'unfinished', 'manifest.json'), JSON.stringify({
    name: 'unfinished', version: '0.1.0', kind: 'Multiple', transport: 'InProcess', status: 'skeleton',
    core_api_version: { major: 1, min_minor: 0 }, capabilities: { supported: [] },
    dependencies: [], host_permissions: [], config_schema: [], hot_reload: false,
  }));
  fs.writeFileSync(path.join(root, 'plugins', 'plugins.json'), JSON.stringify({
    schema_version: 1, mode: 'auto', plugins: [{ id: 'unfinished', directory: 'unfinished', enabled: false }],
  }));
  let dashboard;
  const server = http.createServer((req, res) => dashboard.handler(req, res, { address: req.socket.remoteAddress }));
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const origin = `http://127.0.0.1:${server.address().port}`;
  dashboard = createDashboard({ root, key: 'test-key', settings: { origins: [origin] },
    serviceControl: async () => {}, serviceStatus: async () => 'active' });
  t.after(async () => {
    await dashboard.close();
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
    fs.rmSync(root, { recursive: true, force: true });
  });
  const login = await fetch(`${origin}/api/login`, { method: 'POST', headers: { origin, 'content-type': 'application/json' },
    body: JSON.stringify({ key: 'test-key' }) });
  assert.equal(login.status, 200);
  const cookie = login.headers.get('set-cookie').split(';')[0];
  const { csrf } = await login.json();
  const headers = { cookie, origin, 'x-csrf-token': csrf, 'content-type': 'application/json' };
  const discovery = await (await fetch(`${origin}/api/plugins/discover`, { headers })).json();
  assert.equal(discovery.discovery[0].state, 'skeleton');
  assert.equal(discovery.discovery[0].reason, 'plugin_is_skeleton');
  assert.equal(discovery.list.plugins[0].enabled, false);
  const status = await (await fetch(`${origin}/api/plugins/status`, { headers })).json();
  assert.equal(status.plugins.unfinished.effective_state, 'skeleton');
  const draft = await fetch(`${origin}/api/plugins/draft`, { method: 'PUT', headers, body: JSON.stringify({
    list: { schema_version: 1, mode: 'auto', plugins: [{ id: 'unfinished', directory: 'unfinished', enabled: true }] },
  }) });
  assert.equal(draft.status, 422);
  assert.match(JSON.stringify(await draft.json()), /plugin_is_skeleton/);
});
