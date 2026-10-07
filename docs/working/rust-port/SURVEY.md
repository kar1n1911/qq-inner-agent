# qq-inner-agent → Rust 移植代码勘察报告

**勘察对象**:`/Users/coleanderson/github_repos/qq-inner-agent`,HEAD = `6978d8b` "Add layered personality scoped expression learning and optional emoji"
**勘察范围**:`src/*.mjs` 全部 19 个模块(**逐行读完**)、`web/*`(app.js / index.html / i18n.mjs 结构;CSS 与 favicon 未逐行读,仅供样式)、`scripts/*.py` 全部 3 个、`config.example.json`、`package.json`、`agent`、`examples/napcat-websocket-server.json`、`.gitignore`。`test/*` 仅清点文件名(13 个),未逐行读。
**事实来源**:代码本身。README 未作为依据。凡未能确认处均显式标注。

---

## 1. 模块清单

### 1.0 依赖图(实读 `grep` 结果)

```
main.mjs  ──> config, store, provider, onebot, engine, settings, policy
engine.mjs ─> config, policy, prompts, sending, memory, expression, activity, orientation
store.mjs  ─> memory-ranking, expression, memory          (+ node:sqlite)
memory.mjs ─> memory-ranking                              (+ node:crypto)
expression.mjs ─> memory, memory-ranking
orientation.mjs ─> prompts
activity.mjs ─> policy
policy.mjs ─> store  (只 import similarity)
dashboard.mjs ─> config, settings, provider, store, onebot, diagnostics
diagnostics.mjs ─> onebot
cli.mjs ──> config, store, provider, onebot
settings.mjs ─> config
config / provider / sending / prompts / learning / memory-ranking ─> 无内部依赖
```

无循环依赖。`store ← policy` 与 `store ← expression` 是仅有的两处「底层反向引用」:`policy` 需要 `similarity`,而 `expression` 需要 `memorySubjects`/`rankMemories`。

### 1.1 `src/config.mjs`(149 行)

职责:默认配置表、深合并、校验归一化、加载、就绪判断。

```js
export const defaults = { /* 完整默认对象,见 §4.1 */ }
export function merge(base, extra)   // 深合并;跳过 __proto__/constructor/prototype 键;数组整体替换(不逐元素合并)
export function validate(c)          // 就地校验+归一化,返回 c;违规 throw Error(msg)
export function loadConfig(root)     // -> config(附带 apiKey / onebotToken / dataDir)
export function readiness(c)         // -> ['API key','model','selected chat IDs'] 的子集
```

`loadConfig` 精确行为:
- `read(name)` = 文件存在则 `JSON.parse`,否则 `{}`(仅 `config.json`)。
- `c = validate(merge(defaults, read('config.json')))`。
- `vendorKey` = 当 `new URL(c.provider.baseUrl).hostname === 'api.deepseek.com'` 时取 `process.env.DEEPSEEK_API_KEY`,否则按 `kind` 取 `OPENAI_API_KEY` / `ANTHROPIC_API_KEY`。
- `c.apiKey = process.env.LLM_API_KEY || vendorKey || s.apiKey || ''`
- `c.onebotToken = process.env.ONEBOT_TOKEN || s.onebotToken || ''`
- `c.dataDir = path.resolve(root, c.storage.directory)`
- 注意:`loadConfig` **不拒绝未知键**(merge 会原样保留),只有 dashboard 写配置时的 `settings.knownConfig` 才拒绝未知键。

`validate` 的两条隐式迁移(按字符串全等比较):
```js
'你是 QQ 聊天中的 AI 参与者。友善、简洁、真诚，保持好奇心，结合聊天内容提供有用的回应。不要编造亲身经历，也不要冒充真人。'
'You are a thoughtful AI participant in a QQ conversation. ... claim to be human.'
```
命中其一即替换为 `defaults.agent.persona`。

### 1.2 `src/store.mjs`(158 行)

职责:SQLite 门面(建表/迁移/CRUD)、调用预算、投递追踪、旧版非分层检索;持有 `LayeredMemory` 与 `ExpressionMemory` 实例。

```js
export function terms(text)        // -> Set<string>;拉丁/数字词(长度≥2) + 汉字二元组
export function similarity(a, b)   // -> 交集大小 / sqrt(|x|·|y|),空集返回 0
export class Store { /* 见下,含 db / memory / expressions 公开字段 */ }
```

`Store` 方法签名与语义:

| 方法 | 语义 |
|---|---|
| `constructor(filename)` | `new DatabaseSync(filename)`;`PRAGMA journal_mode=WAL; busy_timeout=5000`;建全部表/索引;实例化 `LayeredMemory`、`ExpressionMemory`;若 `thoughts` 无 `subject` 列则 `ALTER TABLE ADD COLUMN subject TEXT` |
| `recoverDeliveries()` | `UPDATE deliveries SET status='uncertain' WHERE status='pending'` |
| `close()` | `db.close()` |
| `message(m) -> boolean` | `INSERT OR IGNORE INTO messages VALUES(...)`,返回 `changes` 是否为真(用于消息去重) |
| `history(chat, limit=24) -> row[]` | `SELECT * ... ORDER BY ts DESC,rowid DESC LIMIT ?` 后 `.reverse()`(返回**时间升序**,末尾是最新) |
| `retrieve(chat, query, now, opts)` | **遗留**路径:`notes` + `learned_memories` + 最近 500 条 `messages`,用 `similarity * exp(-Δt/604800)`,owner_note 加 0.15,阈值 0.12,去重后取 `limit`。src 内已无调用者,仅 `test/core.test.mjs`、`test/learning.test.mjs` 使用 |
| `learningState(chat) -> row` | `chat_learning` 行;缺省 `{style:'',sources:'[]',updated:0,last_id:'',epoch:0}` |
| `retrieveScoped(chat, sender, query, now, settings, opts)` | `notes`(layer=`owner_note`)+ `memory.short(...)`,交 `rankMemories(...,{requireMatch:true})`,再按 `settings.recallChars` 累计截断,取 `opts.limit ?? 6` |
| `learn(chat, update, now, lastId, settings, epoch, layered) -> boolean` | `BEGIN IMMEDIATE`;epoch 不匹配则 ROLLBACK 返回 `false`;`memory.apply`、`expressions.apply`、upsert `chat_learning`、处理 `forgetIds`/`memories`、按 `maxMemories` 裁剪;`COMMIT` |
| `resetLearning(chat, now, subject=null)` | 事务:`chat_learning.epoch+1`、清空 style/sources、`DELETE learned_memories`、`memory.reset`、`expressions.reset` |
| `note(chat, text, now)` | 插入 `notes` |
| `reservoir(chat, now, ttl, limit, subject=null) -> thought[]` | `used=0 AND created > now-ttl` 的候选;`subject` 非 null 时再按 subject 过滤 |
| `addThought(chat, thought, now)` | 插入 `thoughts`(含 `subject`) |
| `score(id, score)` / `use(id)` | 更新 `thoughts.score` / `thoughts.used=1` |
| `decision(chat, action, score, tags, now)` | 插入 `decisions`(`tags` JSON.stringify) |
| `callBudget(now, max) -> boolean` | 先 `DELETE FROM calls WHERE ts < now-3600`;计数 `>= max` 返回 false(且不插入);否则插入并返回 true |
| `delivery(chat, proactive, now) -> id` | 插入 `deliveries(status='pending')`,返回新 UUID |
| `finishDelivery(id, status, messageId=null)` | 更新状态与 `message_id` |
| `counts(chat, now) -> {total, proactive, last}` | 最近 3600s、`status IN ('sent','pending','uncertain')` 的聚合 |
| `markHandled(chat, id, pause=false)` | upsert `handled` |
| `handled(chat) -> row` | 读取 `handled`;**src 内无调用者(死代码)** |
| `assessment(chat, humanId) -> row` | `send_assessments` 按 `(chat,human_id)` 查 |
| `sendingTiming(chat, now, fallbackGap) -> {gap, recentHumans}` | `gap` = 距上次有效投递的秒数(无则用 `fallbackGap`);`recentHumans` = 最近 60s 人类消息数 |
| `assess(chat, humanId, now, status, details)` | 插入 `send_assessments` |
| `assessmentStatus(chat, humanId, status)` | 更新状态 |
| `expect(chat, now, seconds, forecast)` | upsert `expectations`(重置 `observation=NULL`) |
| `observe(m, now)` | 给 `observation IS NULL AND ts<=now AND expires>=now` 的 expectation 写入 `{event:'human_message', addressed:m.hint==='self', at:now}` |
| `expectation(chat, now) -> obj\|null` | 未过期则返回 `{forecast, elapsedSeconds, observation}`,无观察则 `{event:'no_message_yet'}` |
| `activeChats(since) -> string[]` | 最近有非自身消息的不同 chat |
| `prune(now, retentionDays, maxPerChat)` | 见 §3.3 |

### 1.3 `src/memory.mjs`(155 行)

```js
export function memorySubjects(chat, sender) -> string[]
// group:<id> -> ['group', 'person:<sender>'];private:<id> -> ['person:<sender>'](要求等价)
// 任一不合法 -> throw Error('invalid_memory_scope')

export function parseMemoryUpdates(value, history, chat, sender, settings) -> update[]
// 最多 4 条;subject ∈ memorySubjects;layer ∈ {long_term,traits};operation ∈ {upsert,forget}
// key 非空且 ≤64;sourceIds 长度 1..6 且每个必须命中 history 中的非自身消息
// 归属校验:subject==='group' 时 sources 的 sender 去重数 >= 2;person 时每个 source 的 sender 必须等于该 subject
// upsert 需 text(长 ≤ min(500,longChars) / min(300,traitChars))与 importance∈[0,1];keywords ≤8 个、每个 ≤32;confidence∈[0,1] 默认 .6
// 同 (subject,layer,key) 重复 -> throw('duplicate_memory_update')
// 返回 [{keywords, confidence, subject, layer, key, operation, text, importance, sources:[{id,sender,ts}]}]

export class LayeredMemory {
  constructor(db)
  put(chat, subject, layer, slot, text, sources, importance, now, expires, metadata={}, revisionLimit=3)
  capture(message, now, settings)     // 每个 subject 写一条 short_term:slot=message.id
  apply(chat, updates, now, settings) // forget -> DELETE;upsert -> put(...);末尾 enforce
  rows(chat, subject, layer, now) -> row[]
  context(chat, sender, now, settings, query='') -> [{subject, long_term:[], traits:[]}]
  bounded(rows, chars) -> row[]
  short(chat, sender, now, settings, excluded=[]) -> row[]
  enforce(chat, now, settings, subjectsToCheck=null)
  configure(now, settings)            // 对全部 chat 跑 enforce
  reset(chat, subject=null)
}
```

`put` 的去重/复活规则(逐字复刻语义):
- `old` 存在且 `layer!=='short_term'` 且新证据最大 ts 小于旧证据最大 ts → **直接 return(丢弃更旧证据)**。
- `fresh = sources 中存在未知 (sender,id)`;`old && !fresh && old.text===text` → return(同一证据重复不续期)。
- 文本变化且 `layer!=='short_term'` → 先把旧行写入 `memory_revisions`(revision 递增),再按 `revisionLimit` 裁剪。
- 证据集合并后按 ts 降序截断到 12 条。
- `updated`/`expires` 仅在 `fresh` 时刷新。

### 1.4 `src/memory-ranking.mjs`(48 行)

```js
export function tokens(text) -> string[]   // 汉字逐字滑窗二元组;其他 `[\p{L}\p{N}]+` 长度>1 则整体
export function rankMemories(rows, query, now, settings, { requireMatch = false } = {}) -> row[]
```
算法要点:BM25 形式的 `lexical`(k1=1.2,b=0.75,IDF=`log(1+(N-df+0.5)/(df+0.5))`,TF 系数 2.2);`recency = 2^(-Δt / (recallHalfLifeDays·86400))`;把 `[lexical 权重3, recency 1, importance 1, confidence 1]` 做 **RRF 融合**(`saliency += weight/(20+rank)`);owner_note 额外 +0.04;最后贪心排序,同 subject 与已选项做字符二元组 `overlap` 惩罚 `0.08·max(overlap)`。过滤条件:owner_note 放行,否则 `confidence >= minConfidence`(默认 0.35)且 `!requireMatch || lexical > 0`。

### 1.5 `src/expression.mjs`(71 行)

```js
export function parseExpressions(value, history, chat, sender) -> expr[]
// ≤4 条;subject ∈ memorySubjects;kind ∈ {jargon,expression}
// term/meaning/situation/example 均为非空字符串,term ≤40、其余 ≤160
// confidence ∈ [0,1];sourceIds 1..6 且全部命中 history
// 归属:person subject 时每个 source 的发送者必须匹配
// 证据包含:每条 source 的 text 必须包含 (jargon? term : example);且至少一条包含 example
export class ExpressionMemory {
  constructor(db)
  apply(chat, updates, now, settings)
  context(chat, sender, query, now, settings, memorySettings) -> row[]
  used(chat, rows, text, now)
  prune(now, settings)
  reset(chat, subject)
}
export function personalityContext(agent, random=Math.random) -> {identity, behavior, replyStyle, interests, variant}
export function decorationChoices(store, chat, now, settings, random=Math.random) -> {symbols, faceIds}
export function decorate(response, choices, maxCharacters) -> {text, faceId, decorated}
```
`context` 过滤:`updated > now-retentionDays·86400`、`confidence >= minConfidence`、且 `last_used=0 || last_used <= now-reuseSeconds`;还需 `sources.length >= 2` 且 group subject 需 ≥2 个不同 sender。
`decorate` 规则:emoji 必须在 `choices.symbols` 中且 `len(emoji)+1 < maxCharacters`;否则尝试 `faceId`(须在 `choices.faceIds` 中);emoji 作为 ` ${emoji}` 追加在按字符裁剪后的正文尾部。

