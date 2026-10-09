import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import vm from 'node:vm';
import { defaults } from '../src/config.mjs';
import { publicSettings, saveSettings, atomicJson, recoverSettings } from '../src/settings.mjs';

function fixture(t, config = defaults) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'qq-patch-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.writeFileSync(path.join(root, 'config.json'), JSON.stringify(config, null, 2) + '\n');
  fs.writeFileSync(path.join(root, 'secrets.json'), '{ "apiKey": "old-key", "onebotToken": "old-token" }\n');
  return root;
}
const read = root => fs.readFileSync(path.join(root, 'config.json'), 'utf8');
const save = (root, base, patch, extra = {}) => saveSettings(root, { revision: base.revision, patch, ...extra });
const sorted = value => value && typeof value === 'object' && !Array.isArray(value)
  ? Object.fromEntries(Object.keys(value).sort().map(k => [k, sorted(value[k])])) : value;

test('scalar and nested patches preserve every other byte, order, escapes and CRLF', t => {
  const config = sorted(defaults);
  config.localExtension = { secret: 'keep private', number: 10 };
  const root = fixture(t, config), file = path.join(root, 'config.json');
  fs.writeFileSync(file, read(root).replace('"number": 10', '"number": 1e1').replace('"name": "Luma"', '"name": "\\u004cuma"').replaceAll('\n', '\r\n'));
  for (const [key, from, to] of [['agent.threshold', 4.09, 3.9], ['agent.sending.settleSeconds', 15, 1]]) {
    const before = read(root), base = publicSettings(root);
    assert.equal(base.config.localExtension, undefined);
    save(root, base, { [key]: to });
    const after = read(root), field = key.split('.').at(-1);
    assert.equal(after, before.replace(`"${field}": ${from}`, `"${field}": ${to}`));
    assert.equal(after.split('\n').filter((line, i) => line !== before.split('\n')[i]).length, 1);
    assert.deepEqual(Object.keys(JSON.parse(after)), Object.keys(JSON.parse(before)));
  }
  assert.equal(fs.readFileSync(path.join(root, 'secrets.json'), 'utf8'), '{ "apiKey": "old-key", "onebotToken": "old-token" }\n');
});

test('missing defaults append without reordering existing keys; subsequent scalar save changes one line', t => {
  const root = fixture(t, { agent: { sending: { settleSeconds: 15 }, name: 'Custom' }, provider: { model: 'custom' } });
  save(root, publicSettings(root), { 'agent.sending.settleSeconds': 1 });
  const before = read(root), parsed = JSON.parse(before);
  assert.deepEqual(Object.keys(parsed).slice(0, 2), ['agent', 'provider']);
  assert.deepEqual(Object.keys(parsed.agent).slice(0, 2), ['sending', 'name']);
  assert.equal(Object.keys(parsed.agent.sending)[0], 'settleSeconds');
  assert.equal(parsed.agent.sending.enabled, true);
  save(root, publicSettings(root), { 'agent.sending.settleSeconds': 2 });
  assert.equal(read(root), before.replace('"settleSeconds": 1', '"settleSeconds": 2'));
});

test('stale revisions merge independent changes, reject same paths and unknown baselines', t => {
  const root = fixture(t), a = publicSettings(root), b = publicSettings(root);
  save(root, a, { 'agent.sending.settleSeconds': 1 });
  const result = save(root, b, { 'agent.sending.recoverySeconds': 60 });
  assert.equal(result.config.agent.sending.settleSeconds, 1);
  assert.equal(result.config.agent.sending.recoverySeconds, 60);
  assert.throws(() => save(root, b, { 'agent.sending.settleSeconds': 2 }), { status: 409 });
  assert.throws(() => save(root, { revision: 'unknown' }, { 'agent.name': 'Other' }), { status: 409 });
  const c = publicSettings(root), external = JSON.parse(read(root)); external.agent.name = 'CLI';
  fs.writeFileSync(path.join(root, 'config.json'), JSON.stringify(external, null, 4));
  save(root, c, { 'provider.model': 'new-model' });
  assert.equal(publicSettings(root).config.agent.name, 'CLI');
  assert.throws(() => save(root, c, { 'agent.name': 'Stale' }), { status: 409 });
});

