import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import * as prompts from '../src/prompts.mjs';

const contracts = JSON.parse(readFileSync(new URL('./fixtures/prompt-contracts.json', import.meta.url)));

test('task contracts retain all pre-refactor JSON examples byte for byte', () => {
  for (const [name, examples] of Object.entries(contracts)) {
    assert.deepEqual(prompts[name].match(/\{[^\n]*\}/g), examples);
    assert.ok(prompts[name].startsWith('TASK: '));
    assert.doesNotMatch(prompts[name], /personality|persona|你是 QQ|AI 腔|优先 face/);
  }
});

test('identity occurs once and every named rule can be disabled independently', () => {
  for (const contract of Object.keys(prompts.taskRules)) {
    const full = prompts.composePrompt(contract);
    assert.equal(full.split(prompts.identity).length, 2);
    assert.ok(full.includes(prompts.outputContract));
    for (const name of prompts.taskRules[contract]) {
      const without = prompts.composePrompt(contract, { disabledRules: [name] });
      assert.ok(!without.includes(prompts.rules[name]), name);
      assert.ok(without.includes(contract));
      for (const other of prompts.taskRules[contract].filter(n => n !== name)) {
        assert.ok(without.includes(prompts.rules[other]), other);
      }
    }
  }
  assert.equal(prompts.composePrompt(prompts.formation, { ruleNames: [] }),
    [prompts.identity, prompts.outputContract, prompts.formation].join('\n'));
});

test('language is a switchable rule and responsibility wording survives', () => {
  assert.equal(prompts.articulationFor('en', { disabledRules: ['language'] }), prompts.composePrompt(prompts.articulation));
  assert.throws(() => prompts.articulationFor('fr'), { message: 'Invalid reply language' });
  for (const clue of ['涉及对方决策或利益', '第三方的具体言行', '无害的日常描写或情绪状态', '不主动冒充真人']) {
    assert.ok(prompts.rules.responsibility.includes(clue));
  }
});
