import fs from 'node:fs';
import path from 'node:path';

export const defaults = {
  ui: { language: 'zh-CN' },
  provider: { kind: 'openai', baseUrl: 'https://api.openai.com/v1', model: '',
    maxTokens: 1600, tokenParameter: 'max_completion_tokens', timeoutSeconds: 60,
    retries: 2, requestsPerHour: 120, anthropicAuth: 'x-api-key', workspaceId: '', thinking: null },
  onebot: { url: 'ws://127.0.0.1:3001/', selfId: '', heartbeatSeconds: 30,
    requestTimeoutSeconds: 12, reconnectMaxSeconds: 60, forwardEnabled: false },
  telegram: { enabled: false, proxy: '', pollTimeoutSeconds: 20, requestTimeoutSeconds: 30,
    reconnectMaxSeconds: 60, allowedGroups: [], allowedUsers: [] },
  agent: { name: 'Luma', persona: '你是 QQ 聊天中善于接话、抛出话题、带动轻松交流的 AI 伙伴。先接住对方的情绪和话头，再给出一个容易接下去的回应。可以分享贴合上下文的观察、轻巧联想、适度玩笑，或一个具体且低负担的问题；不要每句话都追问，也不要把闲聊变成客服答疑或长篇讲课。话题自然结束时，可以从共同兴趣或未完的话题轻轻开启新方向，但冷场不必硬救。气氛热闹时给别人空间，有人认真求助或表达难过时先认真回应。逐渐适应每个聊天的用语、节奏和兴趣，尊重明确反馈，不把一个人的偏好当成所有人的偏好。表达自然、有温度，不编造亲身经历，不冒充真人。', replyLanguage: 'auto',
    personality: { behavior: '先听懂当前话题，再决定接话、补充、提问或安静旁观。认真求助优先，不强行热场。', replyStyle: '自然、简洁、口语化，一次接住一个重点。避免客服式开场、机械复述、连续追问和过度比喻。', interests: [], variants: [], variantProbability: 0 },
    expression: { learn: true, useLearned: true, minConfidence: 0.8, maxPerReply: 2, maxEntries: 100, retentionDays: 90, reuseSeconds: 1800 },
    emoji: { enabled: true, probability: 0.15, cooldownSeconds: 600, symbols: ['🙂','😂','🤔','👍'], faceIds: [], learnFrequency: false, faceOnly: false },
    learning: { enabled: true, minMessages: 8, intervalSeconds: 300, maxMemories: 100, memoryDays: 30, retrievalLimit: 6 },
    observation: { enabled: true, minSeconds: 300, minMessages: 20, thresholdMode: 'both', historyLimit: 30 },
    memory: { recallChars: 2400, recallHalfLifeDays: 30, minConfidence: 0.35, revisionLimit: 3, shortHours: 72, shortLimit: 40, shortChars: 1000, longChars: 1800, traitChars: 900, longDays: 365, traitDays: 180, maxPeople: 200 },
    affect: { enabled: false }, memoryRecall: false, threeLayerDecision: false, multiBubble: false,
    ownerTeaching: { enabled: false, ownerUin: '' },
    topicSource: { enabled: false, github: [], feeds: [], intervalHours: 1, maxPerHour: 1, maxRequests: 4, maxItems: 3, maxChars: 600, maxTotalChars: 1800, cacheHours: 1, threshold: 0.15 },
    relay: { enabled: false, allowHighRisk: false, ownerUin: '', threshold: 0.15, highThreshold: 0.65, maxMergedMessages: 3 },
    identity: { enabled: false, minTraits: 3, minAgeDays: 7, cooldownDays: 14, growPersona: false, allowNickname: false, allowGroupCard: false, allowAvatar: false, allowSignature: false },
    backstory: { enabled: false },
    aliases: ['Luma'], allowedGroups: [], allowedUsers: [], ignoredUsers: [],
    proactive: true, dryRun: false, threshold: 4.09, interruptThreshold: 4.8,
    sending: { enabled: true, proactiveProbability: 0.8, addressedProbability: 1,
      settleSeconds: 15, recoverySeconds: 300, burstScale: 6,
      maxNegativeProbability: 0.4, expectationSeconds: 300 },
    rhythm: { enabled: false, dayProbability: 0.85, edgeProbability: 0.65, centerProbability: 0.02, sigma: 0.22,
      activeMinSeconds: 300, activeMaxSeconds: 1200, restMinSeconds: 600, restMaxSeconds: 2400 },
    schedule: { enabled: false, activeStart: '08:00', inactiveStart: '23:00', timezone: 'Europe/Stockholm' },
    system1Probability: 0, proactiveTone: false, pauseSeconds: 45,
    debounceSeconds: 3, minThinkIntervalSeconds: 15, proactiveCooldownSeconds: 180,
    maxProactivePerHour: 6, maxMessagesPerHour: 30, activeWindowSeconds: 900,
    thoughtTtlSeconds: 1800, thoughtLimit: 12, historyLimit: 24,
    maxInputChars: 2000, maxOutputChars: 800, maxConcurrentChats: 2,
    maxActiveChats: 64, quietHours: { start: 23, end: 8, timezone: 'Europe/Stockholm' } },
  storage: { directory: 'data', retentionDays: 30, maxMessagesPerChat: 500 },
};