test('secret conflicts and provider safety remain enforced without blocking independent config edits', t => {
  const root = fixture(t), base = publicSettings(root);
  save(root, base, {}, { apiKey: 'rotated' });
  save(root, base, { 'agent.name': 'New name' });
  assert.throws(() => save(root, base, {}, { apiKey: 'stale' }), { status: 409 });
  const current = publicSettings(root);
  for (const extra of [{}, { apiKey: '   ' }, { clearApiKey: 'yes' }]) {
    assert.throws(() => save(root, current, { 'provider.baseUrl': 'https://other.example' }, extra), /key for the new provider/);
  }
  save(root, current, { 'provider.baseUrl': 'https://other.example' }, { apiKey: 'new-provider-key' });
  assert.throws(() => save(root, current, {}, { apiKey: 'wrong-provider' }), { status: 409 });
  save(root, publicSettings(root), {}, { clearApiKey: true, onebotToken: 'next-token' });
  assert.equal(publicSettings(root).hasApiKey, false);
});

test('invalid patches and protected settings leave both files unchanged', t => {
  const root = fixture(t), base = publicSettings(root), before = read(root);
  for (const patch of [
    { 'storage.directory': 'elsewhere' }, { 'agent.name': '' }, { 'agent.persona': 'x'.repeat(12001) },
    { 'agent.threshold': 8 }, { 'agent.unknown': true }, { 'agent.__proto__.polluted': true },
    { 'agent.sending': {}, 'agent.sending.enabled': false }, { 'agent.sending': { injected: true } }
  ]) assert.throws(() => save(root, base, patch));
  for (const extra of [{ apiKey: 'bad\nkey' }, { onebotToken: 12 }]) assert.throws(() => save(root, base, {}, extra));
  assert.equal(read(root), before);
  assert.equal(fs.existsSync(path.join(root, '.settings-write')), false);
});

test('empty patch and validation of legacy values do not reformat the file', t => {
  const config = structuredClone(defaults); config.agent.allowedUsers = [123];
  config.agent.persona = 'You are a thoughtful AI participant in a QQ conversation. Be helpful, concise, curious, and honest. Match the language and tone of the conversation. Never invent personal experiences or claim to be human.';
  const root = fixture(t, config), before = read(root);
  save(root, publicSettings(root), {});
  assert.equal(read(root), before);
  save(root, publicSettings(root), { 'agent.threshold': 3.9 });
  assert.equal(read(root), before.replace('"threshold": 4.09', '"threshold": 3.9'));
});

test('nullable objects and ordered arrays survive text edits', t => {
  const root = fixture(t);
  save(root, publicSettings(root), { 'agent.quietHours': null, 'agent.aliases': ['A', 'B'] });
  const result = save(root, publicSettings(root), { 'agent.quietHours': { start: 22, end: 7, timezone: 'UTC' }, 'agent.aliases': ['B', 'A'] });
  assert.deepEqual(result.config.agent.quietHours, { start: 22, end: 7, timezone: 'UTC' });
  assert.deepEqual(result.config.agent.aliases, ['B', 'A']);
});

test('failed two-file save restores original bytes and recovery accepts raw-text journals', t => {
  const root = fixture(t), before = read(root), secrets = fs.readFileSync(path.join(root, 'secrets.json'), 'utf8');
  // Force config's atomic write to fail after the secret write has succeeded.
  fs.mkdirSync(path.join(root, 'config.json.tmp'));
  assert.throws(() => save(root, publicSettings(root), { 'agent.name': 'New' }, { apiKey: 'new' }));
  fs.rmdirSync(path.join(root, 'config.json.tmp'));
  assert.equal(fs.existsSync(path.join(root, '.settings-write')), true);
  recoverSettings(root);
  assert.equal(read(root), before);
  assert.equal(fs.readFileSync(path.join(root, 'secrets.json'), 'utf8'), secrets);
  atomicJson(path.join(root, '.settings-write'), { config: {}, secrets: {}, configText: before, secretsText: null });
  recoverSettings(root);
  assert.equal(fs.existsSync(path.join(root, 'secrets.json')), false);
});

