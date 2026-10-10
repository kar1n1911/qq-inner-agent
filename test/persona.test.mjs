import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { defaults, validate, merge } from '../src/config.mjs';

// Design basis: docs/working/prompt-and-learning-design.md §14 L842-856.
const legacyPersonas = [
  "你是 QQ 聊天中善于接话、抛出话题、带动轻松交流的 AI 伙伴。先接住对方的情绪和话头，再给出一个容易接下去的回应。可以分享贴合上下文的观察、轻巧联想、适度玩笑，或一个具体且低负担的问题；不要每句话都追问，也不要把闲聊变成客服答疑或长篇讲课。话题自然结束时，可以从共同兴趣或未完的话题轻轻开启新方向，但冷场不必硬救。气氛热闹时给别人空间，有人认真求助或表达难过时先认真回应。逐渐适应每个聊天的用语、节奏和兴趣，尊重明确反馈，不把一个人的偏好当成所有人的偏好。表达自然、有温度，不编造亲身经历，不冒充真人。",
  "你是 QQ 聊天中的 AI 参与者。友善、简洁、真诚，保持好奇心，结合聊天内容提供有用的回应。不要编造亲身经历，也不要冒充真人。",
  "You are a thoughtful AI participant in a QQ conversation. Be helpful, concise, curious, and honest. Match the language and tone of the conversation. Never invent personal experiences or claim to be human."
];

test('default persona allows harmless fiction and retains all section 14 limits in JS and Rust', () => {
  const rust = JSON.parse(fs.readFileSync(new URL('../rust/src/defaults.json', import.meta.url), 'utf8'));
  assert.equal(rust.agent.persona, defaults.agent.persona);
  for (const persona of [defaults.agent.persona, rust.agent.persona]) {
    assert.doesNotMatch(persona, /不(?:得)?编造亲身经历/);
    assert.ok(persona.includes('允许无害的经历虚构与情绪/状态表达'));
    assert.ok(persona.includes('不得虚构涉及对方决策或利益的事实、第三方的具体事实'));
    assert.ok(persona.includes('不冒充真人，被直接问是不是 AI 时可以不正面回答，但不得声称自己是真人'));
  }
});

test('all former defaults migrate to the new persona without rewriting custom personas', () => {
  const rustSource = fs.readFileSync(new URL('../rust/src/config.rs', import.meta.url), 'utf8');
  const legacyBlock = rustSource.match(/pub const LEGACY_PERSONAS:[\s\S]*?=\[([\s\S]*?)\];/)[1];
  const rustLegacy = [...legacyBlock.matchAll(/"([^"]*)"/g)].map(match => match[1]);
  assert.deepEqual(rustLegacy, legacyPersonas, 'JS and Rust recognize all three historical defaults');
  for (const persona of legacyPersonas) {
    const config = validate(merge(defaults, { agent: { persona } }));
    assert.equal(config.agent.persona, defaults.agent.persona);
    assert.notEqual(config.agent.persona, persona);
  }
  const custom = '自定义人格，不编造亲身经历。';
  assert.equal(validate(merge(defaults, { agent: { persona: custom } })).agent.persona, custom);
});
