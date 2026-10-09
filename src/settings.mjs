import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { defaults, merge, validate } from './config.mjs';
import { isDeepStrictEqual } from 'node:util';
import { withDefaults, editJson } from './settings-json.mjs';
import { execFileSync } from 'node:child_process';

// 动态白名单：优先用 Rust 内核导出的默认 schema（config-defaults 命令），
// 避免每次新增配置键都要手改 config.mjs 的 defaults。内核不可用时回退到 JS 静态 defaults。
let _rustSchema = null;
function schema(root) {
  if (_rustSchema) return _rustSchema;
  try {
    const bin = process.env.AGENT_CORE || path.join(root, 'rust', 'target', 'release', 'qq-inner-core');
    _rustSchema = JSON.parse(execFileSync(bin, ['config-defaults'], { timeout: 5000 }).toString('utf8'));
  } catch {
    _rustSchema = defaults;
  }
  return _rustSchema;
}

export function readJson(file, fallback = {}) { return fs.existsSync(file) ? JSON.parse(fs.readFileSync(file, 'utf8')) : fallback; }
function snapshot(root) {
  const texts = ['config', 'secrets'].map(name => {
    const file = path.join(root, name + '.json');
    return fs.existsSync(file) ? fs.readFileSync(file, 'utf8') : null;
  });
  return { configText: texts[0], secretsText: texts[1], revision: createHash('sha256').update(texts.map(t => t ?? '').join('\0')).digest('hex') };
}
export function revision(root) { return snapshot(root).revision; }
function atomicText(file, text) {
  const tmp = file + '.tmp';
  fs.writeFileSync(tmp, text, { mode: 0o600 });
  fs.chmodSync(tmp, 0o600); fs.renameSync(tmp, file);
}
export function atomicJson(file, value) { atomicText(file, JSON.stringify(value, null, 2) + '\n'); }
export function knownConfig(value, shape = defaults, prefix = '') {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw Error('Expected a configuration object');
  for (const [k, v] of Object.entries(value)) {
    if (!Object.hasOwn(shape, k)) throw Error(`Unknown setting: ${prefix}${k}`);
    if (shape[k] && typeof shape[k] === 'object' && !Array.isArray(shape[k]) && v !== null) knownConfig(v, shape[k], prefix + k + '.');
  }
  return value;
}
// Bounded baselines: an expired/unknown revision fails closed with 409.
const history = new Map();
const historyKey = (root, rev) => path.resolve(root) + ':' + rev;
const fingerprint = value => createHash('sha256').update(JSON.stringify(value ?? null)).digest('hex');
const get = (value, key) => key.split('.').reduce((o, k) => o?.[k], value);
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
function put(value, key, next) {
  const parts = key.split('.'), end = parts.pop();
  const parent = parts.reduce((o, k) => o[k] ??= {}, value);
  parent[end] = object(parent[end]) && object(next) ? merge(parent[end], next) : structuredClone(next);
}
export function publicSettings(root, baselines = history, current = snapshot(root)) {
  const shape = schema(root);
  const config = validate(withDefaults(JSON.parse(current.configText ?? '{}'), shape));
  // Never send unknown manually-added fields (which could contain credentials).
  function pick(value, shape) {
    return Object.fromEntries(Object.keys(shape).map(k => [k, shape[k] && typeof shape[k] === 'object' && !Array.isArray(shape[k]) && value[k] !== null ? pick(value[k], shape[k]) : value[k]]));
  }
  const secrets = JSON.parse(current.secretsText ?? '{}');
  const result = { config: pick(config, shape), revision: current.revision, hasApiKey: !!secrets.apiKey, hasOnebotToken: !!secrets.onebotToken };
  const key = historyKey(root, result.revision);
  if (!baselines.has(key)) baselines.set(key, { config: structuredClone(result.config), apiKey: fingerprint(secrets.apiKey), onebotToken: fingerprint(secrets.onebotToken) });
  while (baselines.size > 256) baselines.delete(baselines.keys().next().value);
  return result;
}
export function recoverSettings(root) {
  const journal = path.join(root, '.settings-write');
  if (!fs.existsSync(journal)) return;
  const previous = readJson(journal);
  if (!previous || typeof previous !== 'object' || !previous.config || !previous.secrets) throw Error('Invalid settings recovery journal');
  for (const name of ['secrets', 'config']) {
    const file = path.join(root, name + '.json');
    if (previous[name + 'Text'] === null) { if (fs.existsSync(file)) fs.unlinkSync(file); }
    else if (typeof previous[name + 'Text'] === 'string') atomicText(file, previous[name + 'Text']);
    else atomicJson(file, previous[name]); // Compatible with older recovery journals.
  }
  fs.unlinkSync(journal);
}
export function saveSettings(root, payload, baselines = history) {
  if (!payload || !object(payload.patch) || Object.hasOwn(payload, 'config')) throw Error('Expected a settings patch');
  const shape = schema(root), entries = Object.entries(payload.patch);
  for (const [key, value] of entries) {
    let field = shape;
    for (const part of key.split('.')) {
      if (['__proto__', 'prototype', 'constructor'].includes(part) || !object(field) || !Object.hasOwn(field, part)) throw Error(`Unknown setting: ${key}`);
      field = field[part];
    }
    if (object(field) && value !== null) knownConfig(value, field, key + '.');
    if (entries.some(([other]) => other !== key && other.startsWith(key + '.'))) throw Error(`Overlapping setting: ${key}`);
  }
  const current = snapshot(root);
  const old = publicSettings(root, baselines, current);
  const secrets = JSON.parse(current.secretsText ?? '{}');
  const conflict = () => { throw Object.assign(Error('Settings changed elsewhere. Reload before saving.'), { status: 409 }); };
  if (payload.revision !== old.revision) {
    const base = baselines.get(historyKey(root, payload.revision));
    if (!base || entries.some(([key]) => !isDeepStrictEqual(get(base.config, key), get(old.config, key)))) conflict();
    for (const key of ['apiKey', 'onebotToken']) {
      if ((payload[key] || (key === 'apiKey' && payload.clearApiKey)) && base[key] !== fingerprint(secrets[key])) conflict();
    }
    // A replacement key must never be attached to a concurrently changed host.
    if ((payload.apiKey || payload.clearApiKey) && new URL(base.config.provider.baseUrl).host !== new URL(old.config.provider.baseUrl).host) conflict();
  }
  const configFile = path.join(root, 'config.json'), secretsFile = path.join(root, 'secrets.json');
  const { configText, secretsText } = current;
  const config = withDefaults(JSON.parse(configText ?? '{}'), shape);
  for (const [key, value] of entries) put(config, key, value);
  // Validation normalizes some legacy fields. Persist normalization only on
  // explicitly patched paths, so unrelated legacy values remain byte-identical.
  const checked = validate(structuredClone(config));
  for (const [key] of entries) put(config, key, get(checked, key));
  if (config.storage.directory !== old.config.storage.directory) throw Error('Moving the data directory requires stopping the service and editing the local config.');
  for (const k of ['name', 'persona']) if (typeof config.agent[k] !== 'string' || !config.agent[k].trim() || config.agent[k].length > 12000) throw Error(`Invalid agent.${k}`);
  if (typeof config.provider.model !== 'string' || config.provider.model.length > 200) throw Error('Invalid model');
  if (new URL(config.provider.baseUrl).host !== new URL(old.config.provider.baseUrl).host && secrets.apiKey && !(typeof payload.apiKey === 'string' && payload.apiKey.trim()) && payload.clearApiKey !== true) throw Error('Enter a key for the new provider, or clear the saved key, before changing provider host.');
  for (const k of ['apiKey', 'onebotToken']) {
    if (payload[k] != null && (typeof payload[k] !== 'string' || payload[k].length > 8192 || /[\r\n]/.test(payload[k]))) throw Error(`Invalid ${k}`);
    if (payload[k]?.trim()) secrets[k] = payload[k].trim();
  }
  if (payload.clearApiKey === true) delete secrets.apiKey;
  const lock = path.join(root, '.settings-write');
  const nextText = editJson(configText ?? '{}\n', config);
  if (revision(root) !== current.revision) conflict();
  atomicJson(lock, { config: JSON.parse(configText ?? '{}'), secrets: JSON.parse(secretsText ?? '{}'), configText, secretsText });
  try {
    if (!isDeepStrictEqual(secrets, JSON.parse(secretsText ?? '{}'))) atomicJson(secretsFile, secrets);
    if (nextText !== configText) atomicText(configFile, nextText);
  }
  catch (error) { recoverSettings(root); throw error; }
  fs.unlinkSync(lock);
  return publicSettings(root, baselines);
}
