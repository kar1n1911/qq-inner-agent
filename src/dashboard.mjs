import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import net from 'node:net';
import https from 'node:https';
import { randomBytes, createHash, timingSafeEqual } from 'node:crypto';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
import { DatabaseSync } from 'node:sqlite';
import { loadConfig } from './config.mjs';
import { publicSettings, saveSettings, readJson, recoverSettings } from './settings.mjs';
import { Provider, listModels } from './provider.mjs';
import { OneBot } from './onebot.mjs';
import { Diagnostics } from './diagnostics.mjs';
import { ControlClient } from './control.mjs';

const exec = promisify(execFile), hash = s => createHash('sha256').update(s).digest();
const equal = (a, b) => timingSafeEqual(hash(String(a)), hash(String(b)));
const fail = (status, message) => Object.assign(Error(message), { status });
const pluginHash = bytes => createHash('sha256').update(bytes).digest('hex');
const pluginListFile = root => path.join(root, 'plugins', 'plugins.json');
const pluginDraftFile = root => path.join(loadConfig(root).dataDir, 'plugin-draft.json');
function pluginLive(root) {
  const file = pluginListFile(root);
  const bytes = fs.existsSync(file) ? fs.readFileSync(file) : null;
  return { bytes, revision: bytes === null ? null : pluginHash(bytes) };
}
function pluginWrite(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const temporary = `${file}.${randomBytes(8).toString('hex')}.tmp`;
  try {
    fs.writeFileSync(temporary, value, { mode: 0o600 });
    fs.renameSync(temporary, file);
  } finally { if (fs.existsSync(temporary)) fs.unlinkSync(temporary); }
}
function pluginManifest(root, directory) {
  if (typeof directory !== 'string' || !/^[^/\\.][^/\\]*$/.test(directory))
    return { state: 'unrecognized', reason: 'invalid_directory' };
  const packageDir = path.join(root, 'plugins', directory);
  try { if (!fs.lstatSync(packageDir).isDirectory() || fs.lstatSync(packageDir).isSymbolicLink())
    return { state: 'unrecognized', reason: 'invalid_directory' }; }
  catch { return { state: 'unrecognized', reason: 'missing_directory' }; }
  const file = path.join(root, 'plugins', directory, 'manifest.json');
  if (!fs.existsSync(file)) return { state: 'unrecognized', reason: 'manifest_missing' };
  let manifest;
  try { manifest = JSON.parse(fs.readFileSync(file, 'utf8')); }
  catch { return { state: 'unrecognized', reason: 'invalid_json' }; }
  const valid = manifest && manifest.name === directory && typeof manifest.version === 'string' && manifest.version !== 'TBD'
    && ['Chat', 'Model', 'Db', 'Memory', 'External', 'Persona'].includes(manifest.kind)
    && ['InProcess', 'Sidecar', 'Remote'].includes(manifest.transport)
    && manifest.core_api_version?.major === 1 && Number.isInteger(manifest.core_api_version?.min_minor)
    && Array.isArray(manifest.dependencies) && Array.isArray(manifest.host_permissions)
    && (Array.isArray(manifest.config_schema) || !!manifest.config_schema && typeof manifest.config_schema === 'object')
    && manifest.capabilities && (manifest.hot_reload === undefined || typeof manifest.hot_reload === 'boolean')
    && manifest.status !== 'placeholder' && !JSON.stringify(manifest).includes('"TBD"');
  return valid ? { state: 'discovered', reason: null, manifest } : { state: 'unrecognized', reason: manifest?.status === 'placeholder' ? 'placeholder' : 'invalid_manifest' };
}
function pluginPublicList(root, list) {
  if (!Array.isArray(list?.plugins)) return null;
  for (const item of list.plugins) {
    if (!item || typeof item !== 'object') return null;
    const manifest = pluginManifest(root, item.directory).manifest;
    const fields = Array.isArray(manifest?.config_schema) ? manifest.config_schema :
      Object.entries(manifest?.config_schema || {}).map(([key, field]) => ({ key, ...field }));
    if (Object.keys(item.params || {}).some(key => fields.some(field => field.key === key && field.secret === true)
      || /(?:key|token|secret|password)/i.test(key))) return null;
  }
  return list;
}
function pluginDiscovery(root) {
  const folder = path.join(root, 'plugins');
  if (!fs.existsSync(folder)) return [];
  return fs.readdirSync(folder, { withFileTypes: true }).filter(item => item.isDirectory() && !item.isSymbolicLink())
    .map(item => { const result = pluginManifest(root, item.name); return { directory: item.name, state: result.state, reason: result.reason,
      ...(result.manifest ? { kind: result.manifest.kind, transport: result.manifest.transport,
        hot_reload: result.manifest.hot_reload === true, capabilities: result.manifest.capabilities,
        dependencies: result.manifest.dependencies } : {}) }; });
}
function pluginPackageRevision(root) {
  const folder = path.join(root, 'plugins');
  if (!fs.existsSync(folder)) return pluginHash('missing');
  const parts = fs.readdirSync(folder, { withFileTypes: true })
    .filter(item => item.isDirectory() && !item.isSymbolicLink())
    .map(item => [item.name, fs.existsSync(path.join(folder, item.name, 'manifest.json'))
      ? pluginHash(fs.readFileSync(path.join(folder, item.name, 'manifest.json'))) : null]);
  return pluginHash(JSON.stringify(parts));
}
function pluginValidateList(root, list) {
  if (!list || typeof list !== 'object' || Array.isArray(list) || list.schema_version !== 1
    || !['manual', 'auto'].includes(list.mode) || !Array.isArray(list.plugins)) throw fail(422, 'Invalid plugin list');
  const ids = new Set();
  for (const item of list.plugins) {
    if (!item || typeof item !== 'object' || typeof item.id !== 'string' || !item.id
      || ids.has(item.id) || typeof item.directory !== 'string' || !/^[^/\\.][^/\\]*$/.test(item.directory)
      || typeof item.enabled !== 'boolean'
      || !['params', 'bindings', 'secret_refs'].every(key => item[key] === undefined || item[key] && typeof item[key] === 'object' && !Array.isArray(item[key])))
      throw fail(422, 'Invalid plugin entry');
    ids.add(item.id);
    const { state, reason, manifest } = pluginManifest(root, item.directory);
    if (state !== 'discovered') throw fail(422, `Plugin ${item.id}: ${reason}`);
    const fields = Array.isArray(manifest.config_schema) ? Object.fromEntries(manifest.config_schema.map(field => [field.key, field])) : manifest.config_schema;
    for (const [key, value] of Object.entries(item.params || {})) {
      if (!fields[key] || fields[key].secret === true) throw fail(422, `Plugin ${item.id}: secret or unknown parameter ${key}`);
      const type = fields[key].type;
      if (!(type === 'array' ? Array.isArray(value) : type === 'object' ? value !== null && typeof value === 'object' && !Array.isArray(value)
        : type === 'integer' ? Number.isSafeInteger(value) : type === 'number' ? typeof value === 'number' && Number.isFinite(value) : type === 'string' ? typeof value === 'string'
          : type === 'boolean' && typeof value === 'boolean'))
        throw fail(422, `Plugin ${item.id}: invalid parameter ${key}`);
    }
    for (const [key, value] of Object.entries(item.secret_refs || {})) {
      if (fields[key]?.secret !== true || typeof value !== 'string' || !/^secrets\.[a-zA-Z0-9_]+$/.test(value))
        throw fail(422, `Plugin ${item.id}: invalid secret reference ${key}`);
    }
    for (const [role, target] of Object.entries(item.bindings || {})) {
      if (!manifest.dependencies.some(dep => dep.role === role) || typeof target !== 'string' || !target)
        throw fail(422, `Plugin ${item.id}: invalid binding ${role}`);
    }
    if (item.enabled) {
      for (const [key, field] of Object.entries(fields)) if (field.required === true
        && !Object.hasOwn(item.params || {}, key) && !Object.hasOwn(item.secret_refs || {}, key))
        throw fail(422, `Plugin ${item.id}: missing ${key}`);
      for (const dep of manifest.dependencies) if (dep.required === true && !Object.hasOwn(item.bindings || {}, dep.role))
        throw fail(422, `Plugin ${item.id}: missing binding ${dep.role}`);
    }
  }
}
function pluginMerged(root, proposal, replaceIds = []) {
  const live = pluginLive(root);
  const previous = live.bytes ? JSON.parse(live.bytes) : { schema_version: 1, mode: 'manual', plugins: [] };
  if (!Array.isArray(previous.plugins)) throw fail(422, 'Invalid current plugin list');
  const merged = structuredClone(previous);
  const ids = new Set(merged.plugins.map(item => item.id));
  for (const item of proposal.plugins) {
    if (ids.has(item.id)) {
      const index = merged.plugins.findIndex(old => old.id === item.id);
      if (JSON.stringify(merged.plugins[index]) !== JSON.stringify(item)) {
        if (!replaceIds.includes(item.id)) throw fail(409, `Plugin ${item.id} was edited; explicit replacement is required`);
        merged.plugins[index] = structuredClone(item);
      }
      continue;
    }
    ids.add(item.id); merged.plugins.push(structuredClone(item));
  }
  if (!live.bytes) merged.mode = proposal.mode;
  pluginValidateList(root, merged);
  return { merged, live };
}
function pluginSecretStates(root, manifest, item) {
  const fields = Array.isArray(manifest?.config_schema) ? manifest.config_schema :
    Object.entries(manifest?.config_schema || {}).map(([key, field]) => ({ key, ...field }));
  let stored = {};
  try { stored = readJson(path.join(root, 'secrets.json'), {}); } catch { /* unreadable is not configured */ }
  return Object.fromEntries(fields.filter(field => field.secret === true).map(field => {
    const reference = item?.secret_refs?.[field.key];
    const name = typeof reference === 'string' && /^secrets\.[a-zA-Z0-9_]+$/.test(reference)
      ? reference.slice('secrets.'.length) : null;
    return [field.key, { configured: !!(name && typeof stored[name] === 'string' && stored[name]) }];
  }));
}
function pluginProbe(endpoint, timeoutMs = 1500) {
  return new Promise(resolve => {
    let url;
    try { url = new URL(endpoint); } catch { resolve({ state: 'failed', code: 'invalid_endpoint' }); return; }
    if (!['http:', 'https:'].includes(url.protocol)) { resolve({ state: 'unsupported' }); return; }
    const socket = net.connect({ host: url.hostname, port: Number(url.port) || (url.protocol === 'https:' ? 443 : 80) });
    let finished = false;
    const finish = result => { if (finished) return; finished = true; socket.destroy(); resolve(result); };
    socket.setTimeout(timeoutMs, () => finish({ state: 'failed', code: 'timeout' }));
    socket.once('connect', () => finish({ state: 'connected', evidenceSource: 'tcp_connect' }));
    socket.once('error', () => finish({ state: 'failed', code: 'connection_failed' }));
  });
}
async function body(req) {
  if (!String(req.headers['content-type']).startsWith('application/json')) throw fail(415, 'JSON required');
  const chunks = []; let size = 0;
  for await (const chunk of req) { size += chunk.length; if (size > 100_000) throw fail(413, 'Request too large'); chunks.push(chunk); }
  try { return JSON.parse(Buffer.concat(chunks)); } catch { throw fail(400, 'Invalid JSON'); }
}
function json(res, status, data) { res.writeHead(status, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(data)); }
function tail(file, limit = 100) {
  if (!fs.existsSync(file)) return [];
  const fd = fs.openSync(file, 'r');
  try {
    const size = fs.fstatSync(fd).size, start = Math.max(0, size - 65536), buf = Buffer.alloc(size - start);
    fs.readSync(fd, buf, 0, buf.length, start);
    const lines = buf.toString().split('\n'); if (start) lines.shift();
    return lines.filter(Boolean).slice(-limit).map(line => { try { return JSON.parse(line); } catch { return null; } }).filter(Boolean);
  } finally { fs.closeSync(fd); }
}
export function createDashboard({ root, settings, key, serviceControl, serviceStatus, makeBot, controlOptions }) {
  const control = new ControlClient(loadConfig(root).dataDir, controlOptions);
  let capture = null;
  const diagnostics = new Diagnostics(() => loadConfig(root), makeBot);
  const sessions = new Map(), attempts = new Map(); let saving = false, testing = false;
  const pluginSelfTests = new Map();
  const origins = new Set(settings.origins), hosts = new Set([...origins].map(o => new URL(o).host));
  serviceControl ||= async action => { await exec('systemctl', ['--user', action, 'qq-inner-agent.service'], { timeout: 30000 }); };
  serviceStatus ||= async () => {
    try { return (await exec('systemctl', ['--user', 'show', 'qq-inner-agent.service', '-p', 'ActiveState', '--value'], { timeout: 2000 })).stdout.trim(); }
    catch { return 'unknown'; }
  };
  function fileSnapshot() {
    const c = loadConfig(root), file = path.join(c.dataDir, 'agent.sqlite');
    let status = null, logs = [];
    try { status = readJson(path.join(c.dataDir, 'status.json'), null); } catch { /* 心跳文件异常也不能阻断回退。 */ }
    try { logs = tail(path.join(c.dataDir, 'agent.log')); } catch { /* 日志轮换或权限错误时返回空列表。 */ }
    let decisions = [], thoughts = [], assessments = [], learning = [], memories = [], observations = [], expressions = [];
    if (fs.existsSync(file)) {
      let db;
      try {
        db = new DatabaseSync(file, { readOnly: true });
        decisions = db.prepare('SELECT chat,ts,action,score,tags FROM decisions ORDER BY ts DESC LIMIT 30').all();
        thoughts = db.prepare('SELECT chat,text,kind,score,created FROM thoughts WHERE used=0 AND created>? ORDER BY created DESC LIMIT 12').all(Date.now()/1000 - c.agent.thoughtTtlSeconds);
        assessments = db.prepare('SELECT chat,ts,status,details FROM send_assessments ORDER BY ts DESC LIMIT 12').all().map(r => ({ ...r, details: JSON.parse(r.details) }));
        memories = db.prepare("SELECT * FROM memory_layers WHERE expires>? ORDER BY CASE layer WHEN 'long_term' THEN 0 WHEN 'traits' THEN 1 ELSE 2 END,updated DESC LIMIT 200").all(Date.now()/1000);
        if (db.prepare("SELECT name FROM sqlite_master WHERE name='memory_revisions'").get()) for (const m of memories) m.revisions = db.prepare('SELECT revision,text,sources,updated,replaced FROM memory_revisions WHERE memory_id=? ORDER BY revision DESC LIMIT 10').all(m.id);
        if (db.prepare("SELECT name FROM sqlite_master WHERE name='expressions'").get()) expressions = db.prepare('SELECT * FROM expressions WHERE updated>? ORDER BY updated DESC LIMIT 200').all(Date.now()/1000-c.agent.expression.retentionDays*86400);
        learning = [...new Set(memories.map(m => m.chat))].map(chat => ({ chat }));
        if (db.prepare("SELECT name FROM sqlite_master WHERE name='group_orientation'").get()) observations = db.prepare('SELECT chat,started,message_count,status,sources,analysis,retry_at,error FROM group_orientation ORDER BY started DESC LIMIT 100').all().map(r => ({ ...r, sources: JSON.parse(r.sources), analysis: JSON.parse(r.analysis) }));
      } catch { /* database may be opening for the first time */ }
      finally { db?.close(); }
    }
    const data = { status, decisions, thoughts, assessments, learning, memories, observations, expressions, logs, savedRevision: publicSettings(root).revision };
    let text = JSON.stringify(data);
    for (const secret of [c.apiKey, c.onebotToken, key].filter(Boolean)) text = text.split(secret).join('[redacted]');
    return JSON.parse(text);
  }
  async function snapshot() {
    // 回退路径必须始终可用：先保留文件/只读 SQLite 快照，任何控制错误都不影响 HTTP 状态页。
    const data = fileSnapshot();
    if (!control.available) return data;
    const c = loadConfig(root);
    const results = await Promise.allSettled([
      control.request('state.get'), control.request('learning.list', { limit: 1000 }), control.request('logs.tail', { lines: 100 })
    ]);
    if (results[0].status === 'fulfilled' && results[0].value && typeof results[0].value === 'object' && !Array.isArray(results[0].value)) {
      const { learningCounts, ...status } = results[0].value; data.status = status;
    }
    const entries = results[1].status === 'fulfilled' && results[1].value?.entries;
    if (Array.isArray(entries) && entries.every(e => e && typeof e === 'object')) {
      data.memories = entries.filter(e => e.layer && e.expires > Date.now()/1000).slice(0, 200);
      data.learning = [...new Set(data.memories.map(m => m.chat))].map(chat => ({ chat }));
      // v1 先返回全部记忆再返回表达；到达总上限时表达可能不完整，沿用 SQLite 的完整结果。
      if (entries.length < 1000) data.expressions = entries.filter(e => !e.layer && e.updated > Date.now()/1000-c.agent.expression.retentionDays*86400).slice(0, 200);
    }
    const lines = results[2].status === 'fulfilled' && results[2].value?.lines;
    if (Array.isArray(lines)) data.logs = lines.map(line => { try { return JSON.parse(line); } catch { return null; } }).filter(Boolean);
    let text = JSON.stringify(data);
    for (const secret of [c.apiKey, c.onebotToken, key].filter(Boolean)) text = text.split(secret).join('[redacted]');
    return JSON.parse(text);
  }
  async function receiveStatus() {
    if (!control.available) return diagnostics.status();
    try {
      const result = await control.request('debug.receive.status');
      if (!capture && !result.listening) return { state: 'idle', events: [] };
      capture ||= { account: '', until: null, error: null };
      capture = { ...capture, state: result.listening ? 'listening' : capture.state === 'stopped' ? 'stopped' : 'finished', events: result.events };
      return capture;
    } catch { return diagnostics.status(); }
  }
  async function remoteDiagnostic(method) {
    if (method === 'debug.send') {
      const state = await control.request('state.get');
      const result = await control.request(method, {}, { timeoutMs: 30000 });
      // v1 不返回随机生成的发送正文；保留旧字段与类型，不伪造实际正文。
      return { account: state.selfId || '', messageId: result.messageId ?? null, text: '',
        message: 'Bridge accepted the self-message. Check your QQ self-chat to confirm delivery.' };
    }
    if (method === 'debug.receive.start') {
      const state = await control.request('state.get');
      const result = await control.request(method);
      capture = { state: 'listening', account: state.selfId || '', until: result.until, events: [], error: null };
    } else {
      await receiveStatus();
      await control.request(method);
      if (capture) capture.state = 'stopped';
    }
    return capture || { state: 'idle', events: [] };
  }
  const clientAddress = (req, peer) => peer?.address || req.socket.remoteAddress;
  const handler = async (req, res, peer) => {
    const secure = !!req.socket.encrypted;
    res.setHeader('Cache-Control', 'no-store');
    res.setHeader('X-Content-Type-Options', 'nosniff');
    res.setHeader('Referrer-Policy', 'no-referrer');
    res.setHeader('X-Frame-Options', 'DENY');
    res.setHeader('Content-Security-Policy', "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'");
    try {
      if (!hosts.has(req.headers.host)) throw fail(403, 'Unknown dashboard address');
      const url = new URL(req.url, 'http://localhost');
      if (req.method === 'GET' && ['/', '/app.js', '/i18n.mjs', '/style.css', '/language.css', '/favicon.svg'].includes(url.pathname)) {
        const name = url.pathname === '/' ? 'index.html' : url.pathname.slice(1);
        const type = { 'index.html': 'text/html; charset=utf-8', 'app.js': 'text/javascript; charset=utf-8', 'i18n.mjs': 'text/javascript; charset=utf-8', 'style.css': 'text/css; charset=utf-8', 'language.css': 'text/css; charset=utf-8', 'favicon.svg': 'image/svg+xml' }[name];
        res.writeHead(200, { 'Content-Type': type }); res.end(fs.readFileSync(path.join(root, 'web', name))); return;
      }
      const origin = req.headers.origin;
      if (req.method !== 'GET' && (!origin || !origins.has(origin) || new URL(origin).host !== req.headers.host)) throw fail(403, 'Origin rejected');
      if (url.pathname === '/api/login' && req.method === 'POST') {
        const ip = clientAddress(req, peer), now = Date.now();
        for (const [id, value] of attempts) if (value.until < now) attempts.delete(id);
        const attempt = attempts.get(ip) || { count: 0, until: now + 600_000 };
        if (attempt.count >= 8 || attempts.size > 1000) throw fail(429, 'Too many attempts. Try again in ten minutes.');
        const input = await body(req);
        if (!equal(input.key || '', key)) { attempt.count++; attempts.set(ip, attempt); throw fail(401, 'Incorrect access key'); }
        attempts.delete(ip);
        for (const [id, s] of sessions) if (s.expires < now) sessions.delete(id);
        if (sessions.size >= 20) sessions.delete(sessions.keys().next().value);
        const id = randomBytes(32).toString('hex'), csrf = randomBytes(24).toString('hex');
        sessions.set(id, { csrf, expires: now + 12 * 3600_000, settingsHistory: new Map() });
        res.setHeader('Set-Cookie', `qia_session=${id}; HttpOnly; SameSite=Strict; Path=/; Max-Age=43200${secure ? '; Secure' : ''}`);
        json(res, 200, { csrf }); return;
      }
      const id = String(req.headers.cookie || '').split(';').map(x => x.trim()).find(x => x.startsWith('qia_session='))?.slice(12);
      const session = sessions.get(id);
      if (!session || session.expires < Date.now()) { if (id) sessions.delete(id); throw fail(401, 'Sign in required'); }
      if (req.method !== 'GET' && !equal(req.headers['x-csrf-token'] || '', session.csrf)) throw fail(403, 'Session verification failed; sign in again.');
      if (url.pathname === '/api/session' && req.method === 'GET') { json(res, 200, { csrf: session.csrf }); return; }
      if (url.pathname === '/api/plugins/discover' && req.method === 'GET') {
        const live = pluginLive(root);
        const list = live.bytes ? pluginPublicList(root, JSON.parse(live.bytes)) : { schema_version: 1, mode: 'manual', plugins: [] };
        json(res, 200, { discovery: pluginDiscovery(root), liveRevision: live.revision,
          packageRevision: pluginPackageRevision(root),
          list, ...(list ? {} : { listError: 'Existing plugin list contains a raw credential. Correct it locally before using the editor.' }) }); return;
      }
      if (url.pathname === '/api/plugins/draft' && req.method === 'GET') {
        json(res, 200, readJson(pluginDraftFile(root), { state: 'idle', changes: [] })); return;
      }
      if (url.pathname === '/api/plugins/draft' && req.method === 'DELETE') {
        const file = pluginDraftFile(root);
        if (fs.existsSync(file)) fs.unlinkSync(file);
        json(res, 200, { state: 'idle', changes: [] }); return;
      }
      if (url.pathname === '/api/plugins/draft' && req.method === 'PUT') {
        const input = await body(req);
        pluginValidateList(root, input.list);
        const replaceIds = Array.isArray(input.replaceIds) && input.replaceIds.every(id => typeof id === 'string') ? input.replaceIds : [];
        const { merged, live } = pluginMerged(root, input.list, replaceIds);
        const previous = live.bytes ? JSON.parse(live.bytes) : { plugins: [] };
        const oldItems = new Map(previous.plugins.map(item => [item.id, item]));
        const changes = merged.plugins.filter(item => JSON.stringify(oldItems.get(item.id)) !== JSON.stringify(item))
          .map(item => ({ action: oldItems.has(item.id) ? 'update' : 'add', id: item.id, directory: item.directory, enabled: item.enabled }));
        const draft = { state: 'staged', draft_id: randomBytes(16).toString('hex'), baselineRevision: live.revision,
          packageRevision: pluginPackageRevision(root), list: input.list, replaceIds, changes, preview: { changes, target: merged } };
        pluginWrite(pluginDraftFile(root), JSON.stringify(draft, null, 2) + '\n');
        json(res, 200, draft); return;
      }
      if (url.pathname === '/api/plugins/activate' && req.method === 'POST') {
        await body(req);
        const draft = readJson(pluginDraftFile(root), null);
        if (!draft) throw fail(409, 'No staged plugin changes');
        if (pluginLive(root).revision !== draft.baselineRevision) throw fail(409, 'Plugin list changed elsewhere; review the draft again');
        if (pluginPackageRevision(root) !== draft.packageRevision) throw fail(409, 'Plugin manifests changed; recognize and review again');
        const { merged, live } = pluginMerged(root, draft.list, draft.replaceIds);
        const text = JSON.stringify(merged, null, 2) + '\n';
        const targetRevision = pluginHash(text);
        const changed = draft.changes.map(change => change.id);
        if (!changed.length) throw fail(409, 'No plugin changes to activate');
        const threshold = Number.isInteger(settings.pluginRestartThreshold) && settings.pluginRestartThreshold > 0
          ? settings.pluginRestartThreshold : 3;
        const topologyChanged = changed.some(id => {
          const before = live.bytes ? JSON.parse(live.bytes).plugins.find(plugin => plugin.id === id) : null;
          const after = merged.plugins.find(plugin => plugin.id === id);
          return before && JSON.stringify(before.bindings || {}) !== JSON.stringify(after.bindings || {});
        });
        const restart = settings.pluginHotReloadAvailable === false || changed.length >= threshold || topologyChanged || changed.some(id => {
          const item = merged.plugins.find(plugin => plugin.id === id);
          return pluginManifest(root, item.directory).manifest?.hot_reload !== true;
        });
        const recordFile = path.join(loadConfig(root).dataDir, 'plugin-activation.json');
        const listFile = pluginListFile(root);
        const restartMarker = path.join(root, '.plugin-restart-pending');
        if (restart) pluginWrite(restartMarker, `${targetRevision}\n`);
        pluginWrite(listFile, text);
        try {
          if (restart) await serviceControl('restart');
        } catch (error) {
          if (live.bytes) pluginWrite(listFile, live.bytes);
          else fs.unlinkSync(listFile);
          throw fail(502, `Plugin framework restart failed: ${error.message}`);
        } finally {
          if (restart && fs.existsSync(restartMarker)) fs.unlinkSync(restartMarker);
        }
        const oldActiveIds = live.bytes ? JSON.parse(live.bytes).plugins.filter(item => item.enabled).map(item => item.id) : [];
        pluginWrite(recordFile, JSON.stringify({ targetRevision, oldRevision: live.revision, oldActiveIds,
          pluginIds: changed, strategy: restart ? 'restart' : 'hot_reload',
          requestedAt: new Date().toISOString() }) + '\n');
        fs.unlinkSync(pluginDraftFile(root));
        json(res, 200, { targetRevision, strategy: restart ? 'restart' : 'hot_reload', pending: true }); return;
      }
      if (url.pathname === '/api/plugins/self-test' && req.method === 'POST') {
        const input = await body(req);
        const live = pluginLive(root);
        const configured = live.bytes ? pluginPublicList(root, JSON.parse(live.bytes)) : null;
        const item = configured?.plugins?.find(plugin => plugin.id === input.id);
        if (!item) throw fail(404, 'Plugin instance not found');
        const packageInfo = pluginManifest(root, item.directory);
        let result = { state: 'unsupported' };
        if (packageInfo.state === 'discovered' && packageInfo.manifest.kind === 'Model'
          && typeof item.params?.base_url === 'string') result = await pluginProbe(item.params.base_url);
        result = { ...result, id: item.id, checkedAt: new Date().toISOString() };
        pluginSelfTests.set(item.id, result);
        json(res, 200, result); return;
      }
      if (url.pathname === '/api/plugins/status' && req.method === 'GET') {
        const dataDir = loadConfig(root).dataDir;
        const record = readJson(path.join(dataDir, 'plugin-activation.json'), null);
        const running = readJson(path.join(dataDir, 'status.json'), null);
        const live = pluginLive(root);
        const configured = live.bytes ? pluginPublicList(root, JSON.parse(live.bytes)) : null;
        const draft = readJson(pluginDraftFile(root), null);
        const listed = configured?.plugins || [];
        const proposed = draft?.state === 'staged' ? draft.list?.plugins || [] : [];
        const targetRevision = record?.targetRevision ?? live.revision;
        const appliedRevision = running?.pluginAppliedRevision ?? null;
        const pending = !!targetRevision && targetRevision !== appliedRevision;
        const error = pending ? running?.reloadError : null;
        const pluginIds = record?.pluginIds || [];
        const structured = pending ? running?.pluginReloadFailure : null;
        const failedIds = structured?.pluginId ? [structured.pluginId] : error
          ? pluginIds.filter(id => String(error).includes(id)) : [];
        const alerts = failedIds.map(pluginId => ({ event: 'plugin_hot_reload_failed', pluginId,
          code: structured?.code || 'plugin_reload_failed', message: structured ? 'Plugin update was rejected; previous instance remains active.' : String(error) }));
        const oldInstanceRunning = pluginIds.some(id => running?.plugins?.[id]?.effective_state === 'active'
          && running.plugins[id].instanceRevision !== targetRevision)
          || !!(pending && record?.oldRevision && running?.pluginAppliedRevision === record.oldRevision
            && record.oldActiveIds?.some(id => pluginIds.includes(id)));
        const discovered = pluginDiscovery(root);
        const entries = new Map();
        for (const found of discovered) entries.set(found.directory, { id: found.directory, directory: found.directory });
        for (const item of listed) entries.set(item.id, item);
        for (const item of proposed) entries.set(item.id, item);
        for (const id of pluginIds) if (!entries.has(id)) entries.set(id, { id, directory: id });
        const plugins = Object.fromEntries([...entries].map(([id, item]) => {
          const found = pluginManifest(root, item.directory);
          const observed = running?.plugins?.[id];
          const hasEvidence = !!(appliedRevision && observed && observed.instanceRevision === appliedRevision);
          const alarm = alerts.some(alert => alert.pluginId === id);
          const oldRunning = !!((observed?.effective_state === 'active' && observed.instanceRevision !== targetRevision)
            || (pending && record?.oldRevision && appliedRevision === record.oldRevision
              && record.oldActiveIds?.includes(id)));
          const effective_state = alarm ? 'abnormal' : found.state !== 'discovered' ? 'unrecognized'
            : item.enabled === false ? 'disabled' : hasEvidence ? observed.effective_state || 'unknown'
              : item.enabled === true && pending ? 'pending' : 'unknown';
          return [id, {
            id, directory: item.directory, kind: found.manifest?.kind ?? null,
            transport: found.manifest?.transport ?? null, requested_enabled: item.enabled === true,
            discoveryState: found.state, reason: found.reason,
            effective_state, targetRevision,
            instanceRevision: hasEvidence ? observed.instanceRevision : oldRunning ? observed?.instanceRevision ?? appliedRevision : null,
            oldInstanceRunning: oldRunning,
            declaredCapabilities: found.manifest?.capabilities ?? null,
            effectiveCapabilities: hasEvidence ? observed.capabilities ?? observed.effectiveCapabilities ?? null : null,
            dependencies: found.manifest?.dependencies ?? [], bindings: item.bindings ?? {},
            params: item.params ?? {}, configSchema: found.manifest?.config_schema ?? [],
            secrets: pluginSecretStates(root, found.manifest, item),
            hostPermissions: found.manifest?.host_permissions ?? [], hotReload: found.manifest?.hot_reload === true,
            portState: hasEvidence ? observed.portState ?? 'unknown' : 'unknown',
            observedAt: hasEvidence ? observed.observedAt ?? null : null,
            evidenceSource: hasEvidence ? observed.evidenceSource ?? null : null,
            selfTest: pluginSelfTests.get(id) ?? null,
          }];
        }));
        json(res, 200, { targetRevision, appliedRevision, pending, strategy: record?.strategy ?? null,
          alerts, plugins, oldInstanceRunning, effective_state: alerts.length ? 'abnormal' : pending ? 'pending' : 'active' }); return;
      }
      if (url.pathname === '/api/model-plugins' && req.method === 'GET') {
        json(res, 200, JSON.parse(fs.readFileSync(path.join(root, 'rust/src/engine/io/adapters/model_config.json'), 'utf8'))); return;
      }
      if (url.pathname === '/api/models' && req.method === 'POST') {
        try { const c = loadConfig(root); if (c.provider.ioPort !== 'legacy' && !control.available) throw fail(503, 'Core control socket unavailable for selected model plugin'); json(res, 200, { models: control.available ? (await control.request('models.list', {}, { timeoutMs: 30000 })).models : await listModels(c.provider, c.apiKey) }); }
        catch (e) { json(res, e.status || 502, { error: e.status ? e.message : e instanceof Error && e.code ? e.code : 'model_list_unavailable_use_manual_entry' }); }
        return;
      }
      if (url.pathname === '/api/debug/receive' && req.method === 'GET') { json(res, 200, await receiveStatus()); return; }
      if (req.method === 'POST' && ['/api/debug/send', '/api/debug/receive', '/api/debug/stop'].includes(url.pathname)) {
        try {
          // 已发出的诊断请求绝不在断线后重试，防止重复发消息。
          const result = control.available ? await remoteDiagnostic(url.pathname === '/api/debug/send' ? 'debug.send' : url.pathname === '/api/debug/receive' ? 'debug.receive.start' : 'debug.receive.stop') : url.pathname === '/api/debug/send' ? await diagnostics.send()
            : url.pathname === '/api/debug/receive' ? await diagnostics.listen() : await diagnostics.stop();
          json(res, 200, result);
        } catch (e) {
          const code = e.code || e.message;
          json(res, 502, { error: /^[a-z0-9_]+$/.test(code) ? code : 'diagnostic_failed',
            hint: 'Check bridge URL, token and QQ login. A failed or timed-out send is not retried; check QQ before trying again.' });
        }
        return;
      }
      if (url.pathname === '/api/logout' && req.method === 'POST') {
        sessions.delete(id); res.setHeader('Set-Cookie', `qia_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0${secure ? '; Secure' : ''}`); json(res, 200, { ok: true }); return;
      }
      if (url.pathname === '/api/config' && req.method === 'GET') { json(res, 200, publicSettings(root, session.settingsHistory)); return; }
      if (url.pathname === '/api/config' && req.method === 'PUT') {
        if (saving) throw fail(409, 'A save is already in progress');
        const input = await body(req); saving = true;
        try { json(res, 200, saveSettings(root, input, session.settingsHistory)); }
        catch (e) { throw fail(e.status || 400, e.message); }
        finally { saving = false; }
        return;
      }
      if (url.pathname === '/api/state' && req.method === 'GET') { json(res, 200, { ...await snapshot(), serviceState: await serviceStatus() }); return; }
      if (url.pathname === '/api/learning/reset' && req.method === 'POST') {
        const input = await body(req);
        if (typeof input.chat !== 'string' || !/^(group|private):[1-9]\d{0,19}$/.test(input.chat)) throw fail(400, 'Invalid chat');
        if (input.subject !== undefined && !(input.subject === 'group' && input.chat.startsWith('group:')) &&
            !(typeof input.subject === 'string' && /^person:[1-9]\d{0,19}$/.test(input.subject) &&
              (input.chat.startsWith('group:') || input.subject.slice(7) === input.chat.slice(8)))) throw fail(400, 'Invalid memory subject');
        if (!control.available) throw fail(503, 'Core control socket unavailable. Start the core service and retry; learning was not reset.');
        try {
          await control.request('learning.reset', { chat: input.chat, ...(input.subject === undefined ? {} : { subject: input.subject }) });
        } catch {
          throw fail(502, 'Learning reset was not confirmed by the core. Check the core service and refresh learning state before retrying.');
        }
        json(res, 200, { ok: true }); return;
      }
      if (url.pathname === '/api/service' && req.method === 'POST') {
        const input = await body(req);
        if (!['start', 'stop', 'restart'].includes(input.action)) throw fail(400, 'Unknown service action');
        await serviceControl(input.action); json(res, 200, { serviceState: await serviceStatus() }); return;
      }
      if (url.pathname === '/api/agent' && req.method === 'POST') {
        const input = await body(req);
        const methods = { pause: 'agent.pause', disengage: 'agent.stop_participation', resume: 'agent.resume', exit: 'agent.exit' };
        if (!Object.hasOwn(methods, input.action)) throw fail(400, 'Unknown agent action');
        if (!control.available) throw fail(503, 'Core control socket unavailable');
        json(res, 200, await control.request(methods[input.action])); return;
      }
      if (url.pathname === '/api/test-model' && req.method === 'POST') {
        if (testing) throw fail(409, 'A connection test is in progress');
        testing = true;
        try {
          const c = loadConfig(root); if ((!c.apiKey && c.provider.ioPort !== 'ollama') || !c.provider.model) throw fail(400, 'Save an API key and model first');
          if (c.provider.ioPort !== 'legacy' && !control.available) throw fail(503, 'Core control socket unavailable for selected model plugin');
          if (control.available) {
            await control.request('test.model', {}, { timeoutMs: 120000 });
            json(res, 200, { ok: true, message: 'API authentication and JSON response verified. No QQ message sent.' }); return;
          }
          const p = new Provider(c.provider, c.apiKey, path.join(c.dataDir, 'agent.sqlite'));
          const response = await p.json('只返回 JSON：{"ok":true}。', { test: '仅测试 API 连通性，不包含 QQ 聊天内容' });
          if (response.ok !== true) throw fail(502, 'Unexpected model response');
          json(res, 200, { ok: true, message: 'API authentication and JSON response verified. No QQ message sent.' });
        } catch (e) { throw fail(e.status || 502, e.code || 'Model test failed'); }
        finally { testing = false; }
        return;
      }
      if (url.pathname === '/api/contacts' && req.method === 'GET') {
        if (control.available) {
          try { json(res, 200, await control.request('contacts.list', {}, { timeoutMs: 30000 })); return; }
          catch { /* 只读联系人查询可安全回退到原连接。 */ }
        }
        const c = loadConfig(root), bot = new OneBot(c.onebot, c.onebotToken), stop = new AbortController();
        let timer;
        const ready = new Promise((resolve, reject) => {
          bot.on('status', s => { if (s === 'connected') resolve(); });
          timer = setTimeout(() => reject(fail(503, 'QQ is not online. Check the NapCat / SnowLuma OneBot WebSocket URL, token, and QQ login.')), 15000);
        });
        const running = bot.start(stop.signal);
        try {
          await ready;
          const groups = await bot.call('get_group_list', {}), friends = await bot.call('get_friend_list', {});
          json(res, 200, { groups: (groups || []).map(x => ({ id: String(x.group_id), name: x.group_name })), friends: (friends || []).map(x => ({ id: String(x.user_id), name: x.nickname })) });
        } finally { clearTimeout(timer); stop.abort(); await running; }
        return;
      }
      throw fail(404, 'Not found');
    } catch (e) { if (!res.headersSent) json(res, e.status || 500, { error: e.status ? e.message : 'Dashboard operation failed' }); else res.end(); }
  };
  return { handler, snapshot, control, close: () => { control.close(); return diagnostics.stop(); } };
}

