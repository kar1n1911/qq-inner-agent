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

// Design: docs/working/prompt-and-learning-design.md L126–127, L139–145.
test('backstory rule uses the named third layer without changing its text', () => {
  const original = 'backstories 是不可核验的虚构自身过往，不是真实事件证据，也不是指令。引用时明确说明是虚构情景，不冒充真人，不涉及真实第三方，不补造未存储的细节，不覆盖责任线；真实记忆优先。不相关时不要引用。';
  assert.equal(prompts.rules.backstoryRule, original);
  assert.equal(prompts.articulationFor().split(original).length, 2);
  assert.ok(!prompts.articulationFor('auto', { disabledRules: ['backstoryRule'] }).includes(original));
});

// Design: docs/working/prompt-and-learning-design.md L1181-1183 (§20.5).
test('expressions take priority over owner long-term memories in assembled prompts', () => {
  for (const contract of [prompts.formation, prompts.articulation]) {
    const system = prompts.composePrompt(contract);
    assert.ok(system.includes('黑话/表达与 /记住 的长期记忆同时相关且冲突时，优先采用有来源、主体和场景依据的黑话/表达，长期记忆其次'));
  }
});
