import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { loadConfig, readiness } from './config.mjs';
import { Provider } from './provider.mjs';
import { OneBot } from './onebot.mjs';
import { ControlClient } from './control.mjs';
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
process.umask(0o077);
const c = loadConfig(root), action = process.argv[2];
if (action !== 'core-status') fs.mkdirSync(c.dataDir, { recursive: true, mode: 0o700 });
if (action === 'core-status') {
  const client = new ControlClient(c.dataDir);
  try {
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(Error('control_unavailable')), 2000);
      client.on('available', available => { if (available) { clearTimeout(timer); resolve(); } });
    });
    console.log(JSON.stringify({ available: true, status: await client.request('state.get') }, null, 2));
  } catch (e) { console.log(JSON.stringify({ available: client.available, error: e.code || e.message })); process.exitCode = 1; }
  finally { client.close(); }
} else if (action === 'add-memory') {
  console.error('Use ./agent add-memory CHAT TEXT; owner notes are managed by the Rust core.');
  process.exitCode = 1;
} else {
  const bot = new OneBot(c.onebot, c.onebotToken), controller = new AbortController();
  let timeout, connectedResolve;
  const ready = new Promise((resolve, reject) => {
    connectedResolve = resolve;
    timeout = setTimeout(() => reject(Error('onebot_connection_timeout')), 20_000);
  });
  bot.on('status', s => { if (['connected', 'qq_offline'].includes(s)) connectedResolve(); });
  const task = bot.start(controller.signal);
  try {
    await ready;
    console.log(`OneBot connected; QQ online: ${bot.online}; account: ${bot.selfId}`);
    if (action === 'contacts') {
      for (const [label, api, id, name] of [['Groups', 'get_group_list', 'group_id', 'group_name'], ['Friends', 'get_friend_list', 'user_id', 'nickname']]) {
        const list = await bot.call(api, {});
        console.log(label + ':');
        for (const x of Array.isArray(list) ? list : []) console.log(`${x[id]}\t${String(x[name] || '').replace(/[\x00-\x1f\x7f-\x9f]/g, '')}`);
      }
    } else {
      console.log('Setup missing: ' + (readiness(c).join(', ') || 'nothing'));
      if (process.argv.includes('--api')) {
        if (!c.apiKey || !c.provider.model) throw Error('API_key_or_model_missing_run_setup');
        const p = new Provider(c.provider, c.apiKey, path.join(c.dataDir, 'agent.sqlite'));
        const result = await p.json('只返回 JSON：{"ok":true}。', { test: '仅测试连通性，不包含 QQ 消息或历史记录' }, controller.signal);
        if (result.ok !== true) throw Error('unexpected_model_response');
        console.log('Model API authentication and JSON response verified. No QQ message sent.');
      }
    }
  } catch (e) { console.error('Check failed: ' + (e.code || 'connection_or_configuration_error')); process.exitCode = 1; }
  finally { clearTimeout(timeout); controller.abort(); await task; }
}