### 1.6 `src/activity.mjs`(43 行)

```js
export function activityProbability(now, schedule, rhythm) -> number
export class ActivityRhythm {
  constructor(store, agent, random=Math.random)  // signature = JSON.stringify([agent.schedule, agent.rhythm])
  snapshot(now) -> { enabled, active, started, until, probability?, draw?, currentProbability? }
}
```
`activityProbability`:时间表关闭或处于活跃窗口 → 返回 `dayProbability`。否则把静默区间 `[inactiveStart, activeStart)`(跨午夜取模 1440)归一化出 `x∈[0,1]`;
`edge = exp(-0.5·(0.5/sigma)²)`,`gaussian = exp(-0.5·((x-0.5)/sigma)²)`,`dip = clamp((gaussian-edge)/(1-edge),0,1)`,
`p = edgeProbability - (edgeProbability-centerProbability)·dip`。
`snapshot`:行缺失、signature 变化、`now < started`、`now >= until` 任一成立时重新抽样,时长 = `min + floor(random·(max-min+1))`,写入 `activity_rhythm(id=1)`。

### 1.7 `src/orientation.mjs`(97 行)

```js
export const orientationPrompt          // ORIENT 中文提示词(边界声明 + 任务 + 输出 JSON 契约)
export function cleanOrientationSource(kind, value, groupId, config, selfId, ignored=[]) -> object
export function observationSatisfied(row, now, config) -> boolean
export class GroupOrientation {
  constructor(store, config, provider, transport, now, signal)
  ensure(chat) -> row ; get(chat) -> row ; joined(chat, timestamp) ; observe(chat) ; profile(chat) -> object|null
  async beforeSpeak(chat) -> boolean
}
```
`cleanOrientationSource` 三分支:`info`(校验 `group_name` 为字符串、`group_id` 匹配)、`notices`(取前 5 条,文本 ≤1500)、`history`(过滤非本群/非自身/被忽略用户,取末 `historyLimit` 条,文本 ≤800)。
`observationSatisfied`:`both` 时 `time && volume`,`either` 时 `time || volume`。
`beforeSpeak` 是**唯一会阻塞发言的闸门**:私人聊天或未启用观察直接 true;`status==='ready'` 直接 true;`now < retry_at` 返回 false;未采集则并发 `Promise.allSettled` 调 `get_group_info` / `_get_group_notice` / `get_group_msg_history`;满足阈值后发一次 `ORIENT` 模型请求;校验 style/summary(≤600)与 topics(≤6 项、每项 ≤60);成功写 `status='ready'`;失败写 `retry_at = now+60`。

### 1.8 `src/engine.mjs`(259 行)

```js
export class Engine {
  constructor(config, store, provider, transport, options = {})   // options: {now, log, random, expressionRandom, activityRandom}
  chats: Map<chat, state> ; running: Set<Promise> ; controller: AbortController
  lastError ; lastCycle ; activity: ActivityRhythm ; orientation: GroupOrientation
  available(now) -> boolean
  state(chat) -> state|null            // 超过 maxActiveChats 返回 null
  ingest(event) -> void
  restore() -> void
  tick() -> void
  async cycle(chat, trigger='message') -> void
  finish(state, chat, id, version, trigger, sent=false)
  async stop()
}
```
`state` 结构:`{ version, lastHuman, lastId, hint, pending, pauseDone, lastThink, busy, due }`。
**注意**:模块内**没有** `export const ...`;`criteria`(8 个评分标签集合)与 `scoreOk` 是模块级私有。

### 1.9 `src/policy.mjs`(73 行)

```js
export function allowed(chat, a) -> boolean            // group 查 allowedGroups;private 查 allowedUsers
export function quiet(now, hours) -> boolean           // hours 为 null 或 start===end 时 false;用 IANA 时区取小时
export function activeAt(now, schedule) -> boolean     // schedule 未启用恒 true
export function normalize(event, selfId, a, now) -> message|null
export function select(rated, allocation, a, turnsSilent=0, random=Math.random) -> candidate|null
export function repeated(text, history) -> boolean     // 完全相等 或 similarity > 0.88
```
`normalize` 是过滤主入口,逐条拒绝条件:非 `post_type==='message'` / `message_type` 非 group|private / 无 `user_id` / 无 `selfId` / `user_id===selfId` / `self_id` 存在且不等于 selfId / 在 `ignoredUsers` / 无目标 id / 无 `message_id` / chat 不在白名单 / `ts` 非有限 / `now-ts > activeWindowSeconds` / `ts > now+60` / 文本为空。
文本归一:`array` 段把 text 拼接,`at` 生成 ` [@qq] `,`face` 生成 ` [QQface:id] `,`reply` 生成 ` [reply] `,其他生成 ` [type] `;`string` 形式用 CQ 码正则与 `cqDecode` 解码 `&#44;/&#91;/&#93;/&amp;`;最后 `trim().slice(0, maxInputChars)`。
`addressed` = 私聊 || `atSelf` || 别名前缀(`alias:`/`alias：`/`@alias `);`hint` = `addressed?'self' : atOther?'other' : 'open'`。
`select`:`adjusted = min(5, motivation · min(1.2, 1.02^turnsSilent))`;`self` 直接取最高分;非 proactive 返回 null;否则要求 `relevance>=3 && originality>=3`,阈值取 `interruptThreshold`(other)或 `threshold`;`open` 且随机数 `< system1Probability` 时退化为 system1 候选。

### 1.10 `src/sending.mjs`(26 行)

```js
export function forecastResult(value) -> {shouldSend, outcomes:{reply,silence,negative}, responseMode, plan}
export function sendingProbability(settings, {proactive, age, gap, recentHumans, score}, forecast) -> {factors, probability, veto}
```
`forecastResult` 校验:`shouldSend` 为布尔;三个 outcome 均在 [0,1] 且和与 1 的偏差 ≤0.02;`responseMode ∈ {answer,ask,acknowledge,wait}`;`wait` 时 `shouldSend` 必须 false;`plan` 非空且 ≤400 字符。
`sendingProbability` 因子:`base`(proactive? proactiveProbability : addressedProbability)、`settle = min(1,max(0,age)/settleSeconds)`、`recovery = min(1,max(0,gap)/recoverySeconds)`、`pace = 1/(1+recentHumans/burstScale)`、`motivation = 0.25+0.75·(clamp(score,1,5)-1)/4`、`forecast = 1-negative`。**非 proactive 时后五者恒为 1**。`veto` 优先 `forecast_withhold`(shouldSend=false),其次 `forecast_risk`(negative > maxNegativeProbability);veto 时 `probability` 直接为 0。

### 1.11 `src/provider.mjs`(99 行)

```js
export class ProviderError extends Error { code }
export function endpoint(base, kind) -> string
export function parseObject(text) -> object
export async function listModels(c, key, fetcher=globalThis.fetch) -> string[]
export class Provider {
  constructor(config, key, store, options = {})   // options: {fetch, sleep, now}
  blockedUntil: number ; calls: number
  async complete(system, user, signal) -> string
  async json(system, payload, signal) -> object
}
```

### 1.12 `src/onebot.mjs`(119 行)

```js
export class OneBotError extends Error { constructor(code, uncertain=false) }
export class OneBot extends EventEmitter {
  constructor(config, token, options = {})   // options.WebSocket 可注入
  socket ; pending: Map<echo,{resolve,reject}> ; connected ; online ; selfId ; reconnects
  async start(signal)     // 无限重连循环
  async session(signal)   // 单次会话
  async call(action, params) -> data
  async send(chat, text, faceId=null) -> data
}
```
事件名只有两个:`'status'`(字符串:连接生命周期状态码)与 `'event'`(OneBot 事件对象)。

### 1.13 `src/diagnostics.mjs`(75 行)

```js
export class Diagnostics {
  constructor(load, makeBot = c => new OneBot(c.onebot, c.onebotToken), durationMs = 60000)
  async connect(onEvent = () => {}) -> { bot, close, secrets }
  async send() -> { account, messageId, text, message }
  status() -> { state, account, until, events, error } | { state:'idle', events:[] }
  async listen() -> status
  async stop(state='stopped') -> status
}
```

### 1.14 `src/settings.mjs`(61 行)

```js
export function readJson(file, fallback = {})
export function revision(root) -> string      // sha256( config.json 内容 + '\0' + secrets.json 内容 )
export function atomicJson(file, value)       // 写 <file>.tmp(mode 0600)→ chmod → rename
export function knownConfig(value, shape = defaults, prefix = '')   // 递归拒绝未知键 -> throw
export function publicSettings(root) -> { config, revision, hasApiKey, hasOnebotToken }
export function recoverSettings(root)
export function saveSettings(root, payload) -> publicSettings
```

### 1.15 `src/dashboard.mjs`(311 行)

```js
export function createDashboard({ root, settings, key, serviceControl, serviceStatus, makeBot })
  -> { handler(req,res,peer), snapshot(), close() }
export function createHttpsRedirectProxy({ upstreamPort, knownHosts = new Set(), onError = () => {} })
  -> { server, peerFor(req, fallback), close() }
export async function closeDashboardListeners({ redirectProxy, upstream, servers = [] })
```
模块尾部有 `if (process.argv[1] === fileURLToPath(import.meta.url))` 直接运行分支(见 §5.1)。

### 1.16 `src/cli.mjs`(51 行)— 无导出,脚本
`process.argv[2]` 分派:`add-memory`(写 note)与默认分支(连接 OneBot;`contacts` 列表;`check [--api]`)。

### 1.17 `src/main.mjs`(97 行)— 无导出,进程入口
### 1.18 `src/prompts.mjs`(48 行)
```js
export const boundary, formation, evaluation, articulation, forecast   // 5 个中文提示词常量
export function articulationFor(language = 'auto') -> string          // 追加语言指令;非法语言 throw
```
### 1.19 `src/learning.mjs`(13 行)— **遗留/死代码**
```js
export function parseLearning(value, history) -> { style, memories, forgetIds }
```
**`src/` 内无任何引用**;唯一使用方是 `test/learning.test.mjs`。运行时 `engine.cycle` 调用 `store.learn` 时传的是 `{style:null, memories:[], forgetIds:[]}` + `layered` 对象,因此 `learned_memories` 的新写入路径实际只由 `store.learn` 内部处理,`parseLearning` 不参与生产。移植时**不需要**实现它(除非要保留测试)。

---

## 2. 运行时主流程

### 2.1 `main.mjs` 启动序列(逐行)

1. `process.umask(0o077)`;定义 `log(event, data)`(JSON 行 → stdout + 追加 `data/agent.log`,文件 >1 MiB 时轮转为 `.log.1`)。
2. 若存在 `.settings-write` → 抛错并 `process.exit(2)`(强制先恢复配置日志);否则 `config = loadConfig(root)`;配置非法同样 exit 2。
3. `fs.mkdirSync(config.dataDir, {recursive:true, mode:0o700})`。
4. `store = new Store(path.join(config.dataDir,'agent.sqlite'))`。
5. `store.recoverDeliveries()` — 把上次遗留的 `pending` 投递标记为 `uncertain`(绝不自动重发)。
6. `provider = new Provider(config.provider, config.apiKey, store)`。
7. `bot = new OneBot(config.onebot, config.onebotToken)`。
8. `engine = new Engine(config, store, provider, bot, {log})`。
9. `abort = new AbortController()`;`appliedRevision = revision(root)`。
10. `connect()`:`bot.on('status', ...)` 打日志;`bot.on('event', e => engine.ingest(e))`;`connection = bot.start(abort.signal)`。
11. `engine.restore()` — 从最近 `activeWindowSeconds` 内有活动的 chat 恢复 `lastHuman/lastId/pauseDone`。
12. `log('started', {mode, missing, provider, model, selectedChats})`。
13. `status()` 立即执行一次并写入 `data/status.json`(tmp + rename,0600)。
14. 注册 4 个定时器:
    - `tick` = `setInterval(() => engine.tick(), 1000)`
    - `report` = `setInterval(status, 5000)`
    - `cleanup` = `setInterval(prune, 3600_000)`,其中 `prune()` 调 `store.prune(now, retentionDays, maxMessagesPerChat)` + `store.memory.configure(now, agent.memory)` + `store.expressions.prune(now, agent.expression)`
    - `watcher` = `setInterval(热重载检查, 1000)`
15. `connect()`;`await stopped`(由 SIGINT/SIGTERM/SIGHUP 触发)。
16. 收尾:`clearInterval` ×4 → `abort.abort()` → `await reloadTask` → `await engine.stop()` → `await connection` → `status()` → `store.close()` → `log('stopped')`。

### 2.2 消息入站 → 发送的完整调用链

```
[WS 帧]
OneBot.session 的 'message' 监听器
  → JSON.parse;若 data.echo 命中 pending → resolve/reject 对应 call
  → 否则若 data.post_type 存在 → connected ? emit('event', data) : early.push(data)
OneBot.emit('event')
  → main.mjs 的 bot.on('event') 回调
    → Engine.ingest(event)                      ← 唯一入口
```