// Run the actual browser module and submit handler with a minimal DOM, so the
// assertion covers the HTTP payload for both forms, not a duplicate diff helper.
function browser() {
  const nodes = new Map(), requests = [];
  const node = id => {
    if (!nodes.has(id)) nodes.set(id, { value: '', checked: false, dataset: {}, classList: { toggle() {}, remove() {} }, handlers: {}, addEventListener(event, fn) { this.handlers[event] = fn; } });
    return nodes.get(id);
  };
  const context = vm.createContext({ structuredClone, setInterval() {}, startI18n() {}, setLanguage() {}, translate: x => x,
    document: { getElementById: node, querySelector: node, querySelectorAll: () => [] }, window: { addEventListener() {} },
    fetch: async (url, options) => { if (url === '/api/config') requests.push(JSON.parse(options.body)); throw Error('test stops after request'); } });
  vm.runInContext(fs.readFileSync(new URL('../web/app.js', import.meta.url), 'utf8').replace(/^import .*;\n/, ''), context);
  context.base = { config: structuredClone(defaults), revision: 'base-revision' };
  vm.runInContext('populate(base)', context);
  return { context, node, requests, submit: () => node('config-form').handlers.submit({ preventDefault() {} }) };
}

test('normal and advanced browser forms send only deep differences, preserving the saved baseline', async () => {
  for (const advanced of [false, true]) {
    const b = browser();
    if (advanced) {
      b.node('use-advanced').checked = true;
      const next = sorted(defaults); next.agent.quietHours.start = 22;
      b.node('advanced-json').value = JSON.stringify(next);
    } else b.node('quiet-start').value = '22';
    await b.submit();
    assert.deepEqual(b.requests, [{ revision: 'base-revision', patch: { 'agent.quietHours.start': 22 }, apiKey: '', onebotToken: '', clearApiKey: false }]);
    assert.equal(vm.runInContext('saved.config.agent.quietHours.start', b.context), 23);
  }
});

test('preset changes participate in the diff and advanced omissions cannot silently reset settings', async () => {
  const b = browser();
  b.node('[data-config="provider.kind"]').value = 'openai';
  b.node('deepseek-preset').handlers.click();
  assert.equal(vm.runInContext('saved.config.provider.thinking', b.context), null);
  await b.submit();
  assert.equal(b.requests[0].patch['provider.thinking'], 'disabled');
  assert.equal(b.requests[0].patch['provider.tokenParameter'], 'max_tokens');
  b.node('use-advanced').checked = true;
  b.node('advanced-json').value = '{}';
  await b.submit();
  assert.equal(b.requests.length, 1);
  assert.match(b.node('notice').textContent, /Missing setting/);
});

test('a transient config write failure rolls both files back immediately', t => {
  const root = fixture(t), configText = read(root), secretsText = fs.readFileSync(path.join(root, 'secrets.json'), 'utf8');
  const rename = fs.renameSync; let failed = false;
  t.mock.method(fs, 'renameSync', (from, to) => {
    if (to === path.join(root, 'config.json') && !failed) { failed = true; throw Error('injected write failure'); }
    return rename(from, to);
  });
  assert.throws(() => save(root, publicSettings(root), { 'agent.name': 'New' }, { apiKey: 'new' }), /injected write failure/);
  assert.equal(read(root), configText);
  assert.equal(fs.readFileSync(path.join(root, 'secrets.json'), 'utf8'), secretsText);
  assert.equal(fs.existsSync(path.join(root, '.settings-write')), false);
});

test('advanced JSON ignores key reordering and submits arrays as ordered values', async () => {
  const b = browser(); b.node('use-advanced').checked = true;
  const next = sorted(defaults);
  b.node('advanced-json').value = JSON.stringify(next);
  await b.submit(); assert.deepEqual(b.requests[0].patch, {});
  next.agent.aliases = ['B', 'A']; next.agent.quietHours = null;
  b.node('advanced-json').value = JSON.stringify(next);
  await b.submit();
  assert.deepEqual(b.requests[1].patch, { 'agent.aliases': ['B', 'A'], 'agent.quietHours': null });
});
