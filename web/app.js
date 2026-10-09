import { startI18n, setLanguage, translate } from './i18n.mjs';

// The one personal line the console shows its owner. The phrase is written in
// English like every other application-owned string so the i18n observer
// translates it in place; refresh() recomputes it, so a language switch follows.
function greetingFor(date) {
  const hour = date.getHours();
  if (hour < 5) return 'Good evening';
  if (hour < 12) return 'Good morning';
  if (hour < 18) return 'Good afternoon';
  return 'Good evening';
}
const $ = id => document.getElementById(id);
startI18n();
let csrf = '', saved = null, dirty = false, polling = false, logs = [], online = false;
let learningView = '', preset = {};
// Object key order is irrelevant; arrays are atomic ordered values. Missing
// advanced keys are rejected rather than silently resetting unrelated settings.
function configPatch(base, next, prefix = '', patch = Object.create(null)) {
  const object = v => v !== null && typeof v === 'object' && !Array.isArray(v);
  if (object(base) && object(next)) {
    for (const key of Object.keys(base)) if (!Object.hasOwn(next, key)) throw Error(`Missing setting: ${prefix}${key}`);
    for (const key of Object.keys(next)) configPatch(base[key], next[key], prefix + key + '.', patch);
  } else if (Array.isArray(base) && Array.isArray(next)) {
    const equal = (a, b) => a === b || (a !== null && b !== null && typeof a === 'object' && typeof b === 'object' && Array.isArray(a) === Array.isArray(b) && Object.keys(a).length === Object.keys(b).length && Object.keys(a).every(k => Object.hasOwn(b, k) && equal(a[k], b[k])));
    if (!equal(base, next)) patch[prefix.slice(0, -1)] = next;
  } else if (base !== next) patch[prefix.slice(0, -1)] = next;
  return patch;
}
const get = (obj, key) => key.split('.').reduce((o, k) => o?.[k], obj);
const set = (obj, key, value) => { const parts = key.split('.'); const end = parts.pop(); parts.reduce((o,k) => o[k] ??= {}, obj)[end] = value; };
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
function applyStatus(text) { for (const id of ['apply-status', 'advanced-apply-status']) $(id).textContent = text; }
function changed() { dirty = true; $('dirty-dot').hidden = false; applyStatus('Unsaved changes'); }
function page(name) {
  for (const p of document.querySelectorAll('.page')) p.hidden = p.id !== name;
  for (const b of document.querySelectorAll('.nav')) b.classList.toggle('active', b.dataset.page === name);
  $('page-title').textContent = { overview: 'Overview', config: 'Configuration', activity: 'Activity & logs', advanced: 'Advanced' }[name];
}
function populate(data) {
  saved = data; preset = {};
  setLanguage(data.config.ui.language);
  $('interface-language').value = data.config.ui.language;
  for (const input of document.querySelectorAll('[data-config]')) {
    const value = get(data.config, input.dataset.config) ?? (input.dataset.type ? [] : input.dataset.default ?? '');
    if (input.type === 'checkbox') input.checked = Boolean(value);
    else input.value = input.dataset.type === 'ids' ? value.join(', ') : input.dataset.type === 'lines' ? value.join('\n') : value;
  }
  const q = data.config.agent.quietHours;
  $('quiet-enabled').checked = !!q; $('quiet-start').value = q?.start ?? 23;
  $('quiet-end').value = q?.end ?? 8; $('quiet-timezone').value = q?.timezone || 'Europe/Stockholm';
  $('key-status').textContent = data.hasApiKey ? 'Saved securely' : 'Not configured';
  $('api-key').value = ''; $('onebot-token').value = ''; $('clear-api-key').checked = false;
  $('advanced-json').value = JSON.stringify(data.config, null, 2);
  $('use-advanced').checked = false; $('threshold-output').textContent = data.config.agent.threshold.toFixed(2);
  dirty = false; $('dirty-dot').hidden = true; applyStatus('No pending changes');
}
function formConfig() {
  if ($('use-advanced').checked) return JSON.parse($('advanced-json').value);
  const c = structuredClone(saved.config);
  for (const input of document.querySelectorAll('[data-config]')) {
    let value = input.type === 'checkbox' ? input.checked : input.dataset.type === 'lines' ? input.value.split('\n').map(x=>x.trim()).filter(Boolean) : input.dataset.type === 'ids' ? ids(input.value) : ['number', 'range'].includes(input.type) ? Number(input.value) : input.value;
    set(c, input.dataset.config, value);
  }
  Object.assign(c.provider, preset);
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
  output.textContent = lines.join('\n') || translate('No matching events yet.');
  if (atBottom) output.scrollTop = output.scrollHeight;
}
async function refresh() {
  if (!csrf || polling) return; polling = true;
  try {
    const state = await api('/api/state'), s = state.status;
    online = true; $('dashboard-connection').textContent = 'Live connection'; document.body.classList.add('live');
    $('greeting').textContent = greetingFor(new Date());
    const fresh = s && Date.now() - Date.parse(s.updatedAt) < 20000;
    $('service-value').textContent = state.serviceState === 'active' ? 'Running' : state.serviceState === 'inactive' ? 'Stopped' : state.serviceState;
    $('service-detail').textContent = fresh ? `${s.activeChats} active conversations` : 'No recent agent heartbeat';
    $('qq-value').textContent = fresh && s.qqOnline ? 'Online' : 'Disconnected';
    $('qq-detail').textContent = fresh && s.onebotConnected ? `Account ${s.selfId}` : 'Waiting for NapCat / SnowLuma';
    $('model-value').textContent = s?.model || saved?.config.provider.model || 'Not set';
    $('provider-detail').textContent = s?.provider === 'anthropic' ? 'Anthropic-compatible API' : 'OpenAI-compatible API';
    $('chat-value').textContent = saved ? saved.config.agent.allowedGroups.length + saved.config.agent.allowedUsers.length : '—';
    $('mode-badge').textContent = state.serviceState !== 'active' ? 'Service stopped' : s?.mode === 'waiting_for_setup' ? 'Setup needed' : s?.scheduleActive === false ? 'Inactive hours' : s?.mode === 'dry_run' ? 'Preview mode' : 'Agent active';
    $('readiness').textContent = s?.missing?.length ? 'Complete setup: ' + s.missing.join(' + ') + '.' : s?.scheduleActive === false ? 'AI participation is paused by the activity schedule or rest block.' : 'The agent is ready to participate in enabled conversations.';
    const rhythm = s?.activityRhythm;
    $('activity-rhythm-status').textContent = fresh && rhythm?.enabled ? `${translate(rhythm.active ? 'Active block' : 'Rest block')} · ${translate('Next block selection')}: ${time(rhythm.until)} · ${translate('Activity probability at selection')}: ${(rhythm.probability * 100).toFixed(1)}% · ${translate('Current curve probability')}: ${(rhythm.currentProbability * 100).toFixed(1)}%` : '';
    const applied = fresh && s.appliedRevision === state.savedRevision && !s.reloading;
    $('applied-indicator').textContent = applied ? 'SETTINGS APPLIED' : state.serviceState !== 'active' ? 'SERVICE STOPPED' : 'APPLYING SETTINGS';
    if (!dirty) applyStatus(s?.reloadError || (applied ? 'Saved settings are active' : state.serviceState !== 'active' ? 'Saved. Start the service to apply.' : 'Waiting for the agent to apply settings…'));
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
    $('assessment-list').replaceChildren();
    if (!state.assessments?.length) $('assessment-list').append(element('p', 'No sending forecasts yet.', 'empty'));
    for (const record of state.assessments || []) {
      const d = record.details, box = element('div', null, 'thought');
      box.append(element('small', `${time(record.ts)} · ${record.chat} · ${translate(record.status)} · ${translate('Send probability')}: ${(d.probability * 100).toFixed(1)}%`));
      box.append(element('p', `${translate('Expected response')}: ${translate('Reply')} ${(d.prediction.outcomes.reply * 100).toFixed(0)}% · ${translate('Silence')} ${(d.prediction.outcomes.silence * 100).toFixed(0)}% · ${translate('Negative reaction')} ${(d.prediction.outcomes.negative * 100).toFixed(0)}%`));
      box.append(element('p', d.prediction.plan));
      const detail = element('details'), summary = element('summary', translate('Calculation details'));
      detail.append(summary, element('pre', JSON.stringify({ factors: d.factors, draw: d.draw, timing: d.timing, veto: d.veto, responseMode: d.prediction.responseMode }, null, 2)));
      box.append(detail); $('assessment-list').append(box);
    }
    $('observation-list').replaceChildren();
    if (!state.observations?.length) $('observation-list').append(element('p', 'No group observation yet.', 'empty'));
    for (const o of state.observations || []) {
      const box = element('article', null, 'thought');
      box.append(element('h3', `${o.chat} · ${o.sources.info?.name || translate('Group name unavailable')}`));
      box.append(element('p', `${translate(o.status === 'ready' ? 'Style selected' : 'Observing before first message')} · ${translate('Elapsed seconds')}: ${Math.max(0, Math.floor(Date.now()/1000-o.started))} · ${translate('New messages')}: ${o.message_count}`));
      box.append(element('p', ['info','notices','history'].map(k => `${translate({info:'Group information',notices:'Group announcements',history:'Group history'}[k])}: ${translate(o.sources.availability?.[k] || 'pending')}`).join(' · ')));
      if (o.analysis.style) box.append(element('p', `${translate('Initial speaking style')}: ${o.analysis.style}`), element('p', o.analysis.summary));
      if (o.error) box.append(element('p', 'Analysis failed; waiting to retry. No group message will be sent.', 'hint'));
      $('observation-list').append(box);
    }
    const learningKey = JSON.stringify([state.learning, state.memories, document.documentElement.lang]);
    if (learningKey !== learningView) {
    learningView = learningKey;
    $('learning-list').replaceChildren();
    if (!state.learning?.length) $('learning-list').append(element('p', 'No learned chat preferences yet.', 'empty'));
    for (const profile of state.learning || []) {
      const chatMemories = (state.memories || []).filter(m => m.chat === profile.chat);
      for (const subject of [...new Set(chatMemories.map(m => m.subject))]) {
      const box = element('article', null, 'thought'), reset = element('button', translate('Reset learned style and memories'), 'secondary');
      reset.type = 'button';
      reset.addEventListener('click', async () => {
        reset.disabled = true;
        try { await api('/api/learning/reset', { method: 'POST', body: JSON.stringify({ chat: profile.chat, subject }) }); notice('Learned style and memories reset.'); await refresh(); }
        catch (e) { notice(e.message, true); reset.disabled = false; }
      });
      box.append(element('h3', `${profile.chat} · ${subject === 'group' ? translate('Group memory') : subject}`), reset);
      for (const layer of ['long_term', 'short_term', 'traits']) {
        box.append(element('h4', translate({ long_term: 'Long-term notebook', short_term: 'Short-term details', traits: 'Traits and topics' }[layer])));
        for (const memory of chatMemories.filter(m => m.subject === subject && m.layer === layer)) {
          const item = element('details'); item.append(element('summary', `${layer === 'short_term' ? '' : memory.slot + ': '}${memory.text}`), element('pre', JSON.stringify({ keywords: JSON.parse(memory.keywords || '[]'), confidence: memory.confidence ?? 0.6, revisions: memory.revisions || [], sources: JSON.parse(memory.sources), importance: memory.importance, revision: memory.revision, updated: new Date(memory.updated * 1000).toISOString(), expires: new Date(memory.expires * 1000).toISOString() }, null, 2))); box.append(item);
        }
      }
      $('learning-list').append(box);
      }
    }
    }
    $('expression-list').replaceChildren();
    if (!state.expressions?.length) $('expression-list').append(element('p', translate('No learned expressions yet.'), 'empty'));
    for (const r of state.expressions || []) {
      const row = element('article', null, 'thought');
      row.append(element('h3', `${r.chat} · ${r.subject} · ${translate(r.kind)}: ${r.term}`), element('p', r.meaning), element('p', `${translate('Applicable situation')}: ${r.situation}`), element('p', `${translate('Observed example')}: ${r.example}`), element('small', `${translate('Confidence')}: ${r.confidence} · ${translate('Evidence messages')}: ${JSON.parse(r.sources).length}`));
      const reset = element('button', translate('Reset this subject’s learning'), 'secondary');
      reset.addEventListener('click', async () => { reset.disabled=true; try { await api('/api/learning/reset',{method:'POST',body:JSON.stringify({chat:r.chat,subject:r.subject})}); await refresh(); } catch(e) {notice(e.message,true);reset.disabled=false;} });
      row.append(reset); $('expression-list').append(row);
    }
    $('thought-list').replaceChildren();
    if (!state.thoughts.length) $('thought-list').append(element('p', 'No retained ideas yet. Ideas that are withheld can remain here for later reevaluation.', 'empty'));
    for (const t of state.thoughts) { const box = element('div', null, 'thought'); box.append(element('p', t.text), element('small', `${t.chat} · ${t.kind} · score ${Number(t.score).toFixed(2)}`)); $('thought-list').append(box); }
    logs = state.logs; renderLogs();
  } catch (e) { document.body.classList.remove('live'); $('dashboard-connection').textContent = 'Reconnecting…'; if (online) notice('Connection interrupted. Retrying automatically.', true); online = false; }
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
for (const selector of ['#interface-language', '[data-config="ui.language"]']) {
  document.querySelector(selector).addEventListener('change', e => {
    const language = e.target.value; setLanguage(language);
    $('interface-language').value = language;
    document.querySelector('[data-config="ui.language"]').value = language;
    if (saved) {
      if ($('use-advanced').checked) {
        try { const c = JSON.parse($('advanced-json').value); c.ui = { ...c.ui, language }; $('advanced-json').value = JSON.stringify(c, null, 2); } catch {}
      }
      changed();
    }
  });
}
$('logout').addEventListener('click', async () => { try { await api('/api/logout', { method: 'POST', body: '{}' }); } finally { signedOut(); } });
$('refresh').addEventListener('click', refresh);
$('config-form').addEventListener('input', () => { changed(); $('threshold-output').textContent = Number(document.querySelector('[data-config="agent.threshold"]').value).toFixed(2); });
$('config-form').addEventListener('submit', async e => {
  e.preventDefault(); document.querySelectorAll('[data-save]').forEach(b => b.disabled = true);
  try {
    const result = await api('/api/config', { method: 'PUT', body: JSON.stringify({ revision: saved.revision, patch: configPatch(saved.config, formConfig()), apiKey: $('api-key').value, onebotToken: $('onebot-token').value, clearApiKey: $('clear-api-key').checked }) });
    populate(result); notice('Settings saved. The agent is applying them now.'); await refresh();
  } catch (e) { notice(e.message, true); }
  finally { document.querySelectorAll('[data-save]').forEach(b => b.disabled = false); }
});
document.querySelectorAll('[data-discard]').forEach(b => b.addEventListener('click', async () => { try { populate(await api('/api/config')); notice('Loaded saved settings.'); } catch(e) { notice(e.message, true); } }));
// Reveal invalid controls even when they belong to the other configuration page.
$('config-form').addEventListener('invalid', e => page(e.target.closest('.page').id), true);
$('load-json').addEventListener('click', () => { const use = $('use-advanced').checked; $('use-advanced').checked = false; $('advanced-json').value = JSON.stringify(formConfig(), null, 2); $('use-advanced').checked = use; });
$('deepseek-preset').addEventListener('click', () => {
  const kind = document.querySelector('[data-config="provider.kind"]').value;
  document.querySelector('[data-config="provider.baseUrl"]').value = kind === 'anthropic' ? 'https://api.deepseek.com/anthropic' : 'https://api.deepseek.com';
  document.querySelector('[data-config="provider.model"]').value = 'deepseek-flash';
  preset = { tokenParameter: 'max_tokens', thinking: 'disabled' };
  if ($('use-advanced').checked) {
    try { const c = JSON.parse($('advanced-json').value); Object.assign(c.provider, preset, { baseUrl: document.querySelector('[data-config="provider.baseUrl"]').value, model: 'deepseek-flash' }); $('advanced-json').value = JSON.stringify(c, null, 2); }
    catch { notice('Fix the advanced JSON before saving the preset.', true); }
  }
  changed();
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
$('load-models').addEventListener('click', async () => {
  $('load-models').disabled = true; $('model-list-status').textContent = 'Loading models from the saved provider…';
  try {
    const r = await api('/api/models', { method: 'POST', body: '{}' });
    $('model-choice').replaceChildren(element('option', 'Choose a model…')); $('model-choice').firstChild.value = '';
    for (const id of r.models) { const option = element('option', id); option.value = id; $('model-choice').append(option); }
    $('model-list-status').textContent = `${r.models.length} models returned. Select one and save to apply. Manual entry is also available.`;
  } catch (e) { $('model-list-status').textContent = `${e.message}. You can enter the model ID manually below.`; }
  finally { $('load-models').disabled = false; }
});
$('model-choice').addEventListener('change', () => {
  if ($('model-choice').value) {
    document.querySelector('[data-config="provider.model"]').value = $('model-choice').value;
    if ($('use-advanced').checked) {
      try { const c = JSON.parse($('advanced-json').value); c.provider.model = $('model-choice').value; $('advanced-json').value = JSON.stringify(c, null, 2); }
      catch { notice('Fix the advanced JSON before saving the selected model.', true); }
    }
    changed();
  }
});
async function refreshDebug() {
  if (!csrf || document.hidden) return;
  try {
    const d = await api('/api/debug/receive');
    $('debug-receive-status').textContent = `${d.state}${d.account ? ' · QQ ' + d.account : ''} · ${d.events.length} events${d.until && d.state === 'listening' ? ' · ' + Math.max(0, Math.ceil((d.until - Date.now()) / 1000)) + 's remaining' : ''}${d.error ? ' · ' + d.error : ''}`;
    $('debug-events').textContent = d.events.length ? d.events.map(e => `${e.receivedAt} · ${e.postType} · ${e.chatType} · ${e.types.join(', ')}\n${e.text || '(attachment without text)'}`).join('\n\n') : translate('No events captured. If the test finishes empty, check self-message reporting and the bridge event connection.');
    $('debug-receive').disabled = ['connecting', 'listening'].includes(d.state);
  } catch (e) { $('debug-receive-status').textContent = e.message; }
}
$('debug-send').addEventListener('click', async () => {
  $('debug-send').disabled = true; $('debug-send-result').textContent = 'Sending to the connected account…';
  try { const r = await api('/api/debug/send', { method: 'POST', body: '{}' }); $('debug-send-result').textContent = `QQ ${r.account} · message ${r.messageId ?? '(no ID returned)'} · ${r.message} ${r.text}`; }
  catch (e) { $('debug-send-result').textContent = `${e.message}. Not retried. Check QQ before trying again.`; }
  finally { $('debug-send').disabled = false; }
});
for (const [id, endpoint] of [['debug-receive', 'receive'], ['debug-stop', 'stop']]) {
  $(id).addEventListener('click', async () => {
    $(id).disabled = true;
    try { await api('/api/debug/' + endpoint, { method: 'POST', body: '{}' }); await refreshDebug(); }
    catch (e) { $('debug-receive-status').textContent = e.message; }
    finally { $(id).disabled = false; }
  });
}
setInterval(refreshDebug, 2000);
boot().catch(() => signedOut());
