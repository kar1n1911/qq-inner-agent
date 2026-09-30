import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { loadConfig, readiness } from './config.mjs';
import { Store } from './store.mjs';
import { Provider } from './provider.mjs';
import { OneBot } from './onebot.mjs';
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
process.umask(0o077);
const c = loadConfig(root), action = process.argv[2];
fs.mkdirSync(c.dataDir, { recursive: true, mode: 0o700 });
if (action === 'add-memory') {
  const [chat, ...words] = process.argv.slice(3);
  if (!/^(group|private):[1-9]\d*$/.test(chat || '') || !words.length) throw Error('Usage: ./agent add-memory group:123 "A short factual note"');
  const store = new Store(path.join(c.dataDir, 'agent.sqlite'));
  store.note(chat, words.join(' ').slice(0, 2000), Date.now() / 1000); store.close();
  console.log('Saved a note scoped to that chat.');
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
        const store = new Store(path.join(c.dataDir, 'agent.sqlite'));
        try {
          const p = new Provider(c.provider, c.apiKey, store);
          const result = await p.json('Return JSON only: {"ok":true}.', { test: 'connectivity only; no QQ messages or history' }, controller.signal);
          if (result.ok !== true) throw Error('unexpected_model_response');
          console.log('Model API authentication and JSON response verified. No QQ message sent.');
        } finally { store.close(); }
      }
    }
  } catch (e) { console.error('Check failed: ' + (e.code || 'connection_or_configuration_error')); process.exitCode = 1; }
  finally { clearTimeout(timeout); controller.abort(); await task; }
}
