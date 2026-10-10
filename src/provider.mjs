import { setTimeout as sleep } from 'node:timers/promises';
import { callBudget } from './model-budget.mjs';

export class ProviderError extends Error {
  constructor(code) { super(code); this.name = 'ProviderError'; this.code = code; }
}
export function endpoint(base, kind) {
  const suffix = kind === 'anthropic' ? '/messages' : '/chat/completions';
  const b = base.replace(/\/+$/, '');
  if (b.endsWith(suffix)) return b;
  return b + (kind === 'anthropic' && !b.endsWith('/v1') ? '/v1' : '') + suffix;
}
export function parseObject(text) {
  const clean = text.trim().replace(/^```(?:json)?\s*/i, '').replace(/\s*```$/, '');
  let result;
  try { result = JSON.parse(clean); } catch { throw new ProviderError('invalid_json'); }
  if (!result || Array.isArray(result) || typeof result !== 'object') throw new ProviderError('invalid_json_object');
  return result;
}
export async function listModels(c, key, fetcher = globalThis.fetch) {
  if (!key) throw new ProviderError('save_api_key_first');
  const deepseek = new URL(c.baseUrl).hostname === 'api.deepseek.com';
  const url = deepseek ? 'https://api.deepseek.com/models' : endpoint(c.baseUrl, c.kind).replace(/\/(chat\/completions|messages)$/, '/models');
  const headers = c.kind === 'anthropic' && !deepseek
    ? { [c.anthropicAuth === 'bearer' ? 'Authorization' : 'x-api-key']: c.anthropicAuth === 'bearer' ? `Bearer ${key}` : key, 'anthropic-version': '2023-06-01' }
    : { Authorization: `Bearer ${key}` };
  if (c.kind === 'anthropic' && !deepseek && c.workspaceId) headers['anthropic-workspace-id'] = c.workspaceId;
  const response = await fetcher(url, { headers, redirect: 'error', signal: AbortSignal.timeout(15000) });
  if (!response.ok) { await response.body?.cancel(); throw new ProviderError(`models_http_${response.status}`); }
  const data = await response.json();
  if (!Array.isArray(data.data)) throw new ProviderError('invalid_model_list');
  return [...new Set(data.data.filter(m => typeof m?.id === 'string' && m.id.length <= 200).map(m => m.id))].slice(0, 500).sort();
}
export class Provider {
  constructor(config, key, budgetFile, options = {}) {
    this.config = config; this.key = key; this.budgetFile = budgetFile;
    this.fetch = options.fetch || globalThis.fetch; this.sleep = options.sleep || sleep;
    this.now = options.now || (() => Date.now() / 1000);
    this.blockedUntil = 0; this.calls = 0;
  }
  async complete(system, user, signal) {
    const c = this.config;
    if (this.now() < this.blockedUntil) throw new ProviderError('provider_backoff');
    const anthropic = c.kind === 'anthropic';
    const headers = { 'Content-Type': 'application/json' };
    let body;
    if (anthropic) {
      headers[c.anthropicAuth === 'bearer' ? 'Authorization' : 'x-api-key'] = c.anthropicAuth === 'bearer' ? `Bearer ${this.key}` : this.key;
      headers['anthropic-version'] = '2023-06-01';
      if (c.workspaceId) headers['anthropic-workspace-id'] = c.workspaceId;
      body = { model: c.model, max_tokens: c.maxTokens, system, messages: [{ role: 'user', content: user }] };
    } else {
      headers.Authorization = `Bearer ${this.key}`;
      body = { model: c.model, [c.tokenParameter]: c.maxTokens, messages: [{ role: 'system', content: system }, { role: 'user', content: user }] };
    }
    if (c.thinking === 'disabled') body.thinking = { type: 'disabled' };
    for (let attempt = 0; attempt <= c.retries; attempt++) {
      signal?.throwIfAborted();
      if (!callBudget(this.budgetFile, this.now(), c.requestsPerHour)) throw new ProviderError('hourly_api_budget');
      let retryDelay = Math.min(30, 2 ** attempt);
      try {
        this.calls++;
        const response = await this.fetch(endpoint(c.baseUrl, c.kind), { method: 'POST', headers, body: JSON.stringify(body), redirect: 'error',
          signal: signal ? AbortSignal.any([signal, AbortSignal.timeout(c.timeoutSeconds * 1000)]) : AbortSignal.timeout(c.timeoutSeconds * 1000) });
        if (!response.ok) {
          await response.body?.cancel();
          if ([401, 403, 400, 404, 422].includes(response.status)) {
            this.blockedUntil = this.now() + 300;
            throw new ProviderError(`http_${response.status}_check_provider_config`);
          }
          if (response.status !== 429 && response.status < 500) throw new ProviderError(`http_${response.status}`);
          const retryAfter = response.headers.get('retry-after');
          const parsed = Number(retryAfter) || (Date.parse(retryAfter) / 1000 - this.now());
          if (Number.isFinite(parsed)) retryDelay = Math.max(retryDelay, Math.min(60, parsed));
          throw new ProviderError('transient_http');
        }
        const text = await response.text();
        if (text.length > 1_000_000) throw new ProviderError('response_too_large');
        let data;
        try { data = JSON.parse(text); } catch { throw new ProviderError('invalid_provider_response'); }
        let content;
        if (anthropic) {
          if (data.stop_reason === 'max_tokens') throw new ProviderError('output_truncated_increase_maxTokens');
          content = Array.isArray(data.content) ? data.content.filter(x => x.type === 'text').map(x => x.text).join('\n') : null;
        } else {
          if (data.choices?.[0]?.finish_reason === 'length') throw new ProviderError('output_truncated_increase_maxTokens');
          content = data.choices?.[0]?.message?.content;
        }
        if (typeof content !== 'string' || !content.trim()) throw new ProviderError('empty_model_response');
        return content;
      } catch (error) {
        if (signal?.aborted) throw signal.reason;
        if (error instanceof ProviderError && error.code !== 'transient_http') throw error;
        if (attempt === c.retries) { this.blockedUntil = this.now() + 60; throw new ProviderError('provider_unavailable'); }
        await this.sleep(retryDelay * 1000, undefined, { signal });
      }
    }
  }
  async json(system, payload, signal) { return parseObject(await this.complete(system, JSON.stringify(payload), signal)); }
}