`Engine.ingest(event)`(engine.mjs:33-55)顺序:

1. 若 `observation.enabled` 且 `event` 是 `notice/group_increase` 且 user_id 是自己、群在白名单:
   `orientation.joined(chat, time)`;若 epoch 变化则 `state.version++; pending=false; pauseDone=true`;`return`。
2. `if (!this.available(now)) return;` — 活跃节奏/时间表闸门。
3. `const m = normalize(event, transport.selfId, a, now)`;`if (!m) return;`
4. `const state = this.state(m.chat)`;`if (!state || !store.message(m)) return;` — **消息去重与建状态**。
5. `orientation.observe(m.chat)` — 群观察计数 +1。
6. `if (a.learning.enabled) store.memory.capture(m, now, a.memory)` — 写 short_term。
7. `store.observe(m, now)` — 补 expectation 的观察记录。
8. `state.version++; state.lastHuman = now; state.lastId = m.id;`
9. `state.hint = (state.pending && state.hint==='self') ? 'self' : m.hint`(批内保持直接点名)。
10. `state.pending = true; state.pauseDone = false; state.due = now + a.debounceSeconds;`

**`ingest` 不做任何模型调用**,只落库并标记。

`Engine.tick()`(每 1 秒)顺序:

1. `if (!available(now))` → 所有 chat `version++; pending=false; pauseDone=true`;`return`。
2. 删除 `!busy && now-lastHuman > activeWindowSeconds` 的 chat(冷却淘汰)。
3. `if (readiness(config).length || !transport.connected || !transport.online || controller.signal.aborted) return;`
4. 遍历 `chats`:
   - `running.size >= maxConcurrentChats` → break;
   - 跳过 `busy`、过期、`now < due`;
   - 跳过 `now - lastThink < minThinkIntervalSeconds && hint !== 'self'`;
   - `trigger = pending ? 'message' : (!pauseDone && now-lastHuman >= pauseSeconds) ? 'pause' : null`;无 trigger 跳过;
   - `pause` 且(`!proactive` 或 quiet)跳过;
   - `hint !== 'self'` 且(`!proactive` 或 quiet)→ 清 pending/pauseDone 后跳过;
   - `state.busy = true`;`const promise = this.cycle(chat, trigger).catch(...).finally(...)`;`running.add(promise)`。

`Engine.cycle(chat, trigger)`(engine.mjs:93-248)完整顺序:

| # | 调用 | 说明 |
|---|---|---|
| 1 | 守卫 | `!state \|\| !allowed \|\| !available` → return |
| 2 | 快照 | `version = state.version`,`id = state.lastId` |
| 3 | **`orientation.beforeSpeak(chat)`** | 可能发 1 次 ORIENT 模型请求;false 则 `due = now+5`,return |
| 4 | 失效检查 | `version` 变 / aborted / 不再 available → return |
| 5 | `activity.snapshot(now).started` | 记录块起点用于后续失效判断 |
| 6 | `store.assessment(chat, id)` | 若 `sending.enabled` 且该人类消息已评估过 → `finish()` return(**防重复**) |
| 7 | `hint` 计算 | `trigger==='pause' ? 'open' : state.hint` |
| 8 | `state.lastThink = now; this.lastCycle = now` | |
| 9 | `store.learningState(chat)` → `profile` | 取 `last_id` / `epoch` / `updated` |
| 10 | 定义 `obsolete()` | aborted / version 变 / activity.started 变 / learning epoch 变 / orientation epoch 变 |
| 11 | `store.history(chat, limit)` | `limit = learning.enabled ? max(historyLimit, learning.minMessages) : historyLimit` |
| 12 | `last` = 最后一条非自身消息 | 无则 return |
| 13 | `store.counts(chat, now)` | 小时窗口投递计数 |
| 14 | 闸门判断 | `total >= maxMessagesPerHour` 或(非点名 且(`proactive >= maxProactivePerHour` 或 `now-last < proactiveCooldownSeconds`))→ `finish()` return |
| 15 | 计算 `learnNow` | `learning.enabled && last.id !== profile.last_id && newHumans.length >= minMessages && now-profile.updated >= intervalSeconds` |
| 16 | `query` | 最近 3 条人类消息文本拼接 |
| 17 | 构造闭包 | `memoryContext()`、`retrieve()`、`chatStyle()`、`learnedExpressions()`、`initialMemory` |
| 18 | 组装 `payload` | personality / expressions / persona / name / trigger / addressedHint / groupOrientation / chatStyle / memoryContext / learning / history / memories / retainedIdeas / priorExpectation |
| 19 | **`provider.json(formation, payload, signal)`** | 第 1 次模型调用 |
| 20 | 校验 `formed` | `candidates` 必须是数组,`allocation ∈ {self,other,open}`,否则 `invalid_formation` |
| 21 | 若 `learnNow && formed.learning` | `parseMemoryUpdates` + `parseExpressions` → `store.learn(...)`;成功则刷新 payload 的 memoryContext/chatStyle/memories/expressions |
| 22 | 写候选 | 最多 3 条,`text` 截断 300,`store.addThought`(去重) |
| 23 | `store.reservoir(...)` → `candidates` | 空则 `finish()` return |
| 24 | **`provider.json(evaluation, {...}, signal)`** | 第 2 次模型调用 |
| 25 | 评分校验 | `ratings` 数组;逐条按 id 命中、去重、三项分数均 ∈[1,5];`for/against` 各取 ≤2 个合法标签;空则 `invalid_ratings` |
| 26 | `store.score(r.id, r.motivation)` | 逐条回写 |
| 27 | `allocation` | `hint==='self'\|\|'other' ? hint : formed.allocation` |
| 28 | `turnsSilent` | 最后一条自身消息之后的非自身消息数 |
| 29 | **`select(rated, allocation, a, turnsSilent)`** | 决策:选出候选或 null |
| 30 | 门禁 | `!selected` 或(非点名 且(`!proactive` 或 quiet))→ `store.decision('withhold')` + `finish()` return |
| 31 | 若 `sending.enabled` | `timing = {proactive, age, ...store.sendingTiming(...), score}` |
| 32 | **`provider.json(forecast, {...}, signal)`** | 第 3 次模型调用(可选) |
| 33 | `sendingProbability(...)` + `random()` | `admitted = !veto && draw < probability` |
| 34 | `store.assess(chat, id, now, admitted?'admitted':'withheld', {...gate, draw, timing, prediction})` | |
| 35 | 未准入 | `store.decision(gate.veto \|\| 'probability_withhold')` + `finish(..., sent=true)` return |
| 36 | `decorationChoices(store, chat, now, a.emoji, this.expressionRandom)` | |
| 37 | **`provider.json(articulationFor(replyLanguage), {...}, signal)`** | 第 4 次模型调用;失败时 `.catch` 写 `assessmentStatus('generation_failed')` |
| 38 | 校验正文 | 非空字符串且不含 `</?(think\|analysis)>`;否则写 `generation_failed` 并抛 `invalid_articulation` |
| 39 | `decorate(response, decorations, maxOutputChars)` | → `{text, faceId, decorated}` |
| 40 | 失效/超窗检查 | → `assessmentStatus('cancelled')` return |
| 41 | 离线检查 | → `assessmentStatus('cancelled')` 且抛 `qq_offline` |
| 42 | 静默/重复检查 | `(proactive && quiet) \|\| repeated(text, history)` → `cancelled` + `store.use(selected.id)` + `finish()` return |
| 43 | `dryRun` | → `assessmentStatus('dry_run')` + `decision('dry_run')` + `use` + `finish()` return |
| 44 | **`store.delivery(chat, proactive, now)`** | 先落库再发送 |
| 45 | `store.use(selected.id)`;`finish(..., sent=true)` | |
| 46 | **`transport.send(chat, text, faceId)`** | 真正发出 |
| 47 | 成功 | `finishDelivery('sent', message_id)` → `expressions.used` → 若 `decorated` 写 `decoration_usage` → `assessmentStatus('sent')` → 若有 prediction 则 `store.expect` → `store.message(自身回复)` → `store.decision('sent')` → `lastError=null` |
| 48 | 失败 | `finishDelivery(uncertain?'uncertain':'failed')` → `assessmentStatus` → `decision('delivery_uncertain'/'delivery_failed')` → 记录 `lastError`(**不重试**) |

### 2.3 并发模型(全部循环与定时器)

| 位置 | 类型 | 周期 | 作用 |
|---|---|---|---|
| `main.mjs:59` | `setInterval` | 1000 ms | `engine.tick()`(跳过 reloading/shuttingDown) |
| `main.mjs:60` | `setInterval` | 5000 ms | 写 `data/status.json` |
| `main.mjs:62` | `setInterval` | 3 600 000 ms | `prune()`(消息/记忆/表达清理) |
| `main.mjs:69` | `setInterval` | 1000 ms | 配置 revision 比对与热重载 |
| `onebot.mjs:82` | `setInterval` | `heartbeatSeconds`(默认 30 s) | `get_status` 心跳;失败置 `online=false` 并关连接 |
| `onebot.mjs:17-29` | `while` 循环 | 指数退避 `delay=min(reconnectMaxSeconds, delay*2)` + `Math.random()`;连接存活 >30 s 则 `delay` 重置为 1 | 重连 |
| `provider.mjs:56` | `for` 循环 | `retryDelay = min(30, 2^attempt)`,并可由 `Retry-After` 抬高(≤60 s) | 模型请求重试 |
| `dashboard.mjs` 前端 | `setInterval` | 2000 ms(两处) | `refresh()` 与 `refreshDebug()` |
| `activity` | 抽样 | 块时长 `activeMin..Max` / `restMin..Max` | 持久化在 SQLite |

并发上限与背压:
- `maxConcurrentChats`(默认 2)限制同时运行的 `cycle` 数,通过 `Engine.running` Set 计量。
- `maxActiveChats`(默认 64)限制 `chats` Map 规模。
- 每 chat 单飞:`state.busy`。
- `debounceSeconds`(默认 3)合并突发:新消息把 `due` 往后推。
- `minThinkIntervalSeconds`(默认 15)限制思考频率(点名例外)。
- `pauseSeconds`(默认 45)产生一次 `pause` 触发。
- `activeWindowSeconds`(默认 900)淘汰冷 chat;`cycle` 内二次校验防幽灵回复。
- 全局 `AbortController`:`engine.stop()` 时 abort,`obsolete()` 与各阶段都会检查 `signal.aborted`;`provider` 用 `AbortSignal.any([signal, timeout])`。
- 生成期间新输入抵达 → `version` 递增 → 旧 cycle 在下一个 `obsolete()` 检查点静默返回。

---

## 3. SQLite schema

数据库文件:`<dataDir>/agent.sqlite`(`dataDir` 默认 `<root>/data`)。`PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;`。
**注意**:`CREATE TABLE IF NOT EXISTS` 分散在 4 个模块里(`store.mjs` / `memory.mjs` / `expression.mjs` / `activity.mjs` / `orientation.mjs`),没有集中迁移脚本,只有两处 `ALTER TABLE ADD COLUMN`。

### 3.1 表定义(逐字)

`store.mjs` 构造函数:
```sql
messages(chat TEXT, id TEXT, sender TEXT, name TEXT, text TEXT, ts REAL, self INTEGER DEFAULT 0, PRIMARY KEY(chat,id))
thoughts(id TEXT PRIMARY KEY, chat TEXT, text TEXT, kind TEXT, created REAL, used INTEGER DEFAULT 0, score REAL DEFAULT 0)
notes(id TEXT PRIMARY KEY, chat TEXT, text TEXT, created REAL)
decisions(id TEXT PRIMARY KEY, chat TEXT, ts REAL, action TEXT, score REAL, tags TEXT)
deliveries(id TEXT PRIMARY KEY, chat TEXT, ts REAL, proactive INTEGER, status TEXT, message_id TEXT)
calls(ts REAL)
send_assessments(id TEXT PRIMARY KEY, chat TEXT, human_id TEXT, ts REAL, status TEXT, details TEXT, UNIQUE(chat,human_id))
expectations(chat TEXT PRIMARY KEY, ts REAL, expires REAL, forecast TEXT, observation TEXT)
handled(chat TEXT PRIMARY KEY, human_id TEXT, pause_done INTEGER DEFAULT 0)
chat_learning(chat TEXT PRIMARY KEY, style TEXT, sources TEXT, updated REAL, last_id TEXT, epoch INTEGER DEFAULT 0)
learned_memories(id TEXT PRIMARY KEY, chat TEXT, text TEXT, sources TEXT, created REAL, expires REAL)

CREATE INDEX learned_memories_chat ON learned_memories(chat,expires);
CREATE INDEX messages_chat_ts ON messages(chat,ts);
CREATE INDEX deliveries_chat_ts ON deliveries(chat,ts);
CREATE INDEX thoughts_chat ON thoughts(chat,created);
```
迁移:`ALTER TABLE thoughts ADD COLUMN subject TEXT`(仅当 `PRAGMA table_info(thoughts)` 无 `subject`)。

