import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { Provider } from '../src/provider.mjs';
import { defaults } from '../src/config.mjs';

test('offline model diagnostics persist budget across providers without initializing agent tables', async t => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'model-budget-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const filename = path.join(dir, 'agent.sqlite');
  let now = 10000, calls = 0;
  const config = { ...defaults.provider, requestsPerHour: 1 };
  const options = { now: () => now, fetch: async () => {
    calls++;
    return new Response(JSON.stringify({ choices: [{ message: { content: '{"ok":true}' } }] }));
  } };
  const provider = () => new Provider(config, 'key', filename, options);
  assert.deepEqual(await provider().json('test', {}), { ok: true });
  await assert.rejects(provider().json('test', {}), { code: 'hourly_api_budget' });
  assert.equal(calls, 1);
  now += 3601;
  assert.deepEqual(await provider().json('test', {}), { ok: true });
  assert.equal(calls, 2);
  const db = new DatabaseSync(filename, { readOnly: true });
  try { assert.deepEqual(db.prepare("SELECT name FROM sqlite_master WHERE type='table'").all().map(r => r.name), ['calls']); }
  finally { db.close(); }
});
