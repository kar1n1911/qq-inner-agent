import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { createDashboard } from '../src/dashboard.mjs';

const repository = path.dirname(fileURLToPath(new URL('../package.json', import.meta.url)));
const revision = bytes => createHash('sha256').update(bytes).digest('hex');
const item = (id, directory = id, enabled = false, secret_refs = {}) =>
  ({ id, directory, enabled, params: {}, bindings: {}, secret_refs });
const list = plugins => ({ schema_version: 1, mode: 'manual', plugins });

function manifest(name, overrides = {}) {
  return {
    name, version: '1.0.0', kind: 'Model', transport: 'Sidecar',
    core_api_version: { major: 1, min_minor: 0, max_minor: null },
    protocol: { name: 'fixture', schema_version: '1' },
    entry: { executable: 'sidecar/run' },
    capabilities: { supported: ['chat_completion'], unsupported: [], unknown: [] },
    dependencies: [{ role: 'MemoryPort', needed_for: 'optional_context', required: false }],
    host_permissions: [],
    config_schema: [{ key: 'api_key', label: 'API key', type: 'string', required: false, secret: true }],
    hot_reload: true,
    ...overrides,
  };
}

async function fixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ab2-page-'));
  fs.mkdirSync(path.join(root, 'plugins'));
  fs.mkdirSync(path.join(root, 'data'));
  fs.symlinkSync(path.join(repository, 'web'), path.join(root, 'web'), 'dir');
  fs.writeFileSync(path.join(root, 'config.json'), '{}\n');
  fs.writeFileSync(path.join(root, 'secrets.json'), '{}\n');
  let app;
  const server = http.createServer((req, res) => app.handler(req, res));
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const origin = `http://127.0.0.1:${server.address().port}`;
  app = createDashboard({ root, key: 'fixture-access-key', settings: { origins: [origin] },
    serviceControl: async () => {}, serviceStatus: async () => 'active' });
  const login = await fetch(`${origin}/api/login`, {
    method: 'POST', headers: { Origin: origin, 'Content-Type': 'application/json' },
    body: JSON.stringify({ key: 'fixture-access-key' }),
  });
  assert.equal(login.status, 200);
  const { csrf } = await login.json();
  const cookie = login.headers.get('set-cookie').split(';')[0];
  t.after(async () => {
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
    fs.rmSync(root, { recursive: true, force: true });
  });
  return {
    root,
    package(name, specification = manifest(name)) {
      const dir = path.join(root, 'plugins', name);
      fs.mkdirSync(dir, { recursive: true });
      fs.writeFileSync(path.join(dir, 'manifest.json'), JSON.stringify(specification));
      return dir;
    },
    live(value) {
      const bytes = JSON.stringify(value, null, 2) + '\n';
      fs.writeFileSync(path.join(root, 'plugins', 'plugins.json'), bytes);
      return { bytes, revision: revision(bytes) };
    },
    running(value) { fs.writeFileSync(path.join(root, 'data', 'status.json'), JSON.stringify(value)); },
    async request(route, method = 'GET', body) {
      const response = await fetch(`${origin}${route}`, {
        method, headers: { Origin: origin, Cookie: cookie, 'X-CSRF-Token': csrf,
          ...(body === undefined ? {} : { 'Content-Type': 'application/json' }) },
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      });
      return { status: response.status, text: await response.text() };
    },
  };
}

function parsed(response) {
  assert.equal(response.status, 200, response.text);
  return JSON.parse(response.text);
}

test('the page shows every installed package, including placeholders and unrecognized directories', async t => {
  const f = await fixture(t);
  f.package('ready');
  f.package('placeholder', manifest('placeholder', { version: 'TBD', status: 'placeholder' }));
  fs.mkdirSync(path.join(f.root, 'plugins', 'missing_manifest'));
  const discovered = parsed(await f.request('/api/plugins/discover')).discovery;
  assert.deepEqual(new Set(discovered.map(row => row.directory)),
    new Set(['ready', 'placeholder', 'missing_manifest']));
  for (const id of ['placeholder', 'missing_manifest']) {
    const row = discovered.find(entry => entry.directory === id);
    assert.equal(row.state, 'unrecognized');
    assert.ok(row.reason, `${id} needs a visible reason`);
  }
  const page = parsed(await f.request('/api/plugins/status'));
  for (const id of ['ready', 'placeholder', 'missing_manifest']) {
    assert.ok(page.plugins?.[id], `plugin page must include ${id}, not only recently changed IDs`);
  }
  assert.notEqual(page.plugins.placeholder.effective_state, 'active');
});