`memory.mjs`(`LayeredMemory`):
```sql
memory_layers(
  id TEXT PRIMARY KEY, chat TEXT NOT NULL, subject TEXT NOT NULL, layer TEXT NOT NULL,
  slot TEXT NOT NULL, text TEXT NOT NULL, sources TEXT NOT NULL, importance REAL,
  created REAL, updated REAL, expires REAL, revision INTEGER DEFAULT 1,
  UNIQUE(chat,subject,layer,slot))
CREATE INDEX memory_layers_scope ON memory_layers(chat,subject,layer,expires);

memory_revisions(memory_id TEXT, revision INTEGER, text TEXT, sources TEXT, updated REAL, replaced REAL,
  PRIMARY KEY(memory_id,revision))
CREATE TRIGGER memory_revision_cleanup AFTER DELETE ON memory_layers
  BEGIN DELETE FROM memory_revisions WHERE memory_id=OLD.id; END;
```
迁移:`ALTER TABLE memory_layers ADD COLUMN keywords TEXT NOT NULL DEFAULT '[]'`;`ADD COLUMN confidence REAL NOT NULL DEFAULT 0.6`。

`expression.mjs`(`ExpressionMemory`):
```sql
expressions(chat TEXT, subject TEXT, kind TEXT, term TEXT, meaning TEXT, situation TEXT, example TEXT,
  confidence REAL, sources TEXT, updated REAL, last_used REAL DEFAULT 0,
  PRIMARY KEY(chat,subject,kind,term))
decoration_usage(chat TEXT PRIMARY KEY, ts REAL)
```

`activity.mjs`(`ActivityRhythm`):
```sql
activity_rhythm(id INTEGER PRIMARY KEY CHECK(id=1), signature TEXT, started REAL, until REAL,
  active INTEGER, probability REAL, draw REAL)
```

`orientation.mjs`(`GroupOrientation`):
```sql
group_orientation(chat TEXT PRIMARY KEY, started REAL, message_count INTEGER DEFAULT 0,
  status TEXT DEFAULT 'observing', collected INTEGER DEFAULT 0, sources TEXT DEFAULT '{}',
  analysis TEXT DEFAULT '{}', retry_at REAL DEFAULT 0, error TEXT, epoch INTEGER DEFAULT 0, joined_at REAL DEFAULT 0)
```

共 **15 张表**(messages, thoughts, notes, decisions, deliveries, calls, send_assessments, expectations, handled, chat_learning, learned_memories, memory_layers, memory_revisions, expressions, decoration_usage, activity_rhythm, group_orientation —— 实为 **17 张**),4 个显式索引 + 1 个触发器。JSON 以 TEXT 存储的列:`decisions.tags`、`send_assessments.details`、`expectations.forecast`/`observation`、`chat_learning.sources`、`learned_memories.sources`、`memory_layers.sources`/`keywords`、`expressions.sources`、`group_orientation.sources`/`analysis`。

### 3.2 写入 / 读取时机

| 表 | 写 | 读 |
|---|---|---|
| `messages` | `ingest`(人类消息)、`cycle` 成功后(自身回复) | `history`(每 cycle、`restore`、`orientation`)、`activeChats`、`prune` |
| `thoughts` | `cycle` 候选落库、`score`、`use` | `reservoir`(cycle 2 次)、dashboard `snapshot` |
| `notes` | `cli add-memory` | `retrieve`、`retrieveScoped` |
| `decisions` | cycle 各出口 | dashboard `snapshot`(最近 30) |
| `deliveries` | `delivery`/`finishDelivery` | `counts`、`sendingTiming`、`prune` |
| `calls` | `callBudget`(每次模型请求前) | 同 |
| `send_assessments` | `assess`/`assessmentStatus` | `cycle` 防重、dashboard(最近 12) |
| `expectations` | `expect`、`observe` | `expectation`(组装 payload) |
| `handled` | `markHandled`(每次 `finish`) | **无读取方** |
| `chat_learning` | `learn`、`resetLearning`、`prune` | `learningState` |
| `learned_memories` | `learn`(**当前 `update.memories` 恒为空,故运行时几乎不写**)、`prune` | `retrieve`(遗留)/`prune` |
| `memory_layers` | `capture`(每条人类消息!)、`apply`、`enforce`、`reset` | `context`、`short`、dashboard |
| `memory_revisions` | `put`(仅文本变化且非 short_term) | dashboard `snapshot`(每记忆最近 10) |
| `expressions` | `apply`、`used`、`prune`、`reset` | `context`、dashboard |
| `decoration_usage` | `cycle` 成功且 `decorated` | `decorationChoices` |
| `activity_rhythm` | `snapshot`(重新抽样时 upsert id=1) | `snapshot` |
| `group_orientation` | `ensure`/`joined`/`observe`/`beforeSpeak`/`prune` | `profile`/`beforeSpeak`/dashboard |

### 3.3 保留期清理(`Store.prune`,每 3600 s 一次 + 启动时一次)

按顺序执行(参数 `now`、`retentionDays`(默认 30)、`maxPerChat`(默认 500)):
1. `DELETE FROM messages WHERE ts < now - retentionDays*86400`
2. 对每个 chat:`DELETE FROM messages WHERE chat=? AND rowid NOT IN (SELECT rowid ... ORDER BY ts DESC,rowid DESC LIMIT maxPerChat)`
3. `DELETE FROM thoughts WHERE created < now - 86400`(**固定 24 小时**,与 retentionDays 无关)
4. `DELETE FROM learned_memories WHERE expires<=now OR created < now - retentionDays*86400`
5. `DELETE FROM memory_layers WHERE expires<=now`(**注意:第 5 步先于 `memory.configure`** —— `main.mjs` 的 `prune()` 是先 `store.prune` 再 `store.memory.configure`)
6. 若存在 `group_orientation` 表:`UPDATE group_orientation SET sources=json_remove(sources,'$.history','$.notices') WHERE started < now - retentionDays*86400`(只删历史与公告,**保留 analysis/style**)
7. `UPDATE chat_learning SET style='', sources='[]' WHERE updated < now - retentionDays*86400`
8. 对 `decisions`、`deliveries`、`send_assessments`、`expectations`:`DELETE ... WHERE ts < now - retentionDays*86400`
9. `DELETE FROM handled WHERE chat NOT IN (SELECT DISTINCT chat FROM messages)`

另有独立清理:
- `calls`:在 `callBudget` 内 `DELETE FROM calls WHERE ts < now-3600`(滚动 1 小时)。
- `memory_layers`:`LayeredMemory.enforce` 会 `expires = min(expires, updated + layer 寿命)` 并删除过期行,再按 `shortLimit`/`longChars`/`traitChars` 裁剪,并按 `maxPeople` 淘汰最久未更新的 person subject。
- `expressions`:`ExpressionMemory.prune` 按 `retentionDays` 与每 chat `maxEntries` 裁剪。
- `memory_revisions`:每次 `enforce` 后按 `revisionLimit` 裁剪(逐 memory)。
- `agent.log`:超过 1 MiB 轮转为 `agent.log.1`(仅一轮,不累积)。

---

## 4. 配置系统

### 4.1 `config.json` 完整键与默认值(`defaults`,config.mjs:4-33)

`ui`
| 键 | 默认 | 校验 |
|---|---|---|
| `language` | `'zh-CN'` | ∈ {`zh-CN`,`en`} |

`provider`
| 键 | 默认 | 校验 |
|---|---|---|
| `kind` | `'openai'` | ∈ {openai, anthropic} |
| `baseUrl` | `'https://api.openai.com/v1'` | URL;scheme ∈ {http,https};禁止 userinfo/query/fragment;`http:` 仅允许 host ∈ {127.0.0.1, localhost, [::1]} |
| `model` | `''` | dashboard 保存时 ≤200 字符 |
| `maxTokens` | `1600` | [128, 32000] |
| `tokenParameter` | `'max_completion_tokens'` | ∈ {max_tokens, max_completion_tokens} |
| `timeoutSeconds` | `60` | [1, 300] |
| `retries` | `2` | [0, 5] |
| `requestsPerHour` | `120` | [1, 10000] |
| `anthropicAuth` | `'x-api-key'` | ∈ {x-api-key, bearer} |
| `workspaceId` | `''` | 无校验 |
| `thinking` | `null` | ∈ {null, 'disabled'} |

`onebot`
| 键 | 默认 | 校验 |
|---|---|---|
| `url` | `'ws://127.0.0.1:3001/'` | scheme ∈ {ws,wss};禁止 userinfo/query/fragment;`ws:` 仅 localhost |
| `selfId` | `''` | `''` 或 `/^[1-9]\d*$/` |
| `heartbeatSeconds` | `30` | [1, 300] |
| `requestTimeoutSeconds` | `12` | [1, 120] |
| `reconnectMaxSeconds` | `60` | [1, 300] |

`agent`
| 键 | 默认 | 校验 |
|---|---|---|
| `name` | `'Luma'` | 非空、≤12000(dashboard 侧) |
| `persona` | 长中文串(见下) | 同上 |
| `replyLanguage` | `'auto'` | ∈ {auto, zh-CN, en} |
| `personality.behavior` | `'先听懂当前话题，再决定接话、补充、提问或安静旁观。认真求助优先，不强行热场。'` | 字符串 ≤2000 |
| `personality.replyStyle` | `'自然、简洁、口语化，一次接住一个重点。避免客服式开场、机械复述、连续追问和过度比喻。'` | 字符串 ≤2000 |
| `personality.interests` | `[]` | ≤20 项,每项非空 ≤80 |
| `personality.variants` | `[]` | ≤8 项,每项非空 ≤500 |
| `personality.variantProbability` | `0` | [0,1] |
| `expression.learn` | `true` | boolean |
| `expression.useLearned` | `true` | boolean |
| `expression.minConfidence` | `0.8` | [0,1] |
| `expression.maxPerReply` | `2` | 整数 [1,5] |
| `expression.maxEntries` | `100` | 整数 [1,500] |
| `expression.retentionDays` | `90` | 整数 [1,3650] |
| `expression.reuseSeconds` | `1800` | 整数 [0,86400] |
| `emoji.enabled` | `true` | boolean |
| `emoji.probability` | `0.15` | [0,1] |
| `emoji.cooldownSeconds` | `600` | 整数 [0,86400] |
| `emoji.symbols` | `['🙂','😂','🤔','👍']` | ≤30 项,每项非空 ≤24 |
| `emoji.faceIds` | `[]` | ≤30 项,每项 `/^\d{1,5}$/` |
| `learning.enabled` | `true` | boolean |
| `learning.minMessages` | `8` | 整数 [1,100] |
| `learning.intervalSeconds` | `300` | 整数 [30,86400] |
| `learning.maxMemories` | `100` | 整数 [1,500] |
| `learning.memoryDays` | `30` | 整数 [1,365] |
| `learning.retrievalLimit` | `6` | 整数 [1,20] |
| `observation.enabled` | `true` | boolean |
| `observation.minSeconds` | `300` | 整数 [1,604800] |
| `observation.minMessages` | `20` | 整数 [1,10000] |
| `observation.thresholdMode` | `'both'` | ∈ {both, either} |
| `observation.historyLimit` | `30` | 整数 [1,100] |
| `memory.recallChars` | `2400` | 整数 [200,12000] |
| `memory.recallHalfLifeDays` | `30` | 整数 [1,3650] |
| `memory.minConfidence` | `0.35` | [0,1] |
| `memory.revisionLimit` | `3` | 整数 [1,10] |
| `memory.shortHours` | `72` | 整数 [1,720] |
| `memory.shortLimit` | `40` | 整数 [1,200] |
| `memory.shortChars` | `1000` | 整数 [100,2000] |
| `memory.longChars` | `1800` | 整数 [200,8000] |
| `memory.traitChars` | `900` | 整数 [100,4000] |
| `memory.longDays` | `365` | 整数 [1,3650] |
| `memory.traitDays` | `180` | 整数 [1,3650] |
| `memory.maxPeople` | `200` | 整数 [1,1000] |
| `aliases` | `['Luma']` | 数组,每项非空字符串 |
| `allowedGroups` | `[]` | 每项 `/^[1-9]\d*$/`,归一为 String |
| `allowedUsers` | `[]` | 同上 |
| `ignoredUsers` | `[]` | 同上 |
| `proactive` | `true` | boolean |
| `dryRun` | `false` | boolean |
| `threshold` | `4.09` | [1,5] |
| `interruptThreshold` | `4.8` | [1,5] |
| `sending.enabled` | `true` | boolean |
| `sending.proactiveProbability` | `0.8` | [0,1] |
| `sending.addressedProbability` | `1` | [0,1] |
| `sending.settleSeconds` | `15` | [1,3600] |
| `sending.recoverySeconds` | `300` | [1,86400] |
| `sending.burstScale` | `6` | [1,100] |
| `sending.maxNegativeProbability` | `0.4` | [0,1] |
| `sending.expectationSeconds` | `300` | [1,86400] |
| `rhythm.enabled` | `false` | boolean |
| `rhythm.dayProbability` | `0.85` | [0,1] |
| `rhythm.edgeProbability` | `0.65` | [0,1] |
| `rhythm.centerProbability` | `0.02` | [0,1],且必须 ≤ `edgeProbability` |
| `rhythm.sigma` | `0.22` | [0.05,1] |
| `rhythm.activeMinSeconds` | `300` | 整数 [30,86400] |
| `rhythm.activeMaxSeconds` | `1200` | 整数 [30,86400];Min ≤ Max |
| `rhythm.restMinSeconds` | `600` | 整数 [30,86400] |
| `rhythm.restMaxSeconds` | `2400` | 整数 [30,86400];Min ≤ Max |
| `schedule.enabled` | `false` | boolean |
| `schedule.activeStart` | `'08:00'` | `/^(?:[01]\d\|2[0-3]):[0-5]\d$/` |
| `schedule.inactiveStart` | `'23:00'` | 同上,且必须 ≠ `activeStart` |
| `schedule.timezone` | `'Europe/Stockholm'` | 非空且 `new Intl.DateTimeFormat('en',{timeZone}).format()` 不抛 |
| `system1Probability` | `0` | [0,1] |
| `proactiveTone` | `false` | boolean |
| `pauseSeconds` | `45` | [1,3600] |
| `debounceSeconds` | `3` | [0,60] |
| `minThinkIntervalSeconds` | `15` | [1,3600] |
| `proactiveCooldownSeconds` | `180` | [0,86400] |
| `maxProactivePerHour` | `6` | [0,100] |
| `maxMessagesPerHour` | `30` | [1,200] |
| `activeWindowSeconds` | `900` | [1,86400] |
| `thoughtTtlSeconds` | `1800` | [1,86400] |
| `thoughtLimit` | `12` | [1,30] |
| `historyLimit` | `24` | [1,100] |
| `maxInputChars` | `2000` | [100,10000] |
| `maxOutputChars` | `800` | [10,4000] |
| `maxConcurrentChats` | `2` | [1,8] |
| `maxActiveChats` | `64` | [1,500] |
| `quietHours` | `{start:23, end:8, timezone:'Europe/Stockholm'}` | `start`/`end` 整数 [0,23];timezone 合法;**可为 `null`**(UI 用 null 表示关闭) |