function redirectToHttps(req, host, knownHosts) {
  const raw = String(host || "");
  const authority = knownHosts.has(raw) ? raw.replace(/:\d+$/, "") : null;
  const path = String(req.url || "/") || "/";
  const target = `https://${authority || req.socket.localAddress}:${req.socket.localPort}${path}`;
  const notice = "This dashboard is served over HTTPS. Redirecting to the encrypted address.\n";
  req.socket.end([
    "HTTP/1.1 301 Moved Permanently",
    `Location: ${target}`,
    "Content-Type: text/plain; charset=utf-8",
    `Content-Length: ${Buffer.byteLength(notice)}`,
    "Cache-Control: no-store",
    "Connection: close",
    "", notice
  ].join("\r\n"));
}
// Only TLS traffic opens an upstream connection. Its local port identifies the
// corresponding HTTPS request socket's remote port, including after TLS wrapping.
export function createHttpsRedirectProxy({ upstreamPort, knownHosts = new Set(), onError = () => {} }) {
  const peers = new Map(), sockets = new Set();
  const parsed = http.createServer((req, res) => redirectToHttps(req, req.headers.host, knownHosts));
  parsed.requestTimeout = 20000; parsed.headersTimeout = 10000; parsed.maxHeadersCount = 50;
  const server = net.createServer(socket => {
    sockets.add(socket);
    let upstream, peerPort, peer;
    socket.once('close', () => {
      sockets.delete(socket);
      upstream?.destroy();
    });
    socket.on('error', onError);
    socket.setTimeout(10000, () => socket.destroy());
    socket.once('data', chunk => {
      socket.pause();
      socket.unshift(chunk);
      // A TLS ClientHello begins with a handshake record (0x16). Let the HTTP
      // parser handle all other bytes, including request lines split across packets.
      if (chunk[0] !== 0x16) {
        parsed.emit('connection', socket);
        socket.resume();
        return;
      }
      socket.setTimeout(0);
      upstream = net.connect(upstreamPort, '127.0.0.1');
      upstream.on('error', error => { onError(error); socket.destroy(); });
      upstream.once('close', () => {
        if (peers.get(peerPort) === peer) peers.delete(peerPort);
        socket.destroy();
      });
      upstream.once('connect', () => {
        if (socket.destroyed) { upstream.destroy(); return; }
        peerPort = upstream.localPort;
        peer = { address: socket.remoteAddress };
        peers.set(peerPort, peer);
        socket.pipe(upstream).pipe(socket);
        socket.resume();
      });
    });
  });
  let closing;
  return {
    server,
    peerFor: (req, fallback) => req.socket.remoteAddress === '127.0.0.1'
      ? peers.get(req.socket.remotePort)?.address || fallback : fallback,
    close: () => closing ||= new Promise(resolve => {
      server.close(resolve);
      for (const socket of sockets) socket.destroy();
    })
  };
}

