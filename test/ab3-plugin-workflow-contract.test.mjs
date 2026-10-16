// AB3 acceptance contract at the authenticated dashboard boundary. The route
// names and response fields below are the test author's proposed HTTP contract.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import net from 'node:net';
import { createDashboard } from '../src/dashboard.mjs';

function manifest(name, { hotReload = true, transport = 'Remote', entry = { type: 'http' } } = {}) {
  return {
    name, version: '1.0.0', kind: 'Model', transport, hot_reload: hotReload,
    core_api_version: { major: 1, min_minor: 0, max_minor: null },
    protocol: { name: 'http-json', schema_version: '1.0', schema: 'model-v1' }, entry,
    capabilities: { supported: ['text_completion'], unsupported: [], unknown: [] },
    dependencies: [], host_permissions: [],
    config_schema: [
      { key: 'base_url', label: 'URL', type: 'string', required: true, secret: false },
      { key: 'model', label: 'Model', type: 'string', required: true, secret: false },
      { key: 'api_key', label: 'Key', type: 'string', required: false, secret: true },
    ],
  };
}

function entry(id, enabled = false) {
  return { id, directory: id, enabled,
    params: { base_url: 'http://127.0.0.1:43219/v1', model: 'fixture' },
    bindings: {}, secret_refs: {} };
}

function pluginList(plugins) { return { schema_version: 1, mode: 'manual', plugins }; }

async function modelEndpoint(t) {
  const server = net.createServer(socket => socket.end());
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise(resolve => server.close(resolve)));
  return `http://127.0.0.1:${server.address().port}/v1`;
}

