import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import https from 'node:https';
import { randomBytes, createHash, timingSafeEqual } from 'node:crypto';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
import { DatabaseSync } from 'node:sqlite';
import { loadConfig } from './config.mjs';
import { publicSettings, saveSettings, readJson, recoverSettings } from './settings.mjs';
import { Provider } from './provider.mjs';
import { Store } from './store.mjs';
import { OneBot } from './onebot.mjs';

const exec = promisify(execFile), hash = s => createHash('sha256').update(s).digest();
const equal = (a, b) => timingSafeEqual(hash(String(a)), hash(String(b)));
const fail = (status, message) => Object.assign(Error(message), { status });
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
export function createDashboard({ root, settings, key, serviceControl, serviceStatus }) {
  const sessions = new Map(), attempts = new Map(); let saving = false, testing = false;
  const origins = new Set(settings.origins), hosts = new Set([...origins].map(o => new URL(o).host));
  serviceControl ||= async action => { await exec('systemctl', ['--user', action, 'qq-inner-agent.service'], { timeout: 30000 }); };
  serviceStatus ||= async () => {
    try { return (await exec('systemctl', ['--user', 'show', 'qq-inner-agent.service', '-p', 'ActiveState', '--value'], { timeout: 2000 })).stdout.trim(); }
    catch { return 'unknown'; }
  };
  function snapshot() {
    const c = loadConfig(root), file = path.join(c.dataDir, 'agent.sqlite');
    const status = readJson(path.join(c.dataDir, 'status.json'), null);
    let decisions = [], thoughts = [];
    if (fs.existsSync(file)) {
      let db;
      try {
        db = new DatabaseSync(file, { readOnly: true });
        decisions = db.prepare('SELECT chat,ts,action,score,tags FROM decisions ORDER BY ts DESC LIMIT 30').all();
        thoughts = db.prepare('SELECT chat,text,kind,score,created FROM thoughts WHERE used=0 AND created>? ORDER BY created DESC LIMIT 12').all(Date.now()/1000 - c.agent.thoughtTtlSeconds);
      } catch { /* database may be opening for the first time */ }
      finally { db?.close(); }
    }
    const data = { status, decisions, thoughts, logs: tail(path.join(c.dataDir, 'agent.log')), savedRevision: publicSettings(root).revision };
    let text = JSON.stringify(data);
    for (const secret of [c.apiKey, c.onebotToken, key].filter(Boolean)) text = text.split(secret).join('[redacted]');
    return JSON.parse(text);
  }
  const handler = async (req, res) => {
    const secure = !!req.socket.encrypted;
    res.setHeader('Cache-Control', 'no-store');
    res.setHeader('X-Content-Type-Options', 'nosniff');
    res.setHeader('Referrer-Policy', 'no-referrer');
    res.setHeader('X-Frame-Options', 'DENY');
    res.setHeader('Content-Security-Policy', "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'");
    try {
      if (!hosts.has(req.headers.host)) throw fail(403, 'Unknown dashboard address');
      const url = new URL(req.url, 'http://localhost');
      if (req.method === 'GET' && ['/', '/app.js', '/style.css', '/favicon.svg'].includes(url.pathname)) {
        const name = url.pathname === '/' ? 'index.html' : url.pathname.slice(1);
        const type = { 'index.html': 'text/html; charset=utf-8', 'app.js': 'text/javascript; charset=utf-8', 'style.css': 'text/css; charset=utf-8', 'favicon.svg': 'image/svg+xml' }[name];
        res.writeHead(200, { 'Content-Type': type }); res.end(fs.readFileSync(path.join(root, 'web', name))); return;
      }
      const origin = req.headers.origin;
      if (req.method !== 'GET' && (!origin || !origins.has(origin) || new URL(origin).host !== req.headers.host)) throw fail(403, 'Origin rejected');
      if (url.pathname === '/api/login' && req.method === 'POST') {
        const ip = req.socket.remoteAddress, now = Date.now();
        for (const [id, value] of attempts) if (value.until < now) attempts.delete(id);
        const attempt = attempts.get(ip) || { count: 0, until: now + 600_000 };
        if (attempt.count >= 8 || attempts.size > 1000) throw fail(429, 'Too many attempts. Try again in ten minutes.');
        const input = await body(req);
        if (!equal(input.key || '', key)) { attempt.count++; attempts.set(ip, attempt); throw fail(401, 'Incorrect access key'); }
        attempts.delete(ip);
        for (const [id, s] of sessions) if (s.expires < now) sessions.delete(id);
        if (sessions.size >= 20) sessions.delete(sessions.keys().next().value);
        const id = randomBytes(32).toString('hex'), csrf = randomBytes(24).toString('hex');
        sessions.set(id, { csrf, expires: now + 12 * 3600_000 });
        res.setHeader('Set-Cookie', `qia_session=${id}; HttpOnly; SameSite=Strict; Path=/; Max-Age=43200${secure ? '; Secure' : ''}`);
        json(res, 200, { csrf }); return;
      }
      const id = String(req.headers.cookie || '').split(';').map(x => x.trim()).find(x => x.startsWith('qia_session='))?.slice(12);
      const session = sessions.get(id);
      if (!session || session.expires < Date.now()) { if (id) sessions.delete(id); throw fail(401, 'Sign in required'); }
      if (req.method !== 'GET' && !equal(req.headers['x-csrf-token'] || '', session.csrf)) throw fail(403, 'Session verification failed; sign in again.');
      if (url.pathname === '/api/session' && req.method === 'GET') { json(res, 200, { csrf: session.csrf }); return; }
      if (url.pathname === '/api/logout' && req.method === 'POST') {
        sessions.delete(id); res.setHeader('Set-Cookie', `qia_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0${secure ? '; Secure' : ''}`); json(res, 200, { ok: true }); return;
      }
      if (url.pathname === '/api/config' && req.method === 'GET') { json(res, 200, publicSettings(root)); return; }
      if (url.pathname === '/api/config' && req.method === 'PUT') {
        if (saving) throw fail(409, 'A save is already in progress');
        const input = await body(req); saving = true;
        try { json(res, 200, saveSettings(root, input)); }
        catch (e) { throw fail(e.status || 400, e.message); }
        finally { saving = false; }
        return;
      }
      if (url.pathname === '/api/state' && req.method === 'GET') { json(res, 200, { ...snapshot(), serviceState: await serviceStatus() }); return; }
      if (url.pathname === '/api/service' && req.method === 'POST') {
        const input = await body(req);
        if (!['start', 'stop', 'restart'].includes(input.action)) throw fail(400, 'Unknown service action');
        await serviceControl(input.action); json(res, 200, { serviceState: await serviceStatus() }); return;
      }
      if (url.pathname === '/api/test-model' && req.method === 'POST') {
        if (testing) throw fail(409, 'A connection test is in progress');
        testing = true; let store;
        try {
          const c = loadConfig(root); if (!c.apiKey || !c.provider.model) throw fail(400, 'Save an API key and model first');
          store = new Store(path.join(c.dataDir, 'agent.sqlite'));
          const p = new Provider(c.provider, c.apiKey, store);
          const response = await p.json('Return only {"ok":true}.', { test: 'API connectivity only; no QQ content' });
          if (response.ok !== true) throw fail(502, 'Unexpected model response');
          json(res, 200, { ok: true, message: 'API authentication and JSON response verified. No QQ message sent.' });
        } catch (e) { throw fail(e.status || 502, e.code || 'Model test failed'); }
        finally { store?.close(); testing = false; }
        return;
      }
      if (url.pathname === '/api/contacts' && req.method === 'GET') {
        const c = loadConfig(root), bot = new OneBot(c.onebot, c.onebotToken), stop = new AbortController();
        let timer;
        const ready = new Promise((resolve, reject) => {
          bot.on('status', s => { if (s === 'connected') resolve(); });
          timer = setTimeout(() => reject(fail(503, 'QQ is not online. Connect it in SnowLuma first.')), 15000);
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
  return { handler, snapshot };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.umask(0o077);
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
  recoverSettings(root);
  const settings = readJson(path.join(root, 'dashboard.json'));
  const keyFile = path.join(root, 'data', 'dashboard-access.txt');
  fs.mkdirSync(path.dirname(keyFile), { recursive: true, mode: 0o700 });
  if (!fs.existsSync(keyFile)) fs.writeFileSync(keyFile, randomBytes(24).toString('base64url') + '\n', { mode: 0o600 });
  const key = fs.readFileSync(keyFile, 'utf8').trim();
  const { handler } = createDashboard({ root, settings, key });
  const servers = [];
  if (settings.tls) {
    const server = https.createServer({ key: fs.readFileSync(path.resolve(root, settings.tls.key)), cert: fs.readFileSync(path.resolve(root, settings.tls.cert)), minVersion: 'TLSv1.2' }, handler);
    server.listen(settings.port, settings.host); servers.push(server);
  } else {
    if (!['127.0.0.1', '::1'].includes(settings.host)) throw Error('HTTPS is required for a remote bind');
    const server = http.createServer(handler); server.listen(settings.port, settings.host); servers.push(server);
  }
  if (settings.localPort) { const server = http.createServer(handler); server.listen(settings.localPort, '127.0.0.1'); servers.push(server); }
  for (const server of servers) { server.requestTimeout = 20000; server.headersTimeout = 10000; server.maxHeadersCount = 50; }
  console.log('Dashboard listening. Retrieve the access key with ./agent dashboard-key.');
  for (const sig of ['SIGTERM', 'SIGINT']) process.on(sig, () => { for (const s of servers) { s.close(); s.closeAllConnections(); } });
}
