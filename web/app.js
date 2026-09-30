const $ = id => document.getElementById(id);
let csrf = '', saved = null, dirty = false, polling = false, logs = [], online = false;
const get = (obj, key) => key.split('.').reduce((o, k) => o?.[k], obj);
const set = (obj, key, value) => { const parts = key.split('.'); const end = parts.pop(); parts.reduce((o,k) => o[k], obj)[end] = value; };
const ids = value => [...new Set(value.split(/[\s,]+/).filter(Boolean))];
function notice(text, error = false) { $('notice').textContent = text; $('notice').classList.toggle('error', error); $('notice').hidden = !text; }
function signedOut() { dirty = false; csrf = ''; online = false; $('console').hidden = true; $('login').hidden = false; }
async function api(url, options = {}) {
  const response = await fetch(url, { credentials: 'same-origin', ...options,
    headers: { ...(options.body ? { 'Content-Type': 'application/json' } : {}), ...(csrf ? { 'X-CSRF-Token': csrf } : {}), ...options.headers } });
  const data = await response.json();
  if (!response.ok) { if (response.status === 401) signedOut(); throw Error(data.error || 'Request failed'); }
  return data;
}
function changed() { dirty = true; $('dirty-dot').hidden = false; $('apply-status').textContent = 'Unsaved changes'; }
function page(name) {
  for (const p of document.querySelectorAll('.page')) p.hidden = p.id !== name;
  for (const b of document.querySelectorAll('.nav')) b.classList.toggle('active', b.dataset.page === name);
  $('page-title').textContent = { overview: 'Overview', config: 'Configuration', activity: 'Activity & logs' }[name];
}
function populate(data) {
  saved = data;
  for (const input of document.querySelectorAll('[data-config]')) {
    const value = get(data.config, input.dataset.config);
    if (input.type === 'checkbox') input.checked = value;
    else input.value = input.dataset.type === 'ids' ? value.join(', ') : value;
  }
  const q = data.config.agent.quietHours;
  $('quiet-enabled').checked = !!q; $('quiet-start').value = q?.start ?? 23;
  $('quiet-end').value = q?.end ?? 8; $('quiet-timezone').value = q?.timezone || 'Europe/Stockholm';
  $('key-status').textContent = data.hasApiKey ? 'Saved securely' : 'Not configured';
  $('api-key').value = ''; $('onebot-token').value = ''; $('clear-api-key').checked = false;
  $('advanced-json').value = JSON.stringify(data.config, null, 2);
  $('use-advanced').checked = false; $('threshold-output').textContent = data.config.agent.threshold.toFixed(2);
  dirty = false; $('dirty-dot').hidden = true;
}
function formConfig() {
  if ($('use-advanced').checked) return JSON.parse($('advanced-json').value);
  const c = structuredClone(saved.config);
  for (const input of document.querySelectorAll('[data-config]')) {
    let value = input.type === 'checkbox' ? input.checked : input.dataset.type === 'ids' ? ids(input.value) : ['number', 'range'].includes(input.type) ? Number(input.value) : input.value;
    set(c, input.dataset.config, value);
  }
  if (c.agent.name !== saved.config.agent.name) c.agent.aliases = [...new Set([c.agent.name, ...c.agent.aliases])];
  c.agent.quietHours = $('quiet-enabled').checked ? { start: Number($('quiet-start').value), end: Number($('quiet-end').value), timezone: $('quiet-timezone').value } : null;
  return c;
}
function time(ts) { return new Date(ts * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' }); }
function element(tag, text, cls) { const e = document.createElement(tag); if (text != null) e.textContent = text; if (cls) e.className = cls; return e; }
function renderLogs() {
  if ($('pause-logs').checked) return;
  const filter = $('log-filter').value.toLowerCase();
  const lines = logs.map(x => `${new Date(x.time).toLocaleTimeString()}  ${x.event}  ${JSON.stringify(Object.fromEntries(Object.entries(x).filter(([k]) => !['time','event'].includes(k))))}`).filter(x => x.toLowerCase().includes(filter));
  const output = $('log-output'), atBottom = output.scrollHeight - output.scrollTop - output.clientHeight < 40;
  output.textContent = lines.join('\n') || 'No matching events yet.';
  if (atBottom) output.scrollTop = output.scrollHeight;
}
async function refresh() {
  if (!csrf || polling) return; polling = true;
  try {
    const state = await api('/api/state'), s = state.status;
    online = true; $('dashboard-connection').textContent = 'Live connection';
    const fresh = s && Date.now() - Date.parse(s.updatedAt) < 20000;
    $('service-value').textContent = state.serviceState === 'active' ? 'Running' : state.serviceState === 'inactive' ? 'Stopped' : state.serviceState;
    $('service-detail').textContent = fresh ? `${s.activeChats} active conversations` : 'No recent agent heartbeat';
    $('qq-value').textContent = fresh && s.qqOnline ? 'Online' : 'Disconnected';
    $('qq-detail').textContent = fresh && s.onebotConnected ? `Account ${s.selfId}` : 'Waiting for SnowLuma';
    $('model-value').textContent = s?.model || saved?.config.provider.model || 'Not set';
    $('provider-detail').textContent = s?.provider === 'anthropic' ? 'Anthropic-compatible API' : 'OpenAI-compatible API';
    $('chat-value').textContent = saved ? saved.config.agent.allowedGroups.length + saved.config.agent.allowedUsers.length : '—';
    $('mode-badge').textContent = state.serviceState !== 'active' ? 'Service stopped' : s?.mode === 'waiting_for_setup' ? 'Setup needed' : s?.mode === 'dry_run' ? 'Preview mode' : 'Agent active';
    $('readiness').textContent = s?.missing?.length ? 'Complete setup: ' + s.missing.join(' + ') + '.' : 'The agent is ready to participate in enabled conversations.';
    const applied = fresh && s.appliedRevision === state.savedRevision && !s.reloading;
    $('applied-indicator').textContent = applied ? 'SETTINGS APPLIED' : state.serviceState !== 'active' ? 'SERVICE STOPPED' : 'APPLYING SETTINGS';
    if (!dirty) $('apply-status').textContent = s?.reloadError || (applied ? 'Saved settings are active' : state.serviceState !== 'active' ? 'Saved. Start the service to apply.' : 'Waiting for the agent to apply settings…');
    $('threshold-preview').textContent = saved?.config.agent.threshold.toFixed(2) ?? '—';
    $('cooldown-preview').textContent = `${saved?.config.agent.proactiveCooldownSeconds ?? '—'}s`;
    $('api-calls').textContent = s?.apiCallsThisRun ?? '—';
    $('last-updated').textContent = 'UPDATED ' + new Date().toLocaleTimeString();
    $('decision-list').replaceChildren();
    if (!state.decisions.length) $('decision-list').append(element('p', 'No decisions yet. Activity will appear when the agent processes an enabled chat.', 'empty'));
    for (const d of state.decisions.slice(0, 8)) {
      const row = element('div', null, 'decision-row');
      row.append(element('time', time(d.ts)), element('span', d.chat), element('span', d.action.replaceAll('_', ' ')), element('span', Number(d.score).toFixed(2), 'decision-score'));
      $('decision-list').append(row);
    }
    $('thought-list').replaceChildren();
    if (!state.thoughts.length) $('thought-list').append(element('p', 'No retained ideas yet. Ideas that are withheld can remain here for later reevaluation.', 'empty'));
    for (const t of state.thoughts) { const box = element('div', null, 'thought'); box.append(element('p', t.text), element('small', `${t.chat} · ${t.kind} · score ${Number(t.score).toFixed(2)}`)); $('thought-list').append(box); }
    logs = state.logs; renderLogs();
  } catch (e) { $('dashboard-connection').textContent = 'Reconnecting…'; if (online) notice('Connection interrupted. Retrying automatically.', true); online = false; }
  finally { polling = false; }
}
async function boot() {
  const session = await api('/api/session'); csrf = session.csrf;
  populate(await api('/api/config'));
  $('login').hidden = true; $('console').hidden = false;
  await refresh();
}
$('login-form').addEventListener('submit', async e => {
  e.preventDefault(); const b = e.submitter; b.disabled = true; $('login-error').textContent = '';
  try { const r = await api('/api/login', { method: 'POST', body: JSON.stringify({ key: $('access-key').value }) }); csrf = r.csrf; $('access-key').value = ''; await boot(); }
  catch (e) { $('login-error').textContent = e.message; }
  finally { b.disabled = false; }
});
document.querySelectorAll('[data-page]').forEach(b => b.addEventListener('click', () => page(b.dataset.page)));
$('logout').addEventListener('click', async () => { try { await api('/api/logout', { method: 'POST', body: '{}' }); } finally { signedOut(); } });
$('refresh').addEventListener('click', refresh);
$('config-form').addEventListener('input', () => { changed(); $('threshold-output').textContent = Number(document.querySelector('[data-config="agent.threshold"]').value).toFixed(2); });
$('config-form').addEventListener('submit', async e => {
  e.preventDefault(); $('save').disabled = true;
  try {
    const result = await api('/api/config', { method: 'PUT', body: JSON.stringify({ revision: saved.revision, config: formConfig(), apiKey: $('api-key').value, onebotToken: $('onebot-token').value, clearApiKey: $('clear-api-key').checked }) });
    populate(result); notice('Settings saved. The agent is applying them now.'); await refresh();
  } catch (e) { notice(e.message, true); }
  finally { $('save').disabled = false; }
});
$('discard').addEventListener('click', async () => { try { populate(await api('/api/config')); notice('Loaded saved settings.'); } catch(e) { notice(e.message, true); } });
$('load-json').addEventListener('click', () => { const use = $('use-advanced').checked; $('use-advanced').checked = false; $('advanced-json').value = JSON.stringify(formConfig(), null, 2); $('use-advanced').checked = use; });
$('deepseek-preset').addEventListener('click', () => {
  const kind = document.querySelector('[data-config="provider.kind"]').value;
  document.querySelector('[data-config="provider.baseUrl"]').value = kind === 'anthropic' ? 'https://api.deepseek.com/anthropic' : 'https://api.deepseek.com';
  document.querySelector('[data-config="provider.model"]').value = 'deepseek-flash';
  saved.config.provider.tokenParameter = 'max_tokens'; saved.config.provider.thinking = 'disabled'; changed();
});
document.querySelectorAll('[data-service]').forEach(b => b.addEventListener('click', async () => {
  b.disabled = true; try { await api('/api/service', { method: 'POST', body: JSON.stringify({ action: b.dataset.service }) }); notice(`Service command completed: ${b.dataset.service}.`); await refresh(); } catch(e) { notice(e.message, true); } finally { b.disabled = false; }
}));
$('test-model').addEventListener('click', async () => {
  $('test-model').disabled = true; $('model-test-result').textContent = 'Testing the saved model configuration…';
  try { const r = await api('/api/test-model', { method: 'POST', body: '{}' }); $('model-test-result').textContent = r.message; } catch(e) { $('model-test-result').textContent = e.message; } finally { $('test-model').disabled = false; }
});
$('load-contacts').addEventListener('click', async () => {
  $('load-contacts').disabled = true;
  try {
    const list = await api('/api/contacts'); $('contacts').replaceChildren(); $('contacts').hidden = false;
    for (const [title, items, field] of [['Groups', list.groups, 'agent.allowedGroups'], ['Private contacts', list.friends, 'agent.allowedUsers']]) {
      const section = element('div'); section.append(element('h3', title));
      const target = document.querySelector(`[data-config="${field}"]`);
      if (!items.length) section.append(element('p', 'No contacts returned.', 'hint'));
      for (const item of items) {
        const label = element('label', null, 'check'), input = document.createElement('input'); input.type = 'checkbox'; input.checked = ids(target.value).includes(item.id);
        input.addEventListener('change', () => { const values = new Set(ids(target.value)); if (input.checked) values.add(item.id); else values.delete(item.id); target.value = [...values].join(', '); changed(); });
        label.append(input, element('span', `${item.name || item.id} · ${item.id}`)); section.append(label);
      }
      $('contacts').append(section);
    }
  } catch(e) { notice(e.message, true); } finally { $('load-contacts').disabled = false; }
});
$('log-filter').addEventListener('input', renderLogs); $('pause-logs').addEventListener('change', renderLogs);
window.addEventListener('beforeunload', e => { if (dirty) { e.preventDefault(); e.returnValue = ''; } });
setInterval(refresh, 2000);
boot().catch(() => signedOut());
