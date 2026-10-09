import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';
import { setTimeout as sleep } from 'node:timers/promises';
import { revision, atomicJson } from '../src/settings.mjs';

const bin = fileURLToPath(new URL('../rust/target/release/qq-inner-core', import.meta.url));

test('running Rust kernel hot-applies valid edits and retains its previous configuration on invalid edits', {
  skip: !fs.existsSync(bin) && 'kernel binary not built',
}, async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'qq-reload-test-'));
  const c = { onebot: { url: 'ws://127.0.0.1:1/', requestTimeoutSeconds: 1 }, provider: { model: 'initial' } };
  atomicJson(path.join(root, 'config.json'), c);
  const child = spawn(bin, ['start', '--root', root], { stdio: ['ignore', 'ignore', 'pipe'], env: { PATH: process.env.PATH } });
  let stderr = '', spawnError;
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', chunk => { stderr = (stderr + chunk).slice(-8192); });
  child.on('error', error => { spawnError = error; });
  const exited = new Promise(resolve => child.once('close', resolve));
  async function wait(stage, predicate) {
    for (let i = 0; i < 100; i++) {
      if (spawnError) throw spawnError;
      if (child.exitCode !== null || child.signalCode !== null) {
        throw Error(`Kernel exited during ${stage}: ${child.exitCode ?? child.signalCode}\n${stderr}`);
      }
      try { const s = JSON.parse(fs.readFileSync(path.join(root, 'data/status.json'))); if (predicate(s)) return s; } catch {}
      await sleep(100);
    }
    throw Error(`Runtime reload timed out during ${stage}\n${stderr}`);
  }
  try {
    const first = await wait('startup', s => !!s.appliedRevision);
    assert.equal(first.pid, child.pid);
    assert.equal(first.appliedRevision, revision(root));
    assert.equal(first.model, 'initial');
    c.provider.model = 'updated'; atomicJson(path.join(root, 'config.json'), c);
    const expected = revision(root);
    assert.notEqual(expected, first.appliedRevision);
    const updated = await wait('valid edit', s => !s.reloading && !s.reloadError && s.appliedRevision === expected);
    assert.equal(updated.pid, first.pid); assert.equal(updated.model, 'updated');
    c.provider.model = 'must-not-apply';
    c.agent = { threshold: 99 }; atomicJson(path.join(root, 'config.json'), c);
    const rejected = await wait('invalid edit', s => !s.reloading && !!s.reloadError);
    assert.equal(rejected.pid, first.pid);
    assert.equal(rejected.appliedRevision, expected); assert.equal(rejected.model, 'updated');
    c.provider.model = 'recovered';
    c.agent.threshold = 4.5; atomicJson(path.join(root, 'config.json'), c);
    const recoveredRevision = revision(root);
    assert.notEqual(recoveredRevision, expected);
    const recovered = await wait('recovery', s => !s.reloading && !s.reloadError && s.appliedRevision === recoveredRevision);
    assert.equal(recovered.pid, first.pid);
    assert.equal(recovered.model, 'recovered');
  } finally {
    child.kill('SIGTERM');
    const timer = setTimeout(() => child.kill('SIGKILL'), 5000);
    try { await exited; }
    finally { clearTimeout(timer); fs.rmSync(root, { recursive: true, force: true }); }
  }
});
