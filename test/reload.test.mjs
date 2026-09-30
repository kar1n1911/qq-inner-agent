import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { setTimeout as sleep } from 'node:timers/promises';
import { defaults, merge } from '../src/config.mjs';
import { revision, atomicJson } from '../src/settings.mjs';

test('running agent hot-applies valid edits and retains its previous configuration on invalid edits', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'qq-reload-test-'));
  const src = fileURLToPath(new URL('../src', import.meta.url));
  fs.cpSync(src, path.join(root, 'src'), { recursive: true });
  const c = merge(defaults, { onebot: { url: 'ws://127.0.0.1:1/', requestTimeoutSeconds: 1 }, provider: { model: 'initial' } });
  atomicJson(path.join(root, 'config.json'), c);
  const child = spawn(process.execPath, [path.join(root, 'src/main.mjs')], { stdio: 'ignore', env: { PATH: process.env.PATH, NODE_NO_WARNINGS: '1' } });
  const exited = once(child, 'exit');
  async function wait(predicate) {
    for (let i = 0; i < 100; i++) {
      try { const s = JSON.parse(fs.readFileSync(path.join(root, 'data/status.json'))); if (predicate(s)) return s; } catch {}
      await sleep(100);
    }
    throw Error('Runtime reload timed out');
  }
  try {
    const first = await wait(s => !!s.appliedRevision);
    c.provider.model = 'updated'; atomicJson(path.join(root, 'config.json'), c);
    const expected = revision(root);
    const updated = await wait(s => s.appliedRevision === expected);
    assert.equal(updated.pid, first.pid); assert.equal(updated.model, 'updated');
    c.agent.threshold = 99; atomicJson(path.join(root, 'config.json'), c);
    const rejected = await wait(s => !!s.reloadError);
    assert.equal(rejected.appliedRevision, expected); assert.equal(rejected.model, 'updated');
    c.agent.threshold = 4.5; atomicJson(path.join(root, 'config.json'), c);
    const recovered = await wait(s => !s.reloadError && s.appliedRevision === revision(root));
    assert.equal(recovered.pid, first.pid);
  } finally {
    child.kill('SIGTERM');
    const timer = setTimeout(() => child.kill('SIGKILL'), 5000);
    await exited; clearTimeout(timer); fs.rmSync(root, { recursive: true, force: true });
  }
});
