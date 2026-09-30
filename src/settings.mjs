import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { defaults, merge, validate } from './config.mjs';

export function readJson(file, fallback = {}) { return fs.existsSync(file) ? JSON.parse(fs.readFileSync(file, 'utf8')) : fallback; }
export function revision(root) {
  return createHash('sha256').update(['config.json', 'secrets.json'].map(n => fs.existsSync(path.join(root, n)) ? fs.readFileSync(path.join(root, n), 'utf8') : '').join('\0')).digest('hex');
}
export function atomicJson(file, value) {
  const tmp = file + '.tmp';
  fs.writeFileSync(tmp, JSON.stringify(value, null, 2) + '\n', { mode: 0o600 });
  fs.chmodSync(tmp, 0o600); fs.renameSync(tmp, file);
}
export function knownConfig(value, shape = defaults, prefix = '') {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw Error('Expected a configuration object');
  for (const [k, v] of Object.entries(value)) {
    if (!Object.hasOwn(shape, k)) throw Error(`Unknown setting: ${prefix}${k}`);
    if (shape[k] && typeof shape[k] === 'object' && !Array.isArray(shape[k]) && v !== null) knownConfig(v, shape[k], prefix + k + '.');
  }
  return value;
}
export function publicSettings(root) {
  const config = validate(merge(defaults, readJson(path.join(root, 'config.json'))));
  // Never send unknown manually-added fields (which could contain credentials).
  function pick(value, shape) {
    return Object.fromEntries(Object.keys(shape).map(k => [k, shape[k] && typeof shape[k] === 'object' && !Array.isArray(shape[k]) && value[k] !== null ? pick(value[k], shape[k]) : value[k]]));
  }
  const secrets = readJson(path.join(root, 'secrets.json'));
  return { config: pick(config, defaults), revision: revision(root), hasApiKey: !!secrets.apiKey, hasOnebotToken: !!secrets.onebotToken };
}
export function recoverSettings(root) {
  const journal = path.join(root, '.settings-write');
  if (!fs.existsSync(journal)) return;
  const previous = readJson(journal);
  if (!previous || typeof previous !== 'object' || !previous.config || !previous.secrets) throw Error('Invalid settings recovery journal');
  atomicJson(path.join(root, 'secrets.json'), previous.secrets);
  atomicJson(path.join(root, 'config.json'), previous.config);
  fs.unlinkSync(journal);
}
export function saveSettings(root, payload) {
  if (payload.revision !== revision(root)) throw Object.assign(Error('Settings changed elsewhere. Reload before saving.'), { status: 409 });
  const config = validate(merge(defaults, knownConfig(payload.config)));
  const old = publicSettings(root);
  if (config.storage.directory !== old.config.storage.directory) throw Error('Moving the data directory requires stopping the service and editing the local config.');
  for (const k of ['name', 'persona']) if (typeof config.agent[k] !== 'string' || !config.agent[k].trim() || config.agent[k].length > 12000) throw Error(`Invalid agent.${k}`);
  if (typeof config.provider.model !== 'string' || config.provider.model.length > 200) throw Error('Invalid model');
  const secrets = readJson(path.join(root, 'secrets.json'));
  if (new URL(config.provider.baseUrl).host !== new URL(old.config.provider.baseUrl).host && secrets.apiKey && !payload.apiKey && !payload.clearApiKey) throw Error('Enter a key for the new provider, or clear the saved key, before changing provider host.');
  for (const k of ['apiKey', 'onebotToken']) {
    if (payload[k] != null && (typeof payload[k] !== 'string' || payload[k].length > 8192 || /[\r\n]/.test(payload[k]))) throw Error(`Invalid ${k}`);
    if (payload[k]?.trim()) secrets[k] = payload[k].trim();
  }
  if (payload.clearApiKey === true) delete secrets.apiKey;
  const lock = path.join(root, '.settings-write');
  atomicJson(lock, { config: readJson(path.join(root, 'config.json')), secrets: readJson(path.join(root, 'secrets.json')) });
  try { atomicJson(path.join(root, 'secrets.json'), secrets); atomicJson(path.join(root, 'config.json'), config); }
  catch (error) { recoverSettings(root); throw error; }
  fs.unlinkSync(lock);
  return publicSettings(root);
}
