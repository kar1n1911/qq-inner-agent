import test from 'node:test';
import assert from 'node:assert/strict';
import { defaults, merge, validate } from '../src/config.mjs';
import { formation, evaluation, articulationFor } from '../src/prompts.mjs';
import { translate } from '../web/i18n.mjs';
import { Engine } from '../src/engine.mjs';
import { Store } from '../src/store.mjs';

test('language configuration defaults to Chinese interface and validates independent reply preferences', () => {
  assert.equal(defaults.ui.language, 'zh-CN');
  assert.equal(defaults.agent.replyLanguage, 'auto');
  const c = validate(merge(defaults, { ui: { language: 'en' }, agent: { replyLanguage: 'zh-CN', persona: '自定义角色' } }));
  assert.equal(c.agent.persona, '自定义角色');
  assert.throws(() => validate(merge(defaults, { ui: { language: 'unknown' } })));
  assert.throws(() => validate(merge(defaults, { agent: { replyLanguage: 'unknown' } })));
});
test('Chinese prompts preserve JSON contracts and apply output language explicitly', () => {
  assert.match(formation, /候选/); assert.match(formation, /TASK: FORM/);
  assert.match(evaluation, /评分/); assert.match(evaluation, /motivation/);
  assert.match(articulationFor('zh-CN'), /最终回复使用简体中文/);
  assert.match(articulationFor('en'), /最终回复使用英语/);
  assert.match(articulationFor(), /回复语言跟随当前聊天/);
  assert.throws(() => articulationFor('unknown'));
});
test('UI translator handles Chinese and English without modifying unknown model IDs', () => {
  assert.equal(translate('Save & apply', 'zh-CN'), '保存并应用');
  assert.equal(translate('Save & apply', 'en'), 'Save & apply');
  assert.equal(translate('3 active conversations', 'zh-CN'), '3 个活跃聊天');
  assert.equal(translate('deepseek-flash', 'zh-CN'), 'deepseek-flash');
});
test('engine uses selected reply language in the actual articulation request', async () => {
  for (const language of ['en', 'zh-CN', 'auto']) {
    const c = merge(defaults, { agent: { sending: { enabled: false }, allowedUsers: ['20'], quietHours: null, replyLanguage: language } });
    const store = new Store(':memory:'), prompts = [];
    const provider = { json: async (system, payload) => {
      prompts.push(system);
      if (system.includes('TASK: FORM')) return { allocation: 'self', candidates: [{ kind: 'system2', text: '打个招呼' }] };
      if (system.includes('TASK: EVALUATE')) return { ratings: payload.candidates.map(x => ({ id: x.id, motivation: 5, relevance: 5, originality: 5, for: [], against: [] })) };
      return { text: '你好' };
    } };
    const engine = new Engine(c, store, provider, { selfId: '99', connected: true, online: true, send: async () => ({ message_id: 42 }) });
    try {
      engine.ingest({ post_type: 'message', message_type: 'private', self_id: 99, user_id: 20, message_id: 1, time: Date.now() / 1000, message: '你好' });
      await engine.cycle('private:20');
      assert.equal(prompts.length, 3); assert.equal(prompts[2], articulationFor(language));
    } finally { await engine.stop(); store.close(); }
  }
});
