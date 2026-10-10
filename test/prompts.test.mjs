import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import * as prompts from '../src/prompts.mjs';

test('Rust prompts match the current prompt sources and generator', () => {
  execFileSync(process.execPath, [fileURLToPath(new URL('../rust/tools/gen-prompts.mjs', import.meta.url)), '--check'], { stdio: 'pipe' });
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

// Design basis: docs/working/prompt-and-learning-design.md §14 L842-856.
test('section 14 allows harmless fiction while preserving all responsibility limits', () => {
  for (const system of Object.keys(prompts.taskRules).map(contract => prompts.composePrompt(contract))) {
    assert.doesNotMatch(system, /不(?:得)?编造亲身经历|不是编造经历的许可/);
    assert.ok(system.includes('无害的日常描写或情绪状态（如“刚看到一只猫趴在键盘上”“我今天有点困”）可以自然表达。'));
    assert.ok(system.includes('不得虚构涉及对方决策或利益的事实'));
    assert.ok(system.includes('不得转述第三方的具体言行'));
    assert.ok(system.includes('被直接问是否 AI 时不主动冒充真人'));
  }
  assert.ok(prompts.identity.includes('兴趣是选题线索。'));
  assert.ok(!prompts.identity.includes(prompts.responsibility), 'behavior rules stay outside Layer 1');
});