`storage`
| 键 | 默认 | 校验 |
|---|---|---|
| `directory` | `'data'` | 无显式校验;dashboard 保存时禁止变更 |
| `retentionDays` | `30` | [1,3650] |
| `maxMessagesPerChat` | `500` | [25,10000] |

`defaults.agent.persona` 完整默认值:
> `你是 QQ 聊天中善于接话、抛出话题、带动轻松交流的 AI 伙伴。先接住对方的情绪和话头，再给出一个容易接下去的回应。可以分享贴合上下文的观察、轻巧联想、适度玩笑，或一个具体且低负担的问题；不要每句话都追问，也不要把闲聊变成客服答疑或长篇讲课。话题自然结束时，可以从共同兴趣或未完的话题轻轻开启新方向，但冷场不必硬救。气氛热闹时给别人空间，有人认真求助或表达难过时先认真回应。逐渐适应每个聊天的用语、节奏和兴趣，尊重明确反馈，不把一个人的偏好当成所有人的偏好。表达自然、有温度，不编造亲身经历，不冒充真人。`

### 4.2 `secrets.json` 键

只有两个:`apiKey`(string)、`onebotToken`(string)。文件权限 0600。

### 4.3 环境变量

| 变量 | 条件 | 位置 |
|---|---|---|
| `LLM_API_KEY` | 总是优先 | `loadConfig` |
| `DEEPSEEK_API_KEY` | 仅当 `provider.baseUrl` host === `api.deepseek.com` | `loadConfig` |
| `OPENAI_API_KEY` | 当 `kind !== 'anthropic'` 且非 deepseek | `loadConfig` |
| `ANTHROPIC_API_KEY` | 当 `kind === 'anthropic'` 且非 deepseek | `loadConfig` |
| `ONEBOT_TOKEN` | 优先于 `secrets.onebotToken` | `loadConfig` |
| `AGENT_NODE` | 指定 Node 可执行文件 | `agent` 启动脚本 |
| `NODE_NO_WARNINGS=1` | 由 `agent` 与 systemd unit 设置 | `agent` / unit |

优先级:`apiKey = LLM_API_KEY || vendorKey || secrets.apiKey || ''`。

### 4.4 校验与归一化(`validate`)

顺序:persona 迁移 → `ui.language` → `agent.replyLanguage` → personality/expression/emoji 结构与范围 → `rhythm`(含 `centerProbability ≤ edgeProbability`、`sigma`、min≤max)→ `observation` → `memory` 整数区间 → `learning` 整数 → `sending.enabled` → `schedule`(格式、互异、时区有效性)→ `provider.kind` → 两处 URL 解析与协议/host 限制 → `tokenParameter` → `anthropicAuth` → `thinking` → 大表 `ranges`(逐键数值区间)→ 三个 ID 列表正则(并 `map(String)` 归一)→ `aliases` → `selfId` → 三个布尔 → `quietHours`(若存在)。
**无 range 校验的数值**:`agent.memory.shortChars` 等已在专用循环内校验;`personality.variantProbability`/`expression.minConfidence`/`emoji.probability` 在专用循环内。

### 4.5 热重载机制

**谁检查**:`main.mjs:69-93` 的 `watch` 定时器,1000 ms。

**触发条件**:`!reloading && !shuttingDown && !fs.existsSync(<root>/.settings-write) && revision(root) !== appliedRevision`。
`revision(root) = sha256( config.json 内容 + '\0' + secrets.json 内容 )` —— 因此**改 secret 也会触发重载**。

**应用步骤**(逐步):
1. `reloading = true`;`next = loadConfig(root)`。
2. `next.dataDir !== config.dataDir` → throw `data_directory_change`(被 catch)。
3. `reconnect = JSON.stringify(next.onebot) !== JSON.stringify(config.onebot) || next.onebotToken !== config.onebotToken`。
4. `await engine.stop()`(abort 当前 controller,等待所有 in-flight cycle settle)。
5. `chats = engine.chats`(快照 Map)。
6. 若 `reconnect`:`abort.abort()` → `await connection` → 新建 `AbortController` → `bot = new OneBot(next.onebot, next.onebotToken)`。
7. `config = next`;`provider = new Provider(config.provider, config.apiKey, store)`。
8. `store.memory.configure(now, config.agent.memory)`;`store.expressions.prune(now, config.agent.expression)`。
9. `engine = new Engine(config, store, provider, bot, {log})` —— **注意:ActivityRhythm 与 GroupOrientation 都会重建**(新实例、同一 `store.db`)。
10. 把旧 `chats` 中仍 `allowed` 的项拷回新 engine:`engine.chats.set(chat, {...state, busy:false, lastThink:0})`。
11. 若 `reconnect` → `connect()`。
12. `appliedRevision = nextRevision`;`reloadError = null`;`log('config_applied', {revision})`。
13. `catch`:`reloadError = 'Invalid configuration; previous settings remain active.'`;`log('config_reload_rejected')`。
14. `finally`:`reloading = false`;`status()`。

**关键点**:`Engine` 是整体替换的,没有增量 apply;`chats` 状态靠手工迁移;**配置无效时整个 reload 回滚,旧 config/engine 继续运行**。`.settings-write` 的存在会**暂停**重载(dashboard 保存期间的两文件写入窗口)。

---

## 5. 仪表盘 API

### 5.1 进程与监听

`src/dashboard.mjs` 直接运行时(`./agent dashboard` 或 `qq-inner-dashboard.service`):
1. `process.umask(0o077)`;`root` = 仓库根。
2. `recoverSettings(root)`(回滚中断的保存)。
3. `settings = readJson(<root>/dashboard.json)`。
4. 访问密钥文件 `<root>/data/dashboard-access.txt`(0600),不存在则 `randomBytes(24).toString('base64url')` 生成。
5. `createDashboard({root, settings, key})` —— **`serviceControl`/`serviceStatus`/`makeBot` 未传入,走默认实现**(即 `systemctl --user <action> qq-inner-agent.service`)。
6. TLS 模式(`settings.tls` 存在):`https.createServer` 监听 `127.0.0.1:0`(临时端口),再由 `createHttpsRedirectProxy` 的 `net` 服务监听 `settings.port`/`settings.host`;首字节 `0x16` 视为 TLS 直通,否则交给 HTTP 解析器返回 301 到 `https://<同host><同path>`。
7. 非 TLS 模式:`settings.host` 必须是 `127.0.0.1`/`::1`,否则抛错。
8. `settings.localPort` 存在时再开一个 `127.0.0.1` HTTP 监听(默认 5097)。
9. 所有 server 设置 `requestTimeout=20000`、`headersTimeout=10000`、`maxHeadersCount=50`。
10. SIGTERM/SIGINT → `dashboard.close()` + `closeDashboardListeners(...)`(幂等,`stopping` 标志)。

`dashboard.json` 的结构(由 `scripts/install_dashboard.py` 生成):
```json
{ "host":"0.0.0.0", "port":5098, "localPort":5097,
  "origins":["https://<ip>:5098", ..., "https://localhost:5098", "http://localhost:5097", "http://127.0.0.1:5097"],
  "tls":{"key":"data/dashboard-server.key","cert":"data/dashboard-server.crt"} }
```

### 5.2 全局响应头与前置检查(`handler`)

所有响应都设置:
`Cache-Control: no-store`、`X-Content-Type-Options: nosniff`、`Referrer-Policy: no-referrer`、`X-Frame-Options: DENY`、
`Content-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'`。

1. `if (!hosts.has(req.headers.host)) throw 403`(`hosts` 来自 `settings.origins` 的 host 部分)。
2. 静态资源(GET,免鉴权,但已过 host 检查):`/` → `index.html`;`/app.js`、`/i18n.mjs`、`/style.css`、`/language.css`、`/favicon.svg`。文件从 `<root>/web/` 读取。
3. 非 GET 请求:`origin` 必须存在、在 `settings.origins` 中,且 `new URL(origin).host === req.headers.host`,否则 403 `Origin rejected`。
4. 会话:Cookie `qia_session`(HttpOnly; SameSite=Strict; Path=/; Max-Age=43200;TLS 时加 Secure)。会话存在内存 Map,过期 12 小时。有效期检查失败 → 401 `Sign in required`。
5. 非 GET 请求还必须带 `x-csrf-token` 且等于会话 csrf,否则 403 `Session verification failed; sign in again.`。
6. 请求体读取:`Content-Type` 必须以 `application/json` 开头(否则 415);上限 100 000 字节(否则 413);解析失败 400。
7. 任何抛错:`json(res, e.status || 500, {error: e.status ? e.message : 'Dashboard operation failed'})`。

### 5.3 全部路由

| 方法 | 路径 | 鉴权 | 请求体 | 响应体 |
|---|---|---|---|---|
| GET | `/`, `/app.js`, `/i18n.mjs`, `/style.css`, `/language.css`, `/favicon.svg` | 无(仅 host 检查) | — | 静态文件 |
| POST | `/api/login` | 无 | `{key:string}` | `{csrf:string}`;失败 401 `Incorrect access key`;限流 429 `Too many attempts. Try again in ten minutes.` |
| GET | `/api/session` | Cookie | — | `{csrf:string}` |
| POST | `/api/models` | Cookie+CSRF | `{}` | `{models:string[]}` 或 502 `{error}` |
| GET | `/api/debug/receive` | Cookie | — | `{state, account, until, events[], error}` |
| POST | `/api/debug/send` | Cookie+CSRF | `{}` | `{account, messageId, text, message}` 或 502 `{error, hint}` |
| POST | `/api/debug/receive` | Cookie+CSRF | `{}` | 同 `status()` |
| POST | `/api/debug/stop` | Cookie+CSRF | `{}` | 同 `status()` |
| POST | `/api/logout` | Cookie+CSRF | `{}` | `{ok:true}` + 清 Cookie |
| GET | `/api/config` | Cookie | — | `{config, revision, hasApiKey, hasOnebotToken}` |
| PUT | `/api/config` | Cookie+CSRF | `{revision, config, apiKey?, onebotToken?, clearApiKey?}` | 同 GET;409 `A save is already in progress` / `Settings changed elsewhere...` |
| GET | `/api/state` | Cookie | — | `{...snapshot(), serviceState}` |
| POST | `/api/learning/reset` | Cookie+CSRF | `{chat, subject?}` | `{ok:true}`;400 `Invalid chat` / `Invalid memory subject` |
| POST | `/api/service` | Cookie+CSRF | `{action:'start'\|'stop'\|'restart'}` | `{serviceState}`;400 `Unknown service action` |
| POST | `/api/test-model` | Cookie+CSRF | `{}` | `{ok:true, message}`;400 `Save an API key and model first`;409 并发;502 |
| GET | `/api/contacts` | Cookie | — | `{groups:[{id,name}], friends:[{id,name}]}`;503 超时 |
| * | 其他 | — | — | 404 `Not found` |

登录限流细节:按 `clientAddress`(= 代理映射后的 `peer.address`,否则 `req.socket.remoteAddress`),10 分钟窗口内最多 8 次失败;`attempts.size > 1000` 也会 429;成功后清除该 IP 记录。会话上限 20,超出时删除最早插入的一条。

### 5.4 `snapshot()` 的完整响应字段

```js
{
  status,        // data/status.json 的内容(见下),不存在则 null
  decisions,     // SELECT chat,ts,action,score,tags FROM decisions ORDER BY ts DESC LIMIT 30
  thoughts,      // SELECT chat,text,kind,score,created FROM thoughts WHERE used=0 AND created>now-thoughtTtlSeconds DESC LIMIT 12
  assessments,   // SELECT chat,ts,status,details FROM send_assessments DESC LIMIT 12;details 已 JSON.parse
  learning,      // [...new Set(memories.map(m=>m.chat))].map(chat=>({chat}))  —— 仅 chat 列表
  memories,      // SELECT * FROM memory_layers WHERE expires>now ORDER BY layer(long_term<traits<其他),updated DESC LIMIT 200
                 //   每条附带 revisions: SELECT revision,text,sources,updated,replaced FROM memory_revisions WHERE memory_id=? DESC LIMIT 10
  expressions,   // SELECT * FROM expressions WHERE updated>now-retentionDays*86400 ORDER BY updated DESC LIMIT 200
  observations,  // SELECT chat,started,message_count,status,sources,analysis,retry_at,error FROM group_orientation ORDER BY started DESC LIMIT 100
                 //   sources/analysis 已 JSON.parse
  logs,          // tail(data/agent.log, 100) —— 读文件末尾 64 KiB,按行 JSON.parse,取最后 100 条
  savedRevision  // publicSettings(root).revision
}
```
最后把 `[c.apiKey, c.onebotToken, key]` 三个非空字符串在序列化文本中整体替换为 `[redacted]`。

