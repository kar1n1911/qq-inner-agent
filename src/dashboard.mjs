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
import { Store } from './store.mjs';
import { OneBot } from './onebot.mjs';
import { Diagnostics } from './diagnostics.mjs';

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
export function createDashboard({ root, settings, key, serviceControl, serviceStatus, makeBot }) {
  const diagnostics = new Diagnostics(() => loadConfig(root), makeBot);
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
    let decisions = [], thoughts = [], assessments = [], learning = [], memories = [], observations = [];
    if (fs.existsSync(file)) {
      let db;
      try {
        db = new DatabaseSync(file, { readOnly: true });
        decisions = db.prepare('SELECT chat,ts,action,score,tags FROM decisions ORDER BY ts DESC LIMIT 30').all();
        thoughts = db.prepare('SELECT chat,text,kind,score,created FROM thoughts WHERE used=0 AND created>? ORDER BY created DESC LIMIT 12').all(Date.now()/1000 - c.agent.thoughtTtlSeconds);
        assessments = db.prepare('SELECT chat,ts,status,details FROM send_assessments ORDER BY ts DESC LIMIT 12').all().map(r => ({ ...r, details: JSON.parse(r.details) }));
        memories = db.prepare("SELECT * FROM memory_layers WHERE expires>? ORDER BY CASE layer WHEN 'long_term' THEN 0 WHEN 'traits' THEN 1 ELSE 2 END,updated DESC LIMIT 200").all(Date.now()/1000);
        learning = [...new Set(memories.map(m => m.chat))].map(chat => ({ chat }));
        if (db.prepare("SELECT name FROM sqlite_master WHERE name='group_orientation'").get()) observations = db.prepare('SELECT chat,started,message_count,status,sources,analysis,retry_at,error FROM group_orientation ORDER BY started DESC LIMIT 100').all().map(r => ({ ...r, sources: JSON.parse(r.sources), analysis: JSON.parse(r.analysis) }));
      } catch { /* database may be opening for the first time */ }
      finally { db?.close(); }
    }
    const data = { status, decisions, thoughts, assessments, learning, memories, observations, logs: tail(path.join(c.dataDir, 'agent.log')), savedRevision: publicSettings(root).revision };
    let text = JSON.stringify(data);
    for (const secret of [c.apiKey, c.onebotToken, key].filter(Boolean)) text = text.split(secret).join('[redacted]');
    return JSON.parse(text);
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
        sessions.set(id, { csrf, expires: now + 12 * 3600_000 });
        res.setHeader('Set-Cookie', `qia_session=${id}; HttpOnly; SameSite=Strict; Path=/; Max-Age=43200${secure ? '; Secure' : ''}`);
        json(res, 200, { csrf }); return;
      }
      const id = String(req.headers.cookie || '').split(';').map(x => x.trim()).find(x => x.startsWith('qia_session='))?.slice(12);
      const session = sessions.get(id);
      if (!session || session.expires < Date.now()) { if (id) sessions.delete(id); throw fail(401, 'Sign in required'); }
      if (req.method !== 'GET' && !equal(req.headers['x-csrf-token'] || '', session.csrf)) throw fail(403, 'Session verification failed; sign in again.');
      if (url.pathname === '/api/session' && req.method === 'GET') { json(res, 200, { csrf: session.csrf }); return; }
      if (url.pathname === '/api/models' && req.method === 'POST') {
        try { const c = loadConfig(root); json(res, 200, { models: await listModels(c.provider, c.apiKey) }); }
        catch (e) { json(res, 502, { error: e instanceof Error && e.code ? e.code : 'model_list_unavailable_use_manual_entry' }); }
        return;
      }
      if (url.pathname === '/api/debug/receive' && req.method === 'GET') { json(res, 200, diagnostics.status()); return; }
      if (req.method === 'POST' && ['/api/debug/send', '/api/debug/receive', '/api/debug/stop'].includes(url.pathname)) {
        try {
          const result = url.pathname === '/api/debug/send' ? await diagnostics.send()
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
      if (url.pathname === '/api/learning/reset' && req.method === 'POST') {
        const input = await body(req);
        if (typeof input.chat !== 'string' || !/^(group|private):[1-9]\d{0,19}$/.test(input.chat)) throw fail(400, 'Invalid chat');
        if (input.subject !== undefined && !(input.subject === 'group' && input.chat.startsWith('group:')) &&
            !(typeof input.subject === 'string' && /^person:[1-9]\d{0,19}$/.test(input.subject) &&
              (input.chat.startsWith('group:') || input.subject.slice(7) === input.chat.slice(8)))) throw fail(400, 'Invalid memory subject');
        const c = loadConfig(root); fs.mkdirSync(c.dataDir, { recursive: true, mode: 0o700 });
        const store = new Store(path.join(c.dataDir, 'agent.sqlite'));
        try { store.resetLearning(input.chat, Date.now()/1000, input.subject ?? null); } finally { store.close(); }
        json(res, 200, { ok: true }); return;
      }
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
          const response = await p.json('只返回 JSON：{"ok":true}。', { test: '仅测试 API 连通性，不包含 QQ 聊天内容' });
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
  return { handler, snapshot, close: () => diagnostics.stop() };
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