async function fixture(t, { threshold = 16 } = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ab3-plugin-contract-'));
  fs.mkdirSync(path.join(root, 'plugins'));
  fs.mkdirSync(path.join(root, 'data'));
  fs.writeFileSync(path.join(root, 'config.json'), '{}\n');
  fs.writeFileSync(path.join(root, 'secrets.json'), '{}\n');
  fs.writeFileSync(path.join(root, 'data/status.json'), JSON.stringify({
    updatedAt: new Date().toISOString(), appliedRevision: 'old-running-revision',
    plugins: { openai: { effective_state: 'active', instanceRevision: 'old-running-revision' } },
  }));
  const serviceActions = [];
  let dashboard;
  const server = http.createServer((req, res) => dashboard.handler(req, res, { address: req.socket.remoteAddress }));
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const origin = `http://127.0.0.1:${server.address().port}`;
  dashboard = createDashboard({ root, key: 'fixture-access-key',
    settings: { origins: [origin], pluginRestartThreshold: threshold },
    serviceControl: async action => { serviceActions.push(action); }, serviceStatus: async () => 'active' });
  t.after(async () => {
    await dashboard.close();
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
    fs.rmSync(root, { recursive: true, force: true });
  });
  const login = await fetch(`${origin}/api/login`, { method: 'POST',
    headers: { origin, 'content-type': 'application/json' },
    body: JSON.stringify({ key: 'fixture-access-key' }) });
  assert.equal(login.status, 200, 'dashboard fixture must authenticate');
  const cookie = login.headers.get('set-cookie').split(';')[0];
  const { csrf } = await login.json();
  async function request(route, { method = 'GET', body } = {}) {
    const response = await fetch(`${origin}${route}`, { method, headers: {
      cookie, ...(method !== 'GET' ? { origin, 'x-csrf-token': csrf } : {}),
      ...(body === undefined ? {} : { 'content-type': 'application/json' }),
    }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
    const text = await response.text();
    let data; try { data = JSON.parse(text); } catch { data = text; }
    return { status: response.status, data };
  }
  const listPath = path.join(root, 'plugins/plugins.json');
  const addPackage = (name, value = manifest(name)) => {
    const directory = path.join(root, 'plugins', name);
    fs.mkdirSync(directory, { recursive: true });
    if (value !== null) fs.writeFileSync(path.join(directory, 'manifest.json'),
      typeof value === 'string' ? value : JSON.stringify(value));
    return directory;
  };
  return { root, origin, request, listPath, addPackage, serviceActions };
}

test('recognition reports every directory without constructing ports, starting a Sidecar, or writing a list', async t => {
  const f = await fixture(t);
  const marker = path.join(f.root, 'sidecar-started');
  const canary = f.addPackage('canary', manifest('canary', {
    transport: 'Sidecar', entry: { executable: 'start.sh' },
  }));
  const script = path.join(canary, 'start.sh');
  fs.writeFileSync(script, `#!/bin/sh\ntouch '${marker}'\nexec sleep 5\n`, { mode: 0o700 });
  fs.chmodSync(script, 0o700);
  f.addPackage('orphan', null);
  f.addPackage('placeholder', { ...manifest('placeholder'), version: 'TBD', status: 'placeholder' });
  f.addPackage('broken', '{bad json');
  const probe = net.createServer(socket => { probe.connections++; socket.end(); });
  probe.connections = 0;
  await new Promise(resolve => probe.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise(resolve => probe.close(resolve)));
  const selected = entry('canary', true);
  selected.params.base_url = `http://127.0.0.1:${probe.address().port}/v1`;
  const existing = Buffer.from(JSON.stringify(pluginList([selected])) + '\n');
  fs.writeFileSync(f.listPath, existing);

  const found = await f.request('/api/plugins/discover');
  assert.equal(fs.existsSync(marker), false, 'recognition must not start a selected Sidecar');
  assert.equal(probe.connections, 0, 'recognition must not construct and probe the selected port');
  assert.deepEqual(fs.readFileSync(f.listPath), existing, 'recognition must not write the existing list');
  assert.equal(found.status, 200, JSON.stringify(found.data));
  const rows = found.data.discovery;
  assert.ok(Array.isArray(rows), 'recognition must provide user-visible directory results');
  const byName = name => rows.find(row => row.directory === name);
  for (const name of ['canary', 'orphan', 'placeholder', 'broken']) {
    assert.ok(byName(name), `${name} must appear even if its manifest is unusable`);
  }
  assert.match(JSON.stringify(byName('orphan')), /manifest_missing|missing_manifest/);
  assert.match(JSON.stringify(byName('placeholder')), /placeholder|TBD/i);
  assert.match(JSON.stringify(byName('broken')), /invalid_json|parse|malformed/i);

  fs.unlinkSync(f.listPath);
  const absent = await f.request('/api/plugins/discover');
  assert.equal(absent.status, 200, JSON.stringify(absent.data));
  assert.equal(fs.existsSync(f.listPath), false, 'recognition must not create plugins.json when absent');
});

test('editing stages a durable, inspectable draft while live bytes and revision stay unchanged', async t => {
  const f = await fixture(t);
  f.addPackage('openai');
  const original = Buffer.from('{"schema_version":1,"mode":"manual","plugins":[]}\n');
  fs.writeFileSync(f.listPath, original);
  const before = await f.request('/api/state');
  const staged = await f.request('/api/plugins/draft', { method: 'PUT',
    body: { list: pluginList([entry('openai')]) } });
  assert.deepEqual(fs.readFileSync(f.listPath), original, 'editing must not apply the live list');
  const after = await f.request('/api/state');
  assert.equal(after.data.status?.appliedRevision, before.data.status?.appliedRevision,
    'staging must not advance the running revision');
  assert.equal(staged.status, 200, JSON.stringify(staged.data));
  const preview = await f.request('/api/plugins/draft');
  assert.equal(preview.status, 200, JSON.stringify(preview.data));
  assert.match(JSON.stringify(preview.data), /openai/, 'draft must show the proposed plugin change');
  assert.match(JSON.stringify(preview.data), /changes|preview|diff/i, 'draft must show what will be applied');
  const afterRefresh = await f.request('/api/plugins/draft');
  assert.deepEqual(afterRefresh.data, preview.data, 'a page refresh must not discard the draft');
});

test('cancel has no runtime effect; a changed baseline rejects the entire activation without rewriting bytes', async t => {
  const f = await fixture(t);
  f.addPackage('openai');
  const original = Buffer.from(JSON.stringify(pluginList([])) + '\n');
  fs.writeFileSync(f.listPath, original);
  await f.request('/api/plugins/draft', { method: 'PUT', body: { list: pluginList([entry('openai')]) } });
  const cancelled = await f.request('/api/plugins/draft', { method: 'DELETE' });
  assert.equal(cancelled.status, 200, JSON.stringify(cancelled.data));
  assert.deepEqual(fs.readFileSync(f.listPath), original);
  assert.deepEqual(f.serviceActions, [], 'cancel must not restart or reload the framework');
  const emptyDraft = await f.request('/api/plugins/draft');
  assert.doesNotMatch(JSON.stringify(emptyDraft.data), /"id":"openai"/);

  const staged = await f.request('/api/plugins/draft', { method: 'PUT',
    body: { list: pluginList([entry('openai')]) } });
  assert.equal(staged.status, 200, JSON.stringify(staged.data));
  const manualEdit = Buffer.from(JSON.stringify(pluginList([{ ...entry('openai'), id: 'owner-edit' }]), null, 2) + '\n');
  fs.writeFileSync(f.listPath, manualEdit);
  const result = await f.request('/api/plugins/activate', { method: 'POST', body: {} });
  assert.equal(result.status, 409, 'stale draft must be rejected as one batch');
  assert.deepEqual(fs.readFileSync(f.listPath), manualEdit, 'conflict must preserve exact user-edited bytes');
  assert.deepEqual(f.serviceActions, [], 'conflicted activation must not restart the framework');
});

test('activation preserves each manual field and writes secret references', async t => {
  const f = await fixture(t);
  f.addPackage('openai'); f.addPackage('ollama');
  const manual = { ...entry('openai'), id: 'owner-model', enabled: false,
    params: { base_url: 'http://127.0.0.1:43219/owner', model: 'owner-choice' },
    bindings: {}, secret_refs: { api_key: 'secrets.ownerKey' } };
  fs.writeFileSync(f.listPath, JSON.stringify(pluginList([manual])) + '\n');
  const generated = entry('ollama');
  generated.secret_refs = { api_key: 'secrets.newKey' };
  const staged = await f.request('/api/plugins/draft', { method: 'PUT',
    body: { list: pluginList([generated]) } });
  assert.equal(staged.status, 200, JSON.stringify(staged.data));
  const applied = await f.request('/api/plugins/activate', { method: 'POST', body: {} });
  assert.equal(applied.status, 200, JSON.stringify(applied.data));
  const text = fs.readFileSync(f.listPath, 'utf8');
  const list = JSON.parse(text);
  assert.deepEqual(list.plugins.find(item => item.id === 'owner-model'), manual,
    'activation must retain every field of the user-edited entry');
  assert.equal(list.plugins.find(item => item.id === 'ollama')?.secret_refs?.api_key, 'secrets.newKey');
});

test('a raw secret submitted as a plugin parameter never enters plugins.json', async t => {
  const f = await fixture(t);
  f.addPackage('openai');
  const original = Buffer.from(JSON.stringify(pluginList([])) + '\n');
  fs.writeFileSync(f.listPath, original);
  const unsafe = entry('openai');
  unsafe.params.api_key = 'PRIVATE-CANARY-DO-NOT-WRITE';
  const staged = await f.request('/api/plugins/draft', { method: 'PUT',
    body: { list: pluginList([unsafe]) } });
  assert.notEqual(staged.status, 404, 'plugin staging must exist before secret handling can be verified');
  if (staged.status >= 400) {
    assert.ok([400, 422].includes(staged.status), 'raw secret must receive a validation error');
    assert.deepEqual(fs.readFileSync(f.listPath), original, 'rejected secret must not alter the live list');
    return;
  }
  const applied = await f.request('/api/plugins/activate', { method: 'POST', body: {} });
  const text = fs.readFileSync(f.listPath, 'utf8');
  assert.equal(text.includes('PRIVATE-CANARY-DO-NOT-WRITE'), false,
    'a supplied secret value must never be written to plugins.json');
  if (applied.status === 200) {
    assert.ok(JSON.parse(text).plugins.find(item => item.id === 'openai')?.secret_refs?.api_key,
      'an accepted secret must be represented by a reference');
  } else {
    assert.deepEqual(fs.readFileSync(f.listPath), original, 'rejected activation must preserve the live list');
  }
});

test('non-hot-reload plugin and a batch at the configured scale threshold use whole-service restart', async t => {
  const cold = await fixture(t);
  const coldEndpoint = await modelEndpoint(t);
  cold.addPackage('openai', manifest('openai', { hotReload: false }));
  const coldEntry = entry('openai', true); coldEntry.params.base_url = coldEndpoint;
  await cold.request('/api/plugins/draft', { method: 'PUT', body: { list: pluginList([coldEntry]) } });
  const applied = await cold.request('/api/plugins/activate', { method: 'POST', body: {} });
  assert.equal(applied.status, 200, JSON.stringify(applied.data));
  assert.deepEqual(cold.serviceActions, ['restart'], 'hot_reload:false requires a whole-framework restart');

  const large = await fixture(t, { threshold: 2 });
  const largeEndpoint = await modelEndpoint(t);
  large.addPackage('openai'); large.addPackage('claude');
  const first = entry('openai', true), second = entry('claude', true);
  first.params.base_url = largeEndpoint; second.params.base_url = largeEndpoint;
  await large.request('/api/plugins/draft', { method: 'PUT',
    body: { list: pluginList([first, second]) } });
  const batch = await large.request('/api/plugins/activate', { method: 'POST', body: {} });
  assert.equal(batch.status, 200, JSON.stringify(batch.data));
  assert.deepEqual(large.serviceActions, ['restart'], 'the configured batch threshold requires one restart');
});

test('failed hot replacement names the plugin and old live revision; success shows the core-applied revision', async t => {
  const f = await fixture(t);
  const endpoint = await modelEndpoint(t);
  f.addPackage('openai');
  const replacement = entry('openai', true); replacement.params.base_url = endpoint;
  await f.request('/api/plugins/draft', { method: 'PUT', body: { list: pluginList([replacement]) } });
  const activation = await f.request('/api/plugins/activate', { method: 'POST', body: {} });
  assert.equal(activation.status, 200, JSON.stringify(activation.data));
  const target = activation.data.targetRevision;
  assert.ok(typeof target === 'string' && target !== 'old-running-revision',
    'activation must identify the desired revision, distinct from the running one');
  fs.writeFileSync(path.join(f.root, 'data/status.json'), JSON.stringify({
    updatedAt: new Date().toISOString(), appliedRevision: 'old-running-revision',
    reloadError: 'plugin batch rejected: openai backend_unavailable',
    plugins: { openai: { effective_state: 'active', instanceRevision: 'old-running-revision' } },
  }));
  const failed = await f.request('/api/plugins/status');
  assert.equal(failed.status, 200, JSON.stringify(failed.data));
  assert.equal(failed.data.targetRevision, target);
  assert.equal(failed.data.appliedRevision, 'old-running-revision');
  assert.equal(failed.data.alerts?.[0]?.pluginId, 'openai', 'alert must identify the failed plugin');
  assert.equal(failed.data.oldInstanceRunning, true, 'status must say the old instance remains live');

  fs.writeFileSync(path.join(f.root, 'data/status.json'), JSON.stringify({
    updatedAt: new Date().toISOString(), appliedRevision: target, reloadError: null,
    plugins: { openai: { effective_state: 'active', instanceRevision: target } },
  }));
  const succeeded = await f.request('/api/plugins/status');
  assert.equal(succeeded.status, 200, JSON.stringify(succeeded.data));
  assert.equal(succeeded.data.appliedRevision, target, 'show the core-applied revision, not merely saved bytes');
  assert.equal(succeeded.data.pending, false);
});
