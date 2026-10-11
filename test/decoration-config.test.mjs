import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { defaults, merge, validate } from '../src/config.mjs';

const rust = JSON.parse(fs.readFileSync(new URL('../rust/src/defaults.json', import.meta.url), 'utf8'));
const example = JSON.parse(fs.readFileSync(new URL('../config.example.json', import.meta.url), 'utf8'));

// docs/working/prompt-and-learning-design.md §六 L75/L942：开箱即用默认开启。
test('omitted decoration flags enable group frequency learning and face-only in JS and Rust defaults', () => {
  assert.deepEqual(rust.agent.emoji, defaults.agent.emoji);
  for (const input of [{}, { agent: { emoji: {} } }, example]) {
    const config = validate(merge(defaults, input));
    assert.equal(config.agent.emoji.learnFrequency, true);
    assert.equal(config.agent.emoji.faceOnly, true);
  }
});

test('explicit decoration opt-outs and all flag combinations survive JSON round-trips', () => {
  for (const learnFrequency of [false, true]) {
    for (const faceOnly of [false, true]) {
      const flags = { learnFrequency, faceOnly };
      const config = validate(merge(defaults, { agent: { emoji: flags } }));
      const serialized = JSON.stringify(config);
      const restored = validate(merge(defaults, JSON.parse(serialized)));
      for (const [key, value] of Object.entries(flags)) {
        assert.equal(JSON.parse(serialized).agent.emoji[key], value);
        assert.equal(restored.agent.emoji[key], value);
      }
    }
  }
});
