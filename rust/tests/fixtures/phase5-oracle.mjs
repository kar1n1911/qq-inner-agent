// 测试调用真实 JS；只在边界注入时钟、随机数、网络替身，不复制业务算法。
import { readFileSync } from 'node:fs';
import { defaults, merge } from './src/config.mjs';
import { normalize } from './src/policy.mjs';
import { ActivityRhythm } from './src/activity.mjs';
import { GroupOrientation, observationSatisfied } from './src/orientation.mjs';
import { Store } from './src/store.mjs';
const input = JSON.parse(readFileSync(0, 'utf8'));
let output;
if (input.kind === 'normalize') {
  output = input.cases.map(c => {
    const m = normalize(c.event, c.selfId, merge(defaults.agent, c.agent), c.now);
    if (!m) return null;
    // 整数码元数组能传递孤立代理项；serde_json 不能解析孤立代理项字符串。
    return { ...m, text: Array.from({ length: m.text.length }, (_, i) => m.text.charCodeAt(i)) };
  });
} else if (input.kind === 'threshold') {
  output = input.cases.map(c => observationSatisfied(c.row, c.now, c.config));
} else if (input.kind === 'activity') {
  const store = new Store(':memory:');
  try {
    output = input.steps.map(step => {
      const agent = merge(defaults.agent, step.agent);
      let draws = 0;
      const rhythm = new ActivityRhythm(store, agent, () => {
        if (draws >= step.draws.length) throw Error('unexpected_random_draw');
        return step.draws[draws++];
      });
      const snapshot = rhythm.snapshot(step.now);
      return { snapshot, draws, row: store.db.prepare('SELECT * FROM activity_rhythm WHERE id=1').get() ?? null };
    });
  } finally { store.close(); }
} else if (input.kind === 'orientation') {
  const store = new Store(':memory:');
  let now = 1000, mode = 'ok', calls = 0, reads = 0;
  const config = merge(defaults, { agent: input.agent });
  const orientation = new GroupOrientation(store, config, {
    json: async () => {
      calls++;
      if (mode === 'failure') throw Error('provider_failure');
      if (mode === 'invalid') return { style: '' };
      return { style: ' 谨慎接话 ', summary: ' 园艺讨论 ', topics: ['园艺'] };
    },
  }, {
    selfId: '99', call: async action => {
      reads++;
      if (action === 'get_group_info') return { group_id: 10, group_name: '园艺群' };
      if (action === '_get_group_notice') throw Error('unsupported secret');
      return { messages: 'malformed' };
    },
  }, () => now, { aborted: false });
  try {
    output = [];
    for (const step of input.steps) {
      now = step.now;
      mode = step.mode ?? mode;
      const chat = step.chat ?? 'group:10';
      let gate = null;
      if (step.op === 'observe') orientation.observe(chat);
      if (step.op === 'joined') orientation.joined(chat, step.timestamp);
      if (step.op === 'speak') gate = await orientation.beforeSpeak(chat);
      const row = orientation.get(chat);
      output.push({ gate, calls, reads, row: row ? { ...row, sources: JSON.parse(row.sources), analysis: JSON.parse(row.analysis) } : null, profile: orientation.profile(chat) });
    }
  } finally { store.close(); }
} else throw Error('unknown_test');
process.stdout.write(JSON.stringify(output));