export async function closeDashboardListeners({ redirectProxy, upstream, servers = [] }) {
  await Promise.all([
    redirectProxy?.close(),
    ...[upstream, ...servers].filter(Boolean).map(server => new Promise(resolve => {
      server.close(resolve);
      server.closeAllConnections?.();
    }))
  ]);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.umask(0o077);
  const servers = [], applySafety = server => { server.requestTimeout = 20000; server.headersTimeout = 10000; server.maxHeadersCount = 50; };
  let upstream = null, redirectProxy = null;
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
  recoverSettings(root);
  const settings = readJson(path.join(root, 'dashboard.json'));
  const keyFile = path.join(root, 'data', 'dashboard-access.txt');
  fs.mkdirSync(path.dirname(keyFile), { recursive: true, mode: 0o700 });
  if (!fs.existsSync(keyFile)) fs.writeFileSync(keyFile, randomBytes(24).toString('base64url') + '\n', { mode: 0o600 });
  const key = fs.readFileSync(keyFile, 'utf8').trim();
  const dashboard = createDashboard({ root, settings, key });
  const handlerFor = getPeer => (req, res) => dashboard.handler(req, res, getPeer(req));
  const knownHosts = new Set((settings.origins || []).map(origin => new URL(origin).host));
  const record = error => { if (!error || ['ECONNRESET', 'EPIPE', 'ERR_STREAM_PREMATURE_CLOSE'].includes(error.code)) return; console.error('dashboard socket error:', error.code || error.message); };
  if (settings.tls) {
    const tls = { key: fs.readFileSync(path.resolve(root, settings.tls.key)), cert: fs.readFileSync(path.resolve(root, settings.tls.cert)), minVersion: 'TLSv1.2' };
    const peerOf = req => ({ address: redirectProxy?.peerFor(req, req.socket.remoteAddress) || req.socket.remoteAddress });
    upstream = https.createServer(tls, handlerFor(peerOf));
    applySafety(upstream);
    upstream.listen(0, '127.0.0.1', () => {
      if (stopping) { upstream.close(); return; }
      redirectProxy = createHttpsRedirectProxy({ upstreamPort: upstream.address().port, knownHosts, onError: record });
      redirectProxy.server.listen(settings.port, settings.host);
    });
  } else {
    if (!['127.0.0.1', '::1'].includes(settings.host)) throw Error('HTTPS is required for a remote bind');
    const server = http.createServer(handlerFor(req => ({ address: req.socket.remoteAddress })));
    server.listen(settings.port, settings.host); servers.push(server);
  }
  if (settings.localPort) { const server = http.createServer(handlerFor(req => ({ address: req.socket.remoteAddress }))); server.listen(settings.localPort, '127.0.0.1'); servers.push(server); }
  for (const server of servers) applySafety(server);
  console.log('Dashboard listening. Retrieve the access key with ./agent dashboard-key.');
  let stopping = false;
  for (const sig of ['SIGTERM', 'SIGINT']) process.on(sig, () => {
    if (stopping) return;
    stopping = true;
    dashboard.close();
    void closeDashboardListeners({ redirectProxy, upstream, servers });
  });
}