`data/status.json`(由 `main.mjs` 每 5 秒写,原子替换,0600):
```js
{ updatedAt, pid, mode, appliedRevision, reloading, reloadError,
  scheduleActive, activityRhythm, missing, onebotConnected, qqOnline, selfId, reconnects,
  activeChats, model, provider, apiCallsThisRun, lastCycleAt, lastError }
```
其中 `mode ∈ {waiting_for_setup, dry_run, active}`。

### 5.5 读取的文件 / 数据库

| 资源 | 用途 |
|---|---|
| `<root>/config.json`、`<root>/secrets.json` | `loadConfig` / `publicSettings` / `revision` |
| `<root>/dashboard.json` | 监听地址、origins、TLS 路径 |
| `<root>/data/dashboard-access.txt` | 登录密钥 |
| `<root>/data/status.json` | agent 心跳 |
| `<root>/data/agent.sqlite` | `readOnly:true` 的 `DatabaseSync`,只读快照;例外:`/api/learning/reset` 与 `/api/test-model` 会以**读写**方式打开 `Store` |
| `<root>/data/agent.log` | 日志尾 |
| `<root>/web/*` | 静态资源 |

### 5.6 如何控制 agent / 如何同步状态

- 控制:`serviceControl(action)` 默认执行 `systemctl --user <action> qq-inner-agent.service`(`timeout:30000`);`serviceStatus()` 执行 `systemctl --user show qq-inner-agent.service -p ActiveState --value`(`timeout:2000`,失败返回 `'unknown'`)。
- 状态同步:**单向文件轮询**。dashboard 不与该进程通信,只读 `data/status.json` 与 SQLite;前端每 2 秒 `GET /api/state`。
- 「已应用」判定:前端比较 `status.appliedRevision === state.savedRevision && !status.reloading`;`appliedRevision` 由主进程在每次成功 reload 后写入。
- 配置写入:PUT `/api/config` 由 dashboard 进程直接改 `config.json`/`secrets.json`(带 `.settings-write` journal);主进程的 watcher 在 1 秒内发现 revision 变化并热重载。

### 5.7 前端调用的接口(`web/app.js`)

| 接口 | 调用点 |
|---|---|
| `GET /api/session` | `boot()` |
| `GET /api/config` | `boot()`、`discard` 按钮 |
| `PUT /api/config` | `config-form` submit(带 `revision`/`apiKey`/`onebotToken`/`clearApiKey`) |
| `GET /api/state` | `refresh()`,每 2000 ms |
| `POST /api/login` | 登录表单 |
| `POST /api/logout` | `#logout` |
| `POST /api/service` | `[data-service]` 按钮(start/restart/stop) |
| `POST /api/test-model` | `#test-model` |
| `GET /api/contacts` | `#load-contacts` |
| `POST /api/models` | `#load-models` |
| `POST /api/debug/send` | `#debug-send` |
| `GET /api/debug/receive` | `refreshDebug()`,每 2000 ms |
| `POST /api/debug/receive` | `#debug-receive` |
| `POST /api/debug/stop` | `#debug-stop` |
| `POST /api/learning/reset` | 学习列表/表达列表里的重置按钮 |

前端不使用 WebSocket、SSE 或长轮询;全部为 `fetch` + 轮询。`web/i18n.mjs` 通过 `MutationObserver` 就地翻译 DOM 文本与 `placeholder`/`aria-label`,翻译表是 `英文|中文` 行(模板字符串,约 380 行),`translate()` 对 CJK 中文界面返回原文。

---

## 6. OneBot 层

### 6.1 连接与鉴权

- 传输:**正向 WebSocket**,由本程序作为客户端发起(`new WebSocket(url)`)。
- 鉴权:token 通过 **URL query** 附加 —— `url.searchParams.set('access_token', token)`(onebot.mjs:33)。token 为空则不附加。
- 打开超时:`requestTimeoutSeconds * 1000`(默认 12 s),超时 `connect_timeout`,关闭则 `connect_closed`。
- 注入点:`options.WebSocket`(测试可替换),默认 `globalThis.WebSocket`。

### 6.2 握手与就绪判定(`session`)

1. 建 `pending: Map`;注册 `close` / `error` / `message` 监听。
2. `open` 后调用 `get_login_info`。
   - `login.user_id` 缺失 → `missing_account`。
   - `config.selfId` 非空且不等于返回的 `user_id` → `wrong_qq_account`。
   - 否则 `this.selfId = String(login.user_id)`。
3. 调用 `get_status` → `this.online = status?.online === true`。
4. `connected = true`;`emit('status', online ? 'connected' : 'qq_offline')`。
5. **补发早到事件**:`connected` 之前到达的 `post_type` 事件被缓存(上限 200 条,`early` 数组),在置位 `connected` 后按序 `emit('event')`。
6. 启动心跳定时器(见下)。
7. `await closed`(socket 关闭的 Promise)。

### 6.3 心跳

`setInterval(..., heartbeatSeconds * 1000)`,默认 30 s,用 `checking` 布尔防止重入:
- 成功:`this.online = s?.online === true`;
- 失败:`this.online = false` 并 `ws.close()`(从而触发重连)。

### 6.4 请求/响应关联

```js
{ action: string, params: object, echo: string /* randomUUID */ }
```
回包判定:`data.echo != null && pending.has(String(data.echo))` → 取 `data.status === 'ok' && Number(data.retcode) === 0` 视为成功,`resolve(data.data)`;否则 `reject(new OneBotError('onebot_action_failed_' + (retcode||'unknown')))`。
超时:`action_timeout`(`uncertain = true`);`ws.send` 抛错:`send_failed`(uncertain)。
每条消息上限 `1_000_000` 字符,非字符串或解析失败直接忽略。
未匹配 echo 且有 `post_type` 的帧才是事件。
**注意**:`error` 事件监听器**只** `emit('status','websocket_error')`,**不**打印错误对象(注释说明 URL 带 token,且从握手错误回调里调 `close()` 会递归派发)。

### 6.5 事件类型处理

代码**不做**按 `post_type` 的白名单分发,而是把**所有**带 `post_type` 的帧透传给上层;真正的分类在 `policy.normalize`(只接受 `message` + `message_type ∈ {group,private}`)与 `engine.ingest`(额外识别 `notice`/`group_increase`)。
`diagnostics.listen` 额外过滤 `post_type ∈ {message, message_sent}`,并要求 `self_id` 与 `user_id` 都等于本账号。
事件同时走 `protocol` 层的 `get_status` 心跳,不产生事件。

### 6.6 重连策略

```js
let delay = 1;
while (!signal.aborted) {
  const started = Date.now();
  try { await this.session(signal); } catch (e) { if (!signal.aborted) emit('status', e.code || 'connection_failed'); }
  // 清理:pending 全部 reject('connection_lost', uncertain=true);socket.close();socket=null
  this.reconnects++;
  if (Date.now() - started > 30_000) delay = 1;        // 连接存活 >30s 视为稳定,重置退避
  await sleep((delay + Math.random()) * 1000);          // 抖动
  delay = Math.min(this.config.reconnectMaxSeconds, delay * 2);
}
```
退避序列(默认 `reconnectMaxSeconds=60`):1,2,4,8,16,32,60,60,… 每次加 `[0,1)` 秒随机抖动。连接生命周期日志状态串:`connected`、`qq_offline`、`websocket_error`、`connect_timeout`、`connect_closed`、`missing_account`、`wrong_qq_account`、`connection_lost`、`connection_failed`。

### 6.7 发送格式

```js
await this.call(
  type === 'group' ? 'send_group_msg' : 'send_private_msg',
  { [type === 'group' ? 'group_id' : 'user_id']: Number(id),
    message: [ { type:'text', data:{ text } },
               ...(faceId === null ? [] : [ { type:'face', data:{ id: faceId } } ]) ],
    auto_escape: true })
```
约束:必须 `connected && online`,否则 `qq_offline`;chat 必须匹配 `^(group|private):[1-9]\d*$`,否则 `invalid_chat`;`faceId` 必须 `null` 或 `/^\d{1,5}$/`,否则 `invalid_face`。注释明确说明:使用数组文本段是为了让模型写出的 CQ 码变成惰性文本(`auto_escape: true` 双重保险)。

---

## 7. provider 层

### 7.1 端点解析

```js
endpoint(base, kind):
  suffix = kind === 'anthropic' ? '/messages' : '/chat/completions'
  b = base.replace(/\/+$/, '')
  if (b.endsWith(suffix)) return b
  return b + (kind === 'anthropic' && !b.endsWith('/v1') ? '/v1' : '') + suffix
```
`listModels` 的 URL:`api.deepseek.com` → 固定 `https://api.deepseek.com/models`;否则把 `endpoint()` 结果的 `/(chat\/completions|messages)$/` 换成 `/models`。

### 7.2 请求构造(`Provider.complete`)

公共:先查 `now < blockedUntil` → `provider_backoff`。
每次尝试前调 `store.callBudget(now, requestsPerHour)`,失败 → `hourly_api_budget`(`this.calls++` 在预算通过后自增)。

| | OpenAI 兼容 (`kind='openai'`) | Anthropic 兼容 (`kind='anthropic'`) |
|---|---|---|
| 鉴权头 | `Authorization: Bearer <key>` | `x-api-key: <key>` 或 `Authorization: Bearer <key>`(由 `anthropicAuth` 选择) |
| 版本头 | — | `anthropic-version: 2023-06-01` |
| workspace | — | `anthropic-workspace-id`(若 `workspaceId` 非空) |
| body | `{model, [tokenParameter]: maxTokens, messages:[{role:'system',content:system},{role:'user',content:user}]}` | `{model, max_tokens: maxTokens, system, messages:[{role:'user',content:user}]}` |
| 思考 | 若 `thinking === 'disabled'` → 追加 `thinking: {type:'disabled'}` | 同左 |

请求选项:`method:'POST'`、`redirect:'error'`、`signal: AbortSignal.any([callerSignal, AbortSignal.timeout(timeoutSeconds*1000)])`(无 caller signal 时只用 timeout)。

### 7.3 重试 / 超时 / 限流

- `for (attempt = 0; attempt <= retries; attempt++)`。
- `retryDelay = min(30, 2 ** attempt)`;若响应带 `Retry-After`,取 `max(retryDelay, min(60, parsed))`。
- HTTP 错误分类:
  - `[401, 403, 400, 404, 422]` → `blockedUntil = now + 300`(**退避 5 分钟**),抛 `http_<code>_check_provider_config`(不重试)。
  - 非 429 且 < 500 → `http_<code>`(不重试)。
  - 429 或 ≥ 500 → `transient_http`(重试)。
- 捕获阶段:`signal.aborted` 直接抛 `signal.reason`;非 `transient_http` 的 `ProviderError` 立即抛;用尽重试 → `blockedUntil = now + 60`,抛 `provider_unavailable`。
- `response.text()` 长度 > 1 000 000 → `response_too_large`;JSON 解析失败 → `invalid_provider_response`。

### 7.4 响应解析

- Anthropic:`data.stop_reason === 'max_tokens'` → `output_truncated_increase_maxTokens`;正文 = `data.content` 中 `type==='text'` 的 `text` 以 `\n` 连接。
- OpenAI:`data.choices[0].finish_reason === 'length'` → 同上;正文 = `data.choices[0].message.content`。
- 正文非字符串或空白 → `empty_model_response`。
- `Provider.json()` = `parseObject(await complete(system, JSON.stringify(payload)))`;`parseObject` 先剥 `` ```json `` / `` ``` `` 围栏,再 `JSON.parse`,非对象(`null`/数组/非 object)抛 `invalid_json_object`,解析失败抛 `invalid_json`。

`listModels` 的独立错误码:`save_api_key_first`、`models_http_<status>`、`invalid_model_list`;结果去重、`id` 长度 ≤200、取前 500 并排序;超时 15 s。

---

## 8. 决策与记忆逻辑

### 8.1 数据流总览

```
OneBot event
  └─ policy.normalize            → message {chat,id,sender,name,text,ts,self:false,hint}
       └─ store.message          (去重落库)
       └─ memory.capture         → memory_layers(short_term)
       └─ orientation.observe    → group_orientation.message_count
       └─ store.observe          → expectations.observation
  ── tick(1s) ──> policy.select 之前的所有步骤见 §2.2
       └─ orientation.beforeSpeak  → ORIENT(可选)
       └─ memory.context + store.retrieveScoped + expressions.context + reservoir   → payload
       └─ provider.json(formation)   → candidates / allocation / learning
            └─ memory.parseMemoryUpdates + expression.parseExpressions → store.learn
            └─ store.addThought
       └─ provider.json(evaluation)  → ratings
            └─ store.score
       └─ policy.select              → selected | null
       └─ provider.json(forecast)    → prediction(可选)
            └─ sending.sendingProbability + random → admitted?
            └─ store.assess
       └─ expression.decorationChoices
       └─ provider.json(articulationFor) → {text, emoji, faceId}
            └─ expression.decorate
       └─ store.delivery → transport.send → store.finishDelivery / store.message / store.decision / store.expect
```