test('declared capabilities and measured PortState remain separate; absent evidence is unknown', async t => {
  const f = await fixture(t);
  f.package('model');
  const live = f.live(list([item('model', 'model', true)]));
  f.running({ pluginAppliedRevision: live.revision, plugins: { model: {
    effective_state: 'degraded', instanceRevision: live.revision,
    capabilities: { supported: [], unsupported: ['chat_completion'], unknown: [] },
    portState: 'unavailable',
  } } });
  const status = parsed(await f.request('/api/plugins/status'));
  const row = status.plugins?.model;
  assert.ok(row, 'enabled instance must be queryable');
  assert.ok(row.declaredCapabilities?.supported?.includes('chat_completion'));
  assert.ok(row.effectiveCapabilities?.unsupported?.includes('chat_completion'));
  assert.deepEqual(row.dependencies, [{ role: 'MemoryPort', needed_for: 'optional_context', required: false }]);
  assert.notEqual(row.effective_state, 'active');
  assert.equal(row.portState, 'unavailable');
  f.running({ pluginAppliedRevision: live.revision, plugins: {} });
  const withoutEvidence = parsed(await f.request('/api/plugins/status')).plugins?.model;
  assert.equal(withoutEvidence?.portState, 'unknown');
  assert.notEqual(withoutEvidence?.effective_state, 'active');
});

test('opening and refreshing the plugin page never prepares, probes, or starts a sidecar', async t => {
  const f = await fixture(t);
  let probeConnections = 0;
  const probe = http.createServer((_req, res) => res.end('ok'));
  probe.on('connection', () => { probeConnections++; });
  await new Promise(resolve => probe.listen(0, '127.0.0.1', resolve));
  t.after(async () => {
    probe.closeAllConnections();
    await new Promise(resolve => probe.close(resolve));
  });
  const dir = f.package('canary');
  const marker = path.join(f.root, 'constructed');
  fs.mkdirSync(path.join(dir, 'sidecar'));
  fs.writeFileSync(path.join(dir, 'sidecar', 'run'), `#!/bin/sh\nprintf started > '${marker}'\n`);
  fs.chmodSync(path.join(dir, 'sidecar', 'run'), 0o755);
  f.package('openai', manifest('openai', { transport: 'Remote',
    capabilities: { supported: ['text_completion'], unsupported: [], unknown: [] },
    config_schema: [{ key: 'base_url', label: 'Base URL', type: 'string', required: true, secret: false }],
  }));
  const model = item('model', 'openai', true);
  model.params.base_url = `http://127.0.0.1:${probe.address().port}/v1`;
  const live = f.live(list([item('canary', 'canary', true), model]));
  for (let refresh = 0; refresh < 2; refresh++) {
    assert.equal((await f.request('/')).status, 200);
    parsed(await f.request('/api/plugins/discover'));
    parsed(await f.request('/api/plugins/draft'));
    parsed(await f.request('/api/plugins/status'));
  }
  assert.equal(fs.existsSync(marker), false, 'sidecar construction count must remain zero');
  assert.equal(probeConnections, 0, 'page reads must not probe the model endpoint');
  assert.equal(fs.readFileSync(path.join(f.root, 'plugins', 'plugins.json'), 'utf8'), live.bytes);
  assert.equal(fs.existsSync(path.join(f.root, 'data', 'plugin-draft.json')), false);
});

test('plugin activation cannot be inferred from the ordinary configuration revision', async t => {
  const f = await fixture(t);
  f.package('model');
  const live = f.live(list([item('model', 'model', true)]));
  f.running({ appliedRevision: live.revision, plugins: {} });
  const status = parsed(await f.request('/api/plugins/status'));
  assert.equal(status.targetRevision, live.revision);
  assert.equal(status.appliedRevision, null, 'only pluginAppliedRevision proves plugin activation');
  assert.equal(status.pending, true);
  assert.notEqual(status.plugins?.model?.effective_state, 'active');
});

