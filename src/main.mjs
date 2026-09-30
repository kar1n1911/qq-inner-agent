import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { loadConfig, readiness } from './config.mjs';
import { Store } from './store.mjs';
import { Provider } from './provider.mjs';
import { OneBot } from './onebot.mjs';
import { Engine } from './engine.mjs';
import { revision } from './settings.mjs';
import { allowed, activeAt } from './policy.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
process.umask(0o077);
const log = (event, data = {}) => {
  const line = JSON.stringify({ time: new Date().toISOString(), event, ...data });
  console.log(line);
  if (config?.dataDir) {
    const file = path.join(config.dataDir, 'agent.log');
    try {
      if (fs.existsSync(file) && fs.statSync(file).size > 1_048_576) fs.renameSync(file, file + '.1');
      fs.appendFileSync(file, line + '\n', { mode: 0o600 });
    } catch { /* journald still receives the event */ }
  }
};
let config;
try { if (fs.existsSync(path.join(root, '.settings-write'))) throw Error('Pending settings recovery'); config = loadConfig(root); }
catch (e) { console.error('Invalid configuration. Run ./agent setup to correct it.'); process.exit(2); }
fs.mkdirSync(config.dataDir, { recursive: true, mode: 0o700 });
const store = new Store(path.join(config.dataDir, 'agent.sqlite'));
store.recoverDeliveries();
let provider = new Provider(config.provider, config.apiKey, store);
let bot = new OneBot(config.onebot, config.onebotToken);
let engine = new Engine(config, store, provider, bot, { log });
let abort = new AbortController();
let connection, reloading = false, shuttingDown = false;
let appliedRevision = revision(root), reloadError = null;
function connect() {
  bot.on('status', state => log('onebot', { state }));
  bot.on('event', event => { try { engine.ingest(event); } catch { log('event_rejected'); } });
  connection = bot.start(abort.signal).catch(() => { log('connection_loop_failed'); stopResolve(); });
}
engine.restore();
log('started', { mode: readiness(config).length ? 'waiting_for_setup' : config.agent.dryRun ? 'dry_run' : 'active', missing: readiness(config),
  provider: config.provider.kind, model: config.provider.model, selectedChats: config.agent.allowedGroups.length + config.agent.allowedUsers.length });
function status() {
  const missing = readiness(config);
  const data = { updatedAt: new Date().toISOString(), pid: process.pid, mode: missing.length ? 'waiting_for_setup' : config.agent.dryRun ? 'dry_run' : 'active',
    appliedRevision, reloading, reloadError,
    scheduleActive: activeAt(Date.now() / 1000, config.agent.schedule),
    missing, onebotConnected: bot.connected, qqOnline: bot.online, selfId: bot.selfId,
    reconnects: bot.reconnects, activeChats: engine.chats.size, model: config.provider.model,
    provider: config.provider.kind, apiCallsThisRun: provider.calls, lastCycleAt: engine.lastCycle,
    lastError: engine.lastError };
  const tmp = path.join(config.dataDir, 'status.json.tmp');
  fs.writeFileSync(tmp, JSON.stringify(data, null, 2), { mode: 0o600 });
  fs.renameSync(tmp, path.join(config.dataDir, 'status.json'));
}
const tick = setInterval(() => { if (!reloading && !shuttingDown) engine.tick(); }, 1000);
const report = setInterval(status, 5000);
const prune = () => store.prune(Date.now() / 1000, config.storage.retentionDays, config.storage.maxMessagesPerChat);
prune(); const cleanup = setInterval(prune, 3600_000);
status();
let stopResolve;
const stopped = new Promise(r => { stopResolve = r; });
for (const name of ['SIGINT', 'SIGTERM', 'SIGHUP']) process.once(name, () => { shuttingDown = true; abort.abort(); stopResolve(); });
connect();
let reloadTask = Promise.resolve();
const watcher = setInterval(() => {
  if (reloading || shuttingDown || fs.existsSync(path.join(root, '.settings-write'))) return;
  const nextRevision = revision(root);
  if (nextRevision === appliedRevision) return;
  reloading = true;
  reloadTask = (async () => {
    try {
      const next = loadConfig(root);
      if (next.dataDir !== config.dataDir) throw Error('data_directory_change');
      const reconnect = JSON.stringify(next.onebot) !== JSON.stringify(config.onebot) || next.onebotToken !== config.onebotToken;
      await engine.stop();
      if (shuttingDown) return;
      const chats = engine.chats;
      if (reconnect) { abort.abort(); await connection; if (shuttingDown) return; abort = new AbortController(); bot = new OneBot(next.onebot, next.onebotToken); }
      config = next; provider = new Provider(config.provider, config.apiKey, store);
      engine = new Engine(config, store, provider, bot, { log });
      for (const [chat, state] of chats) if (allowed(chat, config.agent)) engine.chats.set(chat, { ...state, busy: false, lastThink: 0 });
      if (reconnect) connect();
      appliedRevision = nextRevision; reloadError = null;
      log('config_applied', { revision: appliedRevision.slice(0, 12) });
    } catch { reloadError = 'Invalid configuration; previous settings remain active.'; log('config_reload_rejected'); }
    finally { reloading = false; status(); }
  })();
}, 1000);
await stopped;
clearInterval(tick); clearInterval(report); clearInterval(cleanup); clearInterval(watcher);
abort.abort(); await reloadTask; await engine.stop(); await connection;
status(); store.close(); log('stopped');