### 8.2 `engine.mjs` 算法要点

- **闸门顺序**(任一不满足即静默):活跃节奏 → 观察期 → 每小时总量 → 主动配额/冷却 → 动机阈值 → 发送策略(预测+概率) → 静默时段 → 重复检测 → 送达前二次校验。
- **版本化失效**:`state.version` 在每次入站消息与配置重载时递增;`cycle` 在 5 个检查点用 `obsolete()` 判定是否放弃(aborted / version / activity 块 / learning epoch / orientation epoch)。
- **单一职责的 `finish`**:统一清 `pending`、重置 `hint='open'`、必要时置 `pauseDone`、写 `handled` 表。
- **先落库后发送**:`store.delivery` 在 `transport.send` 之前,保证「不确定送达」可被 `recoverDeliveries` 标记为 `uncertain` 且永不自动重发。

### 8.3 `policy.mjs` 算法要点

- `select` 的两级阈值:`interruptThreshold`(话题指向他人)高于 `threshold`(开放轮次);`relevance>=3 && originality>=3` 是硬前提;`system1Probability` 只对 `open` 生效且只退化为 `system1` 候选。
- `adjusted` 对 `turnsSilent` 有 `1.02^n` 上限 1.2 的加成(鼓励长时间沉默后开口)。
- `repeated` 用 `similarity > 0.88` 或全等,防止复读。

### 8.4 `sending.mjs` 算法要点

概率是 6 因子乘积,任一因子为零即零;`veto` 有两类且可覆盖概率(置 0)。非主动(点名)回复把后 5 个因子全部置 1,只保留 `addressedProbability`。预测只在 `sending.enabled` 时请求;`timing.age` 是「距最近人类消息」(settling),`timing.gap` 是「距上次被策略计入的投递」(recovery)。

### 8.5 `activity.mjs` 算法要点

高斯形**静默**曲线(非归一化密度);`signature` 绑定 `[schedule, rhythm]` 结构,任一变化即重新抽样;`now < started` 也重抽(时钟回拨保护);块的 `active` 布尔决定取 `activeMin/Max` 还是 `restMin/Max`;端到端只读写 `activity_rhythm(id=1)` 单行。

### 8.6 `orientation.mjs` 算法要点

状态机:`observing` →(采集 3 个来源)→(阈值满足)→(ORIENT 成功)→ `ready`;失败进入 `retry_at = now+60` 的退避。`epoch` 在重新入群时递增,使所有在途分析失效。采集用 `Promise.allSettled`,任一来源失败只记为 `unavailable`。

### 8.7 `memory.mjs` / `memory-ranking.mjs` 算法要点

- 三层:`short_term`(逐条原话,72 h,40 条)、`long_term`(话题键+重要性,365 d,1800 字符/24 项)、`traits`(900 字符/24 项,180 d)。
- `capture` 对 group 场景写两条(群视图 + person 视图),内容是同一句带署名原话;`short()` 反向遍历 subject 使 person 优先于 group。
- `put` 的「不复活」规则:重复同一证据不刷新 `updated`;更旧证据不覆盖;文本变更先归档 revision。
- `rankMemories` 的 RRF 融合与去冗余惩罚(见 §1.4);`requireMatch` 在 `retrieveScoped` 中为 true,在 `context` 中为 false。

### 8.8 `expression.mjs` 算法要点

- 只学 `jargon`(词)与 `expression`(表达法),必须提供 `example` 且 `example` 必须**字面出现**在每条证据消息中(强证据校验)。
- 使用时需 `sources.length >= 2`,group subject 还需 ≥2 个不同 sender。
- `reuseSeconds` 冷却避免连续复用同一梗。
- 装饰:`decorationChoices` 三重闸门(总开关、每 chat 冷却、概率抽样);`decorate` 只允许白名单内的 emoji/faceId,且 emoji 占一个字符预算。

### 8.9 `prompts.mjs` 算法要点

5 段中文提示词(共享 `boundary` 前置声明:引用数据不可信、禁止泄露其他聊天记忆、只输出 JSON、不输出思维链)。任务标识固定为 `FORM` / `EVALUATE` / `ARTICULATE` / `FORECAST` / `ORIENT`。`articulationFor(language)` 追加语言指令(3 种)。

### 8.10 `learning.mjs`

**遗留**:`parseLearning` 只服务旧 schema(`learned_memories` + `chat_learning.style`);生产路径 `engine.cycle` 不再调用它。`learned_memories` 的写入实际已由 `store.learn` 内的循环处理,但 engine 传入的 `update.memories` 恒为 `[]`,因此该表在运行时基本不再增长(仅清理)。

---

## 9. 性能热点判定

### 9.1 「每条人类消息都跑」的热路径(最高优先级)

`Engine.ingest` 是唯一每条消息都执行的路径,含 **SQLite 写 + 分层记忆维护**:

| 序 | 操作 | 成本 | 复杂度 |
|---|---|---|---|
| 1 | `policy.normalize` | 正则 + 字符串拼接 + `slice` | O(文本长度) |
| 2 | `store.message` | **1 次写**(`INSERT OR IGNORE`) | O(log n) |
| 3 | `orientation.observe` | 1 次 UPDATE(仅群聊) | O(1) |
| 4 | `memory.capture` → `put` ×(1 或 2) | 每 subject:1 SELECT + 1 `INSERT..ON CONFLICT`;JSON 解析/序列化 `sources` | O(证据数≤12) |
| 5 | `memory.capture` → `enforce` | 1 UPDATE + 1 DELETE + 1 `SELECT id FROM memory_layers WHERE chat=?`(该 chat **全部**记忆行)+ N 次 revision DELETE + `SELECT DISTINCT subject` + **每 subject × 3 层各 1 次 `rows()` SELECT + 逐行删** | **O(该 chat 全部记忆行数)**,默认 200 人 × 3 层 |
| 6 | `store.observe` | 1 次 UPDATE | O(1) |

**结论**:`ingest` 的实际瓶颈是 **第 4–5 步的 `LayeredMemory.enforce`**。它对每个入站消息都在**同一 chat 的全部主体 × 全部层**上做 SELECT + 裁剪,而 chat 的主体上限默认 200(`maxPeople`)、每主体 3 层。高活跃群里这是每条消息 **十几次到几十次 SQL 往返 + JSON 编解码**。这是移植到 Rust 时收益最大的热点,也是最需要重新设计的地方(应改为增量维护 + 定期批量 enforce,而非每消息 enforce)。

次热点:`messages` 表的写入与 `PRAGMA WAL` 的 fsync 行为(每条消息一次事务提交)。当前代码**没有**把同一批消息合并成单事务。

### 9.2 「每个 cycle 都跑」(模型调用之间,秒级频率)

| 操作 | 成本 |
|---|---|
| `store.history` | 1 SELECT,default 24 行(learning 开启时 `max(24,8)=24`) |
| `store.counts` / `store.sendingTiming` | 各 1 次聚合 SELECT |
| `store.learningState` | 1 SELECT |
| `memory.context` | 每 subject × 2 层 1 次 `rows()` SELECT + `rankMemories`(**CPU**:逐文档 tokenize) |
| `store.retrieveScoped` | `notes`(≤50)+ `memory.short`(每 subject 1 SELECT,≤40)+ `rankMemories(requireMatch)` |
| `expressions.context` | 每 subject 1 SELECT + `rankMemories` |
| `store.reservoir` | 1–2 SELECT |
| `provider.json` ×2~4 | **网络主导**,单次可达数秒;占总耗时 95%+ |
| `store.learn`(仅 cadence 满足) | 1 个 `BEGIN IMMEDIATE` 事务,内含多次 SELECT/INSERT/DELETE |
| 各 `store.decision/assess/expect/delivery/message` | 若干单行写 |

**CPU 热点(纯计算)**:`memory-ranking.rankMemories` 的末尾贪心循环 —— `while (pending.length)` 每轮对**全部** pending 重新计算 `score()`,而 `score()` 内部对同 subject 的每个已选项调用 `overlap(s.text, r.text)`,**每次调用都重新 `tokens()` 两个字符串,没有缓存**。设候选数 n、同 subject 已选 m,则为 `O(n²·m)` 次 tokenize。候选来自 `memory_layers`(≤200 行)+ notes + expressions。这是明确的算法级热点,移植时应预先 tokenize 并缓存(或改用倒排索引)。

**次 CPU 热点**:`policy.repeated` 对最近 ≤24 条自身消息逐个 `similarity`(每个又是两次 `terms()` Set 构造)。

### 9.3 低频 / 空闲路径

| 操作 | 频率 | 说明 |
|---|---|---|
| `store.prune` | 每 3600 s | 每 chat 一次「保留最近 N 条」的子查询 + 多个 `DELETE`;chat 多时是秒级扫表,但可接受 |
| `memory.configure` | 每 3600 s | 对**全部** chat 跑 `enforce`(与 §9.1 第 5 步同代码,但批量) |
| `expressions.prune` | 每 3600 s | 每 chat 裁剪 |
| `status()` | 每 5 s | 一次小对象 JSON 序列化 + 原子文件写 |
| `engine.tick` | 每 1 s | 遍历 ≤`maxActiveChats` 的 Map,纯内存比较,无 IO |
| `orientation.beforeSpeak` | 每群一次 + 失败退避 | 3 个 OneBot 调用 + 1 次模型调用 |
| `provider.listModels` | 手动 | 1 次 HTTP |
| `Dashboard snapshot` | 每次 `/api/state`(前端 2 s) | 只读 DB 的 6~7 条 SELECT + 读日志尾巴 64 KiB + JSON 全量序列化 + 字符串替换脱敏。**每次都重新打开只读 DB 连接**,是仪表盘侧的主要开销 |
| 重连循环 | 事件驱动 | 退避时有 `sleep` |

### 9.4 排序结论(移植时按收益排序)

1. **`LayeredMemory.enforce` 的每消息全量扫描**(§9.1 第 5 步)—— 影响最大、最该重设计。
2. **`memory-ranking.rankMemories` 的无缓存 O(n²·m) tokenize** —— 纯 CPU,单次可达数万次字符串切片。
3. **每条消息一次 SQLite 事务提交**(WAL fsync)。
4. `messages` 表随 chat 增长后的 `history`/`counts` 查询(已有 `messages_chat_ts` 索引覆盖)。
5. `dashboard.snapshot` 每次请求重开连接 + 全量脱敏序列化。
6. 其余(定时器、prune、status)不构成瓶颈。

明确**不是**热点:`onebot` 的 JSON 编解码、`provider` 的请求构造、`prompts` 的字符串拼接、`config.validate`(仅启动/保存时跑)。

---

## 10. Rust / Node 边界建议

### 10.1 切分原则

现有代码已经天然分成**两个进程**:

- `src/main.mjs` + 引擎(`qq-inner-agent.service`)
- `src/dashboard.mjs` + `web/`(`qq-inner-dashboard.service`)

而且两者的耦合**只经过文件系统 + systemctl**(`status.json`、`agent.sqlite`、`config.json`/`secrets.json`、`.settings-write`、`agent.log`),**没有任何进程间 RPC**。
因此最小风险的切分是:**Rust 吃掉 agent 进程的全部,Node 保留 dashboard 进程**,再把现有的「文件轮询」升级为一条显式 IPC。

### 10.2 Rust 负责(建议 crate 划分)

| Rust 模块 | 对应 JS | 理由 |
|---|---|---|
| `onebot` | `onebot.mjs` | I/O + 协议状态机;Rust 的 tokio-tungstenite 更稳,且能统一超时/取消 |
| `provider` | `provider.mjs` | HTTP 客户端、重试/退避/限流;reqwest 原生支持 |
| `store` | `store.mjs` + `memory.mjs` + `expression.mjs` + `activity.mjs` + `orientation.mjs` | SQLite(rusqlite)+ 全部 schema/迁移/保留期;这是热路径 |
| `ranking` | `memory-ranking.mjs` | 纯计算,是 CPU 热点,可用倒排索引重写 |
| `policy` / `sending` / `activity-curve` | `policy.mjs` / `sending.mjs` / `activity.mjs` 的纯函数 | 无 IO,纯逻辑,便于单测 |
| `prompts` | `prompts.mjs` | 静态字符串 + 模板 |
| `engine` | `engine.mjs` | 调度、并发、失效判定 |
| `config` | `config.mjs` + `settings.mjs` | 校验/归一化/原子写/journal |
| `cli` / `diagnostics` | `cli.mjs` / `diagnostics.mjs` | 子命令与自检 |
| `main` | `main.mjs` | 启动、定时器、信号处理 |

### 10.3 Node 保留

| Node 模块 | 理由 |
|---|---|
| `dashboard.mjs` + `web/*`(app.js / index.html / i18n.mjs / CSS / favicon) | 已成熟:CSRF/会话/限流/CSP、TLS 嗅探 + 301 代理 + `peerFor` 端口映射、`MutationObserver` 就地 i18n。重写成本高、收益低,且它是低频管理面,不是性能路径 |
| `scripts/*.py`(setup / install_service / install_dashboard) | 交互式向导与 systemd/openssl 编排;可保留,或逐步替换为 `agent-core setup` 子命令(非必须) |