export function merge(base, extra) {
  const result = structuredClone(base);
  for (const [k, v] of Object.entries(extra || {})) {
    if (['__proto__', 'constructor', 'prototype'].includes(k)) continue;
    result[k] = v && typeof v === 'object' && !Array.isArray(v) && result[k] && typeof result[k] === 'object'
      ? merge(result[k], v) : v;
  }
  return result;
}
export function validate(c) {
  if (c.agent.persona === '你是 QQ 聊天中的 AI 参与者。友善、简洁、真诚，保持好奇心，结合聊天内容提供有用的回应。不要编造亲身经历，也不要冒充真人。') c.agent.persona = defaults.agent.persona;
  if (c.agent.persona === 'You are a thoughtful AI participant in a QQ conversation. Be helpful, concise, curious, and honest. Match the language and tone of the conversation. Never invent personal experiences or claim to be human.') c.agent.persona = defaults.agent.persona;
  if (!['zh-CN', 'en'].includes(c.ui.language)) throw Error('Invalid interface language');
  if (!['auto', 'zh-CN', 'en'].includes(c.agent.replyLanguage)) throw Error('Invalid reply language');
  const { personality, expression, emoji } = c.agent;
  if (!personality || !expression || !emoji) throw Error('Invalid expression settings');
  for (const key of ['behavior','replyStyle']) if (typeof personality[key] !== 'string' || personality[key].length > 2000) throw Error(`Invalid personality.${key}`);
  for (const [list,max,length] of [[personality.interests,20,80],[personality.variants,8,500],[emoji.symbols,30,24],[emoji.faceIds,30,5]]) if (!Array.isArray(list) || list.length>max || list.some(x=>typeof x!=='string'||!x.trim()||x.length>length)) throw Error('Invalid expression list');
  if (emoji.faceIds.some(x=>!/^\d{1,5}$/.test(x))) throw Error('Invalid QQ face ID');
  for (const v of [expression.learn,expression.useLearned,emoji.enabled]) if(typeof v!=='boolean') throw Error('Invalid expression switch');
  for (const v of [personality.variantProbability,expression.minConfidence,emoji.probability]) if(typeof v!=='number'||!Number.isFinite(v)||v<0||v>1) throw Error('Invalid expression probability');
  for (const [v,min,max] of [[expression.maxPerReply,1,5],[expression.maxEntries,1,500],[expression.retentionDays,1,3650],[expression.reuseSeconds,0,86400],[emoji.cooldownSeconds,0,86400]]) if(!Number.isInteger(v)||v<min||v>max) throw Error('Invalid expression limit');
  const schedule = c.agent.schedule;
  const rhythm = c.agent.rhythm;
  if (!rhythm || typeof rhythm.enabled !== 'boolean') throw Error('Invalid activity rhythm');
  for (const key of ['dayProbability', 'edgeProbability', 'centerProbability']) if (typeof rhythm[key] !== 'number' || !Number.isFinite(rhythm[key]) || rhythm[key] < 0 || rhythm[key] > 1) throw Error(`Invalid rhythm.${key}`);
  if (rhythm.centerProbability > rhythm.edgeProbability) throw Error('Rhythm center probability must not exceed edge probability');
  if (typeof rhythm.sigma !== 'number' || !Number.isFinite(rhythm.sigma) || rhythm.sigma < 0.05 || rhythm.sigma > 1) throw Error('Invalid rhythm.sigma');
  for (const kind of ['active', 'rest']) {
    for (const bound of ['Min', 'Max']) { const k = `${kind}${bound}Seconds`; if (!Number.isInteger(rhythm[k]) || rhythm[k] < 30 || rhythm[k] > 86400) throw Error(`Invalid rhythm.${k}`); }
    if (rhythm[`${kind}MinSeconds`] > rhythm[`${kind}MaxSeconds`]) throw Error(`Invalid rhythm.${kind} duration range`);
  }
  const observation = c.agent.observation;
  if (!observation || typeof observation.enabled !== 'boolean' || !['both','either'].includes(observation.thresholdMode)) throw Error('Invalid observation settings');
  for (const [key, min, max] of [['minSeconds',1,604800],['minMessages',1,10000],['historyLimit',1,100]]) if (!Number.isInteger(observation[key]) || observation[key] < min || observation[key] > max) throw Error(`Invalid observation.${key}`);
  if (!c.agent.memory || typeof c.agent.memory !== 'object') throw Error('Invalid memory settings');
  for (const [key, min, max] of [['recallChars',200,12000], ['recallHalfLifeDays',1,3650], ['revisionLimit',1,10], ['shortHours',1,720], ['shortLimit',1,200], ['shortChars',100,2000], ['longChars',200,8000], ['traitChars',100,4000], ['longDays',1,3650], ['traitDays',1,3650], ['maxPeople',1,1000]]) {
    if (!Number.isInteger(c.agent.memory[key]) || c.agent.memory[key] < min || c.agent.memory[key] > max) throw Error(`Invalid memory.${key}: expected ${min}..${max}`);
  }
  if (!c.agent.learning || typeof c.agent.learning.enabled !== 'boolean') throw Error('Invalid learning settings');
  for (const key of ['minMessages', 'intervalSeconds', 'maxMemories', 'memoryDays', 'retrievalLimit']) if (!Number.isInteger(c.agent.learning[key])) throw Error(`Invalid learning.${key}: expected integer`);
  if (!c.agent.sending || typeof c.agent.sending.enabled !== 'boolean') throw Error('Invalid sending policy');
  if (!schedule || typeof schedule.enabled !== 'boolean') throw Error('Invalid activity schedule');
  for (const k of ['activeStart', 'inactiveStart']) if (typeof schedule[k] !== 'string' || !/^(?:[01]\d|2[0-3]):[0-5]\d$/.test(schedule[k])) throw Error(`Invalid schedule.${k}: use HH:MM`);
  if (schedule.activeStart === schedule.inactiveStart) throw Error('Active and inactive start times must differ; disable the schedule for all-day activity');
  if (typeof schedule.timezone !== 'string' || !schedule.timezone.trim()) throw Error('Invalid schedule timezone');
  new Intl.DateTimeFormat('en', { timeZone: schedule.timezone }).format();
  if (!['openai', 'anthropic'].includes(c.provider.kind)) throw Error('provider.kind must be openai or anthropic');
  for (const [name, raw, schemes] of [['provider.baseUrl', c.provider.baseUrl, ['https:', 'http:']], ['onebot.url', c.onebot.url, ['ws:', 'wss:']]]) {
    const u = new URL(raw);
    if (!schemes.includes(u.protocol) || u.username || u.password || u.search || u.hash) throw Error(`${name}: invalid URL (no credentials/query/fragment)`);
    if (['http:', 'ws:'].includes(u.protocol) && !['127.0.0.1', 'localhost', '[::1]'].includes(u.hostname)) throw Error(`${name}: use TLS outside localhost`);
  }
  // Telegram 网关与 Rust config.rs 的 Telegram 段保持同一语义（群 id 可负，代理可指向 localhost）。
  if (c.telegram !== undefined) {
    if (!c.telegram || typeof c.telegram !== 'object' || Array.isArray(c.telegram)) throw Error('Invalid telegram settings');
    // 与 Rust `#[serde(default)]` 一致：缺失字段回退到默认值，旧配置可只写要改的键。
    const t = { ...defaults.telegram, ...c.telegram };
    c.telegram = t;
    if (typeof t.enabled !== 'boolean') throw Error('Invalid telegram.enabled');
    const proxy = String(t.proxy);
    if (proxy.trim()) {
      let u;
      try { u = new URL(proxy); } catch { throw Error('telegram.proxy: invalid URL (no credentials/query/fragment)'); }
      if (!['http:', 'https:'].includes(u.protocol) || !u.hostname || u.username || u.password || u.search || u.hash) throw Error('telegram.proxy: invalid URL (no credentials/query/fragment)');
    }
    for (const [key, pattern, label] of [['allowedGroups', /^-?\d{1,20}$/, 'group IDs'], ['allowedUsers', /^\d{1,20}$/, 'user IDs']]) {
      if (t[key] !== undefined && (!Array.isArray(t[key]) || t[key].some(x => !pattern.test(String(x))))) throw Error(`telegram.${key}: expected ${label}`);
      t[key] = (t[key] === undefined ? [] : t[key]).map(String);
    }
    for (const [key, min, max] of [['pollTimeoutSeconds', 1, 50], ['requestTimeoutSeconds', 1, 300], ['reconnectMaxSeconds', 1, 300]]) {
      const value = t[key];
      if (typeof value !== 'number' || !Number.isFinite(value) || value < min || value > max) throw Error(`Invalid telegram.${key}: expected ${min}..${max}`);
    }
  }
  if (!['max_tokens', 'max_completion_tokens'].includes(c.provider.tokenParameter)) throw Error('Invalid tokenParameter');
  if (!['x-api-key', 'bearer'].includes(c.provider.anthropicAuth)) throw Error('Invalid anthropicAuth');
  if (![null, 'disabled'].includes(c.provider.thinking)) throw Error('thinking must be null or disabled');
  const ranges = {
    'agent.memory.minConfidence': [0, 1],
    'agent.learning.minMessages': [1, 100], 'agent.learning.intervalSeconds': [30, 86400],
    'agent.learning.maxMemories': [1, 500], 'agent.learning.memoryDays': [1, 365],
    'agent.learning.retrievalLimit': [1, 20],
    'agent.sending.proactiveProbability': [0, 1], 'agent.sending.addressedProbability': [0, 1],
    'agent.sending.settleSeconds': [1, 3600], 'agent.sending.recoverySeconds': [1, 86400],
    'agent.sending.burstScale': [1, 100], 'agent.sending.maxNegativeProbability': [0, 1],
    'agent.sending.expectationSeconds': [1, 86400],
    'provider.maxTokens': [128, 32000], 'provider.timeoutSeconds': [1, 300],
    'provider.retries': [0, 5], 'provider.requestsPerHour': [1, 10000],
    'onebot.heartbeatSeconds': [1, 300], 'onebot.requestTimeoutSeconds': [1, 120],
    'onebot.reconnectMaxSeconds': [1, 300], 'agent.threshold': [1, 5],
    'agent.interruptThreshold': [1, 5], 'agent.system1Probability': [0, 1],
    'agent.pauseSeconds': [1, 3600], 'agent.debounceSeconds': [0, 60],
    'agent.minThinkIntervalSeconds': [1, 3600], 'agent.proactiveCooldownSeconds': [0, 86400],
    'agent.maxProactivePerHour': [0, 100], 'agent.maxMessagesPerHour': [1, 200],
    'agent.activeWindowSeconds': [1, 86400], 'agent.thoughtTtlSeconds': [1, 86400],
    'agent.thoughtLimit': [1, 30], 'agent.historyLimit': [1, 100],
    'agent.maxInputChars': [100, 10000], 'agent.maxOutputChars': [10, 4000],
    'agent.maxConcurrentChats': [1, 8], 'agent.maxActiveChats': [1, 500],
    'storage.retentionDays': [1, 3650], 'storage.maxMessagesPerChat': [25, 10000],
  };
  for (const [key, [min, max]] of Object.entries(ranges)) {
    const value = key.split('.').reduce((obj, k) => obj[k], c);
    if (typeof value !== 'number' || !Number.isFinite(value) || value < min || value > max) throw Error(`Invalid ${key}: expected ${min}..${max}`);
  }
  for (const key of ['allowedGroups', 'allowedUsers', 'ignoredUsers']) {
    if (!Array.isArray(c.agent[key]) || c.agent[key].some(x => !/^[1-9]\d*$/.test(String(x)))) throw Error(`agent.${key}: expected QQ IDs`);
    c.agent[key] = c.agent[key].map(String);
  }
  if (!Array.isArray(c.agent.aliases) || c.agent.aliases.some(x => typeof x !== 'string' || !x.trim())) throw Error('Invalid aliases');
  if (!['', ...[String(c.onebot.selfId)].filter(x => /^[1-9]\d*$/.test(x))].includes(String(c.onebot.selfId))) throw Error('Invalid selfId');
  for (const k of ['proactive', 'dryRun', 'proactiveTone']) if (typeof c.agent[k] !== 'boolean') throw Error(`Invalid ${k}`);
  if (c.agent.quietHours) {
    for (const k of ['start', 'end']) if (!Number.isInteger(c.agent.quietHours[k]) || c.agent.quietHours[k] < 0 || c.agent.quietHours[k] > 23) throw Error('Invalid quiet hours');
    new Intl.DateTimeFormat('en', { timeZone: c.agent.quietHours.timezone }).format();
  }
  return c;
}
export function loadConfig(root) {
  const read = name => fs.existsSync(path.join(root, name)) ? JSON.parse(fs.readFileSync(path.join(root, name), 'utf8')) : {};
  const c = validate(merge(defaults, read('config.json')));
  const s = read('secrets.json');
  const vendorKey = new URL(c.provider.baseUrl).hostname === 'api.deepseek.com'
    ? process.env.DEEPSEEK_API_KEY
    : process.env[c.provider.kind === 'openai' ? 'OPENAI_API_KEY' : 'ANTHROPIC_API_KEY'];
  c.apiKey = process.env.LLM_API_KEY || vendorKey || s.apiKey || '';
  c.onebotToken = process.env.ONEBOT_TOKEN || s.onebotToken || '';
  c.telegramToken = process.env.TELEGRAM_BOT_TOKEN || s.telegramToken || '';
  c.dataDir = path.resolve(root, c.storage.directory);
  return c;
}
export function readiness(c) {
  const missing = [];
  if (!c.apiKey) missing.push('API key');
  if (!c.provider.model) missing.push('model');
  // 与 Rust 的网关扁平化一致：Telegram 白名单计入已选聊天，任一侧有配置即算就绪。
  const hasChats = c.agent.allowedGroups.length || c.agent.allowedUsers.length
    || c.telegram?.allowedGroups?.length || c.telegram?.allowedUsers?.length;
  if (!hasChats) missing.push('selected chat IDs');
  return missing;
}