test('secret values never enter page responses, drafts, lists or previews; each field reports actual configuration', async t => {
  const f = await fixture(t);
  f.package('present');
  f.package('absent');
  const sentinel = 'AB2_SECRET_SENTINEL_4d13a8a1';
  fs.writeFileSync(path.join(f.root, 'secrets.json'), JSON.stringify({ present_key: sentinel }));
  const desired = list([item('present', 'present', false, { api_key: 'secrets.present_key' }),
    item('absent', 'absent', false, { api_key: 'secrets.absent_key' })]);
  const responses = [await f.request('/'), await f.request('/app.js'),
    await f.request('/api/plugins/discover'),
    await f.request('/api/plugins/draft', 'PUT', { list: desired, replaceIds: [] }),
    await f.request('/api/plugins/draft'), await f.request('/api/plugins/status'),
    await f.request('/api/state')];
  for (const response of responses) {
    assert.equal(response.status, 200, response.text);
    assert.equal(response.text.includes(sentinel), false, 'secret leaked in a page response');
  }
  const draftBytes = fs.readFileSync(path.join(f.root, 'data', 'plugin-draft.json'), 'utf8');
  assert.equal(draftBytes.includes(sentinel), false);
  const livePath = path.join(f.root, 'plugins', 'plugins.json');
  assert.equal(fs.existsSync(livePath), false, 'staging must not create the live list');
  const status = JSON.parse(responses[5].text);
  assert.equal(status.plugins?.present?.secrets?.api_key?.configured, true);
  assert.equal(status.plugins?.absent?.secrets?.api_key?.configured, false,
    'a secret reference alone does not mean the secret exists');
  const activated = await f.request('/api/plugins/activate', 'POST', {});
  assert.equal(activated.status, 200, activated.text);
  assert.equal(activated.text.includes(sentinel), false);
  assert.equal(fs.readFileSync(livePath, 'utf8').includes(sentinel), false);
});

test('a single plugin enable edit survives refresh as a draft and leaves the running revision unchanged', async t => {
  const f = await fixture(t);
  f.package('first');
  f.package('second');
  const first = item('first');
  const second = item('second');
  const live = f.live(list([first, second]));
  f.running({ pluginAppliedRevision: live.revision, plugins: {} });
  const edited = list([{ ...first, enabled: true }, second]);
  const staged = parsed(await f.request('/api/plugins/draft', 'PUT',
    { list: edited, replaceIds: ['first'] }));
  assert.deepEqual(staged.changes.map(change => change.id), ['first']);
  assert.deepEqual(staged.preview.target.plugins.find(entry => entry.id === 'second'), second);
  assert.equal(fs.readFileSync(path.join(f.root, 'plugins', 'plugins.json'), 'utf8'), live.bytes);
  const afterRefresh = parsed(await f.request('/api/plugins/draft'));
  assert.equal(afterRefresh.list.plugins.find(entry => entry.id === 'first').enabled, true);
  assert.equal(parsed(await f.request('/api/plugins/status')).appliedRevision, live.revision);
});

test('connectivity checks run only on request and unsupported probes are reported honestly', async t => {
  const f = await fixture(t);
  f.package('openai', manifest('openai', { transport: 'Remote',
    capabilities: { supported: ['text_completion'], unsupported: [], unknown: [] },
    config_schema: [{ key: 'base_url', label: 'Base URL', type: 'string', required: true, secret: false }],
  }));
  f.package('persona', manifest('persona', { kind: 'Persona', capabilities:
    { supported: [], unsupported: [], unknown: ['profile'] } }));
  const model = item('model', 'openai', true);
  model.params.base_url = 'http://127.0.0.1:1/v1';
  f.live(list([model, item('persona')]));
  parsed(await f.request('/api/plugins/discover'));
  const before = parsed(await f.request('/api/plugins/status'));
  assert.notEqual(before.plugins?.model?.selfTest?.state, 'connected',
    'page load cannot claim connectivity without a probe');
  const unsupported = await f.request('/api/plugins/self-test', 'POST', { id: 'persona' });
  assert.equal(unsupported.status, 200, unsupported.text);
  assert.equal(JSON.parse(unsupported.text).state, 'unsupported');
  const failed = await f.request('/api/plugins/self-test', 'POST', { id: 'model' });
  assert.equal(failed.status, 200, failed.text);
  assert.equal(JSON.parse(failed.text).state, 'failed',
    'construction or manifest discovery does not prove connectivity');
});