替代方案(不推荐作为第一步):dashboard 也用 axum + rustls 重写。风险集中在自签 CA 生成、TLS 首字节嗅探代理、CSRF/timing-safe 比较、以及 i18n 的 DOM 就地翻译,都是「能用就不动」的部件。

### 10.4 二者交换的数据

| 类别 | 内容 | 方向 |
|---|---|---|
| 持久状态 | `data/agent.sqlite`(**Rust 独占写**;Node 只读打开,`readOnly:true`) | Rust → 文件 → Node |
| 配置 | `config.json`、`secrets.json`(Node 写、Rust 读、`revision` = sha256 联合哈希) | 双向 |
| 保存 journal | `.settings-write`(两文件写入窗口的互斥/回滚标记) | 双向 |
| 心跳 | `data/status.json`(每 5 s) | Rust → Node |
| 日志 | `data/agent.log`(Rust 追加;Node 读尾) | Rust → Node |
| 服务控制 | `systemctl --user <action> qq-inner-agent.service` | Node → systemd |
| 访问密钥 | `data/dashboard-access.txt` | 本地文件 |

### 10.5 建议的控制接口(把文件轮询换成显式 IPC)

**传输**:Unix domain socket,路径 `<root>/data/agent.sock`(mode `0600`,同目录同权限语义)。
**分帧**:NDJSON(每行一个 JSON 对象)。
**方向与形状**:
- 请求(dashboard → Rust):`{"id":"<string>","method":"<string>","params":{...}}`
- 响应:`{"id":"<string>","ok":true,"result":{...}}` 或 `{"id":"<string>","ok":false,"error":{"code":"<string>","message":"<string>"}}`
- 事件(Rust → dashboard,无 `id`):`{"event":"<string>","data":{...}}`

**方法清单**(与当前 dashboard 能力一一对应):

| method | params | result |
|---|---|---|
| `ping` | `{}` | `{pong:true, pid, uptimeSeconds}` |
| `status.get` | `{}` | 与现 `status.json` 同字段:`{updatedAt,pid,mode,appliedRevision,reloading,reloadError,scheduleActive,activityRhythm,missing[],onebotConnected,qqOnline,selfId,reconnects,activeChats,model,provider,apiCallsThisRun,lastCycleAt,lastError}` |
| `config.get` | `{}` | `{config, revision, hasApiKey, hasOnebotToken}` |
| `config.set` | `{revision, config, apiKey?, onebotToken?, clearApiKey?}` | 同 `config.get`;错误码 `stale_revision`、`unknown_setting`、`invalid_*`、`data_directory_change` |
| `models.list` | `{}` | `{models:[string]}`;错误码 `save_api_key_first`、`models_http_<n>`、`invalid_model_list` |
| `model.test` | `{}` | `{ok:true, message:string}`;错误码来自 provider |
| `contacts.list` | `{}` | `{groups:[{id,name}], friends:[{id,name}]}` |
| `state.snapshot` | `{}` | `{status, decisions[], thoughts[], assessments[], learning[], memories[], observations[], expressions[], logs[], savedRevision}`(**密钥已脱敏**) |
| `memory.list` | `{chat?, subject?, layer?, limit?}` | `{memories:[{id,chat,subject,layer,slot,text,sources[],keywords[],importance,confidence,created,updated,expires,revision,revisions[]}]}` |
| `expressions.list` | `{chat?, subject?, limit?}` | `{expressions:[{chat,subject,kind,term,meaning,situation,example,confidence,sources[],updated,last_used}]}` |
| `observations.list` | `{limit?}` | `{observations:[{chat,started,message_count,status,collected,sources{},analysis{},retry_at,error,epoch}]}` |
| `learning.reset` | `{chat, subject?}` | `{ok:true}`;错误码 `invalid_chat`、`invalid_memory_subject` |
| `service.control` | `{action:"start"\|"stop"\|"restart"}` | `{serviceState:string}`(也可继续由 Node 直接调 systemctl) |
| `service.status` | `{}` | `{serviceState:"active"\|"inactive"\|"unknown"}` |
| `debug.send` | `{}` | `{account, messageId, text, message}` |
| `debug.receive.start` | `{}` | `{state, account, until, events[], error}` |
| `debug.receive.status` | `{}` | 同上 |
| `debug.receive.stop` | `{}` | 同上 |
| `logs.tail` | `{limit?:number}` | `{lines:[{time,event,...}]}` |
| `config.reload` | `{}` | `{appliedRevision}`(强制重载,替代文件 revision 轮询的备用路径) |
| `agent.stop` | `{}` | `{ok:true}`(优雅停机) |

**事件清单**(Rust 主动推送):

| event | data |
|---|---|
| `status` | 同 `status.get` 的 result(每 5 s + 状态变化即时) |
| `log` | `{time, event, ...}`(逐条;与 `agent.log` 同构) |
| `decision` | `{id, chat, ts, action, score, tags[]}` |
| `assessment` | `{id, chat, human_id, ts, status, details{factors,draw,timing,veto,prediction}}` |
| `thought` | `{id, chat, text, kind, created, used, score, subject}` |
| `memory.changed` | `{chat, subject, layer, slot, operation:"upsert"\|"forget"\|"expire", revision}` |
| `expression.changed` | `{chat, subject, kind, term, operation}` |
| `orientation.changed` | `{chat, status:"observing"\|"ready", epoch, availability{info,notices,history}}` |
| `connection` | `{state, selfId, reconnects, online}` |
| `config.applied` | `{revision, appliedRevision, reloading, reloadError?}` |
| `budget` | `{callsThisRun, requestsPerHour, windowUsed}` |

**兼容策略**:Node 侧先**双读**(socket 可用则订阅,否则回退到 `status.json` + 只读 SQLite)。这样 Rust 可以灰度上线,Rust 未就绪时现有 dashboard 完全不受影响;一旦 socket 就绪,前端轮询从 2 s 降到事件驱动。

**必须保持不变的契约**(否则 Node 侧要改):`status.json` 字段名与语义、`agent.sqlite` 的表名/列名/JSON 列编码、`config.json`/`secrets.json` 的键与 `revision` 计算方式、`/api/state` 与 `/api/config` 的响应体形状、错误码字符串(`waiting_for_setup`、`http_401_check_provider_config`、`invalid_formation`、`invalid_ratings`、`invalid_articulation`、`output_truncated_increase_maxTokens`、`hourly_api_budget`、`qq_offline`、`delivery_uncertain` 等,前端 `translate()` 与提示文案依赖它们)。

---

## 11. 移植风险与不确定点

### 11.1 高风险(必须在设计阶段解决)

1. **`node:sqlite` → rusqlite 的行为差异**:`INSERT OR REPLACE`/`ON CONFLICT DO UPDATE`、`rowid` 语义、`json_remove(sources,'$.history','$.notices')`(需要 SQLite JSON1 扩展)、触发器 `memory_revision_cleanup`、`CHECK(id=1)`、`PRAGMA table_info` 迁移探测、`PRAGMA journal_mode=WAL` + `busy_timeout`。`rowid NOT IN (SELECT rowid ... ORDER BY ts DESC,rowid DESC LIMIT ?)` 这类保留期写法在 Rust 中最好显式写 `rowid`。
2. **`Intl.DateTimeFormat` 的 IANA 时区语义**(`quiet`、`activeAt`、`activityProbability`、`schedule.timezone` 校验)——Rust 需 `chrono-tz`;`hourCycle:'h23'` 与 `formatToParts` 的边界(23:59→00:00、DST 跳变)必须逐个对齐。
3. **Unicode 分词**:`/[\p{L}\p{N}]{2,}/gu`、`/[\p{Script=Han}]+/gu`、`/\p{Script=Han}/u` 以及中文二元组(含代理对)在 Rust 中的正则与切片语义。若分词不一致,检索排序、`repeated` 去重、证据匹配(`expression_evidence_missing` 用 `text.includes(example)`)结果都会漂移。
4. **`AbortSignal` 取消语义**:引擎在 5 个检查点判断 `obsolete()`,provider 用 `AbortSignal.any([signal, timeout])`,onebot 用 `signal.addEventListener('abort', ...)`。Rust 需要 `tokio_util::sync::CancellationToken` + `select!`,并且要复刻「abort 后不保留部分状态」的精确时机。
5. **`LayeredMemory.enforce` 的每消息全量扫描(§9.1)** —— 若 1:1 照搬,性能问题会一并移植过来。建议在 Rust 侧重新设计为:入站只做 O(1) 追加,`enforce` 改为(消息计数/时间)触发的批处理。
6. **`rankMemories` 的 O(n²·m) 无缓存 tokenize(§9.2)** —— 同上,建议在 Rust 中预计算 token 集合并缓存到行上。

### 11.2 中风险(易漏的细节)

7. **`put` 的「不复活/不降级」规则**(§1.3)四处早返回语义微妙,尤其 `updated`/`expires` 只在 `fresh` 时刷新、`layer==='short_term'` 例外。写错会导致记忆永久续期或提前过期。
8. **`decorate` 的字符预算**:使用 `[...str]` 码点计数(非字节、非 UTF-16),emoji 占 1 个码点。Rust 用 `chars()` 对应;若用 `len()` 会不一致。
9. **`auto_escape: true` + 数组文本段**的双重防注入语义,以及 `faceId` 只能是白名单里的 `\d{1,5}`。
10. **`OneBot.session` 的早到事件缓冲**(`early` 上限 200)+ 「`connected` 置位后才 emit」的时序。
11. **`engine` 的热重载是全量重建**:`chats` Map 手工迁移(`{...state, busy:false, lastThink:0}`),`ActivityRhythm`/`GroupOrientation` 重建但共用同一 `store.db`。若 Rust 改为增量,需要保持 `epoch`/`signature` 语义不变。
12. **`revision(root)` = sha256(config + '\0' + secrets)** —— 改 secret 也触发重载;`.settings-write` 存在时暂停重载。dashboard 保存的 409 语义依赖它。
13. **`provider` 的错误分类表**:`[401,403,400,404,422]` 退避 300 s 且不重试;其他 4xx 不重试;429/5xx 重试;用尽则 `blockedUntil = now+60`。`this.calls++` 的计数时机(预算通过之后)影响 `apiCallsThisRun` 与 `hourly_api_budget`。
14. **`callBudget` 的副作用**:它**同时**做清理(删 1 小时前的行)与准入,且「达上限时不插入」。这是滚动限流,不是固定窗口;Rust 侧若换成 token bucket 会改变行为。
15. **`recoverDeliveries` 的启动语义**:所有遗留 `pending` → `uncertain`,永不重发。移植后如果启动顺序改变(先连接再恢复)可能重复发送。
16. **dashboard 的 TLS 嗅探代理**:首字节 `0x16` 判定 TLS、`peers` 按 `upstream.localPort → client address` 映射、`peerFor` 只在 `remoteAddress==='127.0.0.1'` 时启用。这是登录限流按真实客户端 IP 工作的前提;用通用反代会破坏它。
17. **`settings.knownConfig` 拒绝未知键,而 `loadConfig` 不拒绝** —— 这个不对称是 dashboard 保存与手工编辑的差异,移植时不要统一。
18. **`config.example.json` 已过期**:它缺少 `agent.learning`、`agent.observation`、`agent.sending` 三整块,且 `agent.memory` 只有 4 个键(`recallChars`/`recallHalfLifeDays`/`minConfidence`/`revisionLimit`),`provider.thinking` 是字符串 `"disabled"` 而默认是 `null`。示例文件**不能**当作完整 schema 使用。

### 11.3 低风险 / 纯体力

19. 5 段中文提示词与 380 行 i18n 映射表需要逐字搬运(含全角标点与 `\n`),不可「翻译」。
20. 15/17 张表的列名、JSON 列编码、时间单位(全部为 **秒**,`REAL`)—— `ts`/`updated`/`expires`/`started`/`until` 都是 `Date.now()/1000`,不是毫秒。
21. 错误码字符串是前后端契约(前端 `translate()` 依赖字面量)。
22. `test/` 下 13 个测试文件是现成的行为规格(`activity/core/dashboard/diagnostics/expression/integration/language/learning/memory/orientation/redirect/reload/sending`),建议作为 Rust 侧移植的验收基线。

### 11.4 未能确认 / 需外部验证

- **未能确认**:SnowLuma / NapCat 对 `message_sent` 事件、`reportSelfMessage`、`_get_group_notice`、`get_group_msg_history` 的实际支持情况与字段形状(代码只做了防御性解析,`orientation.mjs` 的注释也承认其他桥可能不实现扩展端点)。`VERIFICATION.md` 亦记录「live source availability remains unverified」。
- **未能确认**:真实 DeepSeek/Anthropic 网关对 `thinking: {type:'disabled'}`、`max_tokens` vs `max_completion_tokens`、`anthropic-workspace-id` 的支持差异。代码按「网关可能不支持」做了保守处理,但真实行为未经端到端验证。
- **未能确认**:`test/` 13 个测试文件的逐条断言(本轮只清点文件名,未逐行读)。若要在 Rust 侧建立等价测试,需要后续逐文件精读。
- **未读**:`web/style.css`、`web/language.css`、`web/favicon.svg`、`web/i18n.mjs` 的 380 行翻译对(只读了头部、`translate()` 实现与尾部)。这些不影响接口地图,只影响 UI 文案搬运量。
- **未跟踪但影响行为**:`data/` 目录下运行时会生成 `agent.log`(含轮转 `agent.log.1`)、`status.json`、`dashboard-access.txt`、TLS 证书等;Rust 侧需保持同名同权限(0600/0700)。
