import { readiness } from './config.mjs';
import { allowed, normalize, quiet, activeAt, select, repeated } from './policy.mjs';
import { formation, evaluation, articulation } from './prompts.mjs';

const criteria = new Set(['relevance', 'information_gap', 'expected_impact', 'urgency', 'coherence', 'originality', 'balance', 'dynamics']);
const scoreOk = n => typeof n === 'number' && Number.isFinite(n) && n >= 1 && n <= 5;

export class Engine {
  constructor(config, store, provider, transport, options = {}) {
    this.config = config; this.store = store; this.provider = provider; this.transport = transport;
    this.now = options.now || (() => Date.now() / 1000); this.log = options.log || (() => {});
    this.chats = new Map(); this.running = new Set(); this.controller = new AbortController();
    this.lastError = null; this.lastCycle = 0;
  }
  state(chat) {
    if (!this.chats.has(chat)) {
      if (this.chats.size >= this.config.agent.maxActiveChats) return null;
      this.chats.set(chat, { version: 0, lastHuman: 0, lastId: '', hint: 'open', pending: false,
        pauseDone: true, lastThink: 0, busy: false, due: 0 });
    }
    return this.chats.get(chat);
  }
  ingest(event) {
    const a = this.config.agent, now = this.now();
    if (!activeAt(now, a.schedule)) return;
    const m = normalize(event, this.transport.selfId, a, now);
    if (!m) return;
    const state = this.state(m.chat);
    if (!state || !this.store.message(m)) return;
    state.version++; state.lastHuman = now; state.lastId = m.id;
    // Keep a direct request pending while later group messages arrive in the same batch.
    state.hint = state.pending && state.hint === 'self' ? 'self' : m.hint;
    state.pending = true; state.pauseDone = false;
    state.due = now + a.debounceSeconds;
  }
  restore() {
    // Preserve memory across restarts, but don't replay old replies or initiate on old history.
    for (const chat of this.store.activeChats(this.now() - this.config.agent.activeWindowSeconds)) {
      if (!allowed(chat, this.config.agent)) continue;
      const state = this.state(chat); if (!state) break;
      const last = this.store.history(chat).filter(x => !x.self).at(-1);
      if (last) { state.lastHuman = last.ts; state.lastId = last.id; state.pauseDone = true; }
    }
  }
  tick() {
    const now = this.now(), a = this.config.agent;
    if (!activeAt(now, a.schedule)) {
      for (const state of this.chats.values()) { state.version++; state.pending = false; state.pauseDone = true; }
      return;
    }
    for (const [chat, state] of this.chats) {
      if (!state.busy && now - state.lastHuman > a.activeWindowSeconds) this.chats.delete(chat);
    }
    if (readiness(this.config).length || !this.transport.connected || !this.transport.online || this.controller.signal.aborted) return;
    for (const [chat, state] of this.chats) {
      if (this.running.size >= a.maxConcurrentChats) break;
      if (state.busy || now - state.lastHuman > a.activeWindowSeconds || now < state.due) continue;
      if (now - state.lastThink < a.minThinkIntervalSeconds && state.hint !== 'self') continue;
      const trigger = state.pending ? 'message' : !state.pauseDone && now - state.lastHuman >= a.pauseSeconds ? 'pause' : null;
      if (!trigger || (trigger === 'pause' && (!a.proactive || quiet(now, a.quietHours)))) continue;
      if (state.hint !== 'self' && (!a.proactive || quiet(now, a.quietHours))) {
        state.pending = false; state.pauseDone = true; continue;
      }
      state.busy = true;
      const promise = this.cycle(chat, trigger).catch(error => {
        this.lastError = error.code || 'cycle_failed';
        this.log('cycle_error', { chat, code: this.lastError });
        state.due = this.now() + 60;
      }).finally(() => { state.busy = false; this.running.delete(promise); });
      this.running.add(promise);
    }
  }
  async cycle(chat, trigger = 'message') {
    const state = this.chats.get(chat), a = this.config.agent, now = this.now();
    if (!state || !allowed(chat, a) || !activeAt(now, a.schedule)) return;
    const version = state.version, id = state.lastId;
    const hint = trigger === 'pause' ? 'open' : state.hint;
    state.lastThink = now; this.lastCycle = now;
    const history = this.store.history(chat, a.historyLimit);
    const last = history.filter(x => !x.self).at(-1);
    if (!last) return;
    const counts = this.store.counts(chat, now);
    const gated = counts.total >= a.maxMessagesPerHour || (hint !== 'self' &&
      (counts.proactive >= a.maxProactivePerHour || now - counts.last < a.proactiveCooldownSeconds));
    if (gated) { this.finish(state, chat, id, version, trigger); return; }
    const signal = this.controller.signal;
    const payload = { persona: a.persona, name: a.name, trigger, addressedHint: hint,
      history: history.map(x => ({ speaker: x.self ? a.name : x.name, text: x.text })),
      memories: this.store.retrieve(chat, last.text, now),
      retainedIdeas: this.store.reservoir(chat, now, a.thoughtTtlSeconds, a.thoughtLimit),
    };
    const formed = await this.provider.json(formation, payload, signal);
    if (!Array.isArray(formed.candidates) || !['self', 'other', 'open'].includes(formed.allocation)) throw Object.assign(Error('Invalid formation'), { code: 'invalid_formation' });
    for (const candidate of formed.candidates.slice(0, 3)) {
      if (!candidate || typeof candidate.text !== 'string' || !candidate.text.trim() || !['system1', 'system2'].includes(candidate.kind)) continue;
      const thought = { text: candidate.text.trim().slice(0, 300), kind: candidate.kind };
      const existing = this.store.reservoir(chat, now, a.thoughtTtlSeconds, a.thoughtLimit);
      if (!existing.some(x => x.text === thought.text)) this.store.addThought(chat, thought, now);
    }
    if (version !== state.version || !activeAt(this.now(), a.schedule)) return; // obsolete context or schedule
    const candidates = this.store.reservoir(chat, now, a.thoughtTtlSeconds, a.thoughtLimit);
    if (!candidates.length) { this.finish(state, chat, id, version, trigger); return; }
    const result = await this.provider.json(evaluation, { ...payload, retainedIdeas: undefined, candidates,
      recentAgentMessages: counts.total }, signal);
    if (!Array.isArray(result.ratings)) throw Object.assign(Error('Invalid evaluation'), { code: 'invalid_evaluation' });
    const seen = new Set();
    const rated = result.ratings.flatMap(r => {
      const c = candidates.find(x => x.id === r?.id);
      if (!c || seen.has(r.id) || ![r.motivation, r.relevance, r.originality].every(scoreOk)) return [];
      seen.add(r.id);
      return [{ ...c, motivation: r.motivation, relevance: r.relevance, originality: r.originality,
        for: Array.isArray(r.for) ? r.for.filter(x => criteria.has(x)).slice(0, 2) : [],
        against: Array.isArray(r.against) ? r.against.filter(x => criteria.has(x)).slice(0, 2) : [] }];
    });
    if (!rated.length) throw Object.assign(Error('Invalid ratings'), { code: 'invalid_ratings' });
    for (const r of rated) this.store.score(r.id, r.motivation);
    if (version !== state.version || !activeAt(this.now(), a.schedule)) return;
    // Explicit @mentions take precedence over the model's turn prediction.
    const allocation = hint === 'self' || hint === 'other' ? hint : formed.allocation;
    const turnsSilent = history.slice(history.findLastIndex(x => x.self) + 1).filter(x => !x.self).length;
    const selected = select(rated, allocation, a, turnsSilent);
    // Model-inferred invitation may affect selection, but may NEVER bypass rate/quiet controls.
    const proactive = hint !== 'self';
    if (!selected || (proactive && (!a.proactive || quiet(now, a.quietHours)))) {
      this.store.decision(chat, 'withhold', selected?.adjusted || 0, [], now);
      this.finish(state, chat, id, version, trigger); return;
    }
    const response = await this.provider.json(articulation, { persona: a.persona, name: a.name,
      history: payload.history, selectedIdea: selected.text, assertiveTone: a.proactiveTone, maxCharacters: a.maxOutputChars }, signal);
    if (typeof response.text !== 'string' || !response.text.trim() || /<\/?(?:think|analysis)>/i.test(response.text)) throw Object.assign(Error('Invalid articulation'), { code: 'invalid_articulation' });
    const text = [...response.text.trim()].slice(0, a.maxOutputChars).join('');
    if (version !== state.version || this.now() - state.lastHuman > a.activeWindowSeconds || signal.aborted || !activeAt(this.now(), a.schedule)) return;
    if (!this.transport.connected || !this.transport.online) throw Object.assign(Error('Offline'), { code: 'qq_offline' });
    if ((proactive && quiet(this.now(), a.quietHours)) || repeated(text, history)) {
      this.store.use(selected.id); this.finish(state, chat, id, version, trigger); return;
    }
    if (a.dryRun) {
      this.store.decision(chat, 'dry_run', selected.adjusted, [...selected.for, ...selected.against], now);
      this.store.use(selected.id); this.finish(state, chat, id, version, trigger);
      this.log('dry_run', { chat, score: selected.adjusted }); return;
    }
    // Persist before transmission. An ambiguous timeout is never automatically resent.
    const deliveryId = this.store.delivery(chat, proactive, this.now());
    this.store.use(selected.id);
    this.finish(state, chat, id, version, trigger, true);
    try {
      const sent = await this.transport.send(chat, text);
      this.store.finishDelivery(deliveryId, 'sent', sent?.message_id);
      this.store.message({ chat, id: String(sent?.message_id ?? deliveryId), sender: this.transport.selfId, name: a.name, text, ts: this.now(), self: true });
      this.store.decision(chat, 'sent', selected.adjusted, selected.for, this.now());
      this.lastError = null; this.log('message_sent', { chat, proactive });
    } catch (error) {
      this.store.finishDelivery(deliveryId, error.uncertain ? 'uncertain' : 'failed');
      this.store.decision(chat, error.uncertain ? 'delivery_uncertain' : 'delivery_failed', selected.adjusted, [], this.now());
      // Do not schedule an automatic duplicate, even for an explicit server rejection.
      this.lastError = error.code || 'delivery_failed'; this.log('delivery_error', { chat, code: this.lastError });
    }
  }
  finish(state, chat, id, version, trigger, sent = false) {
    if (state.version !== version) return;
    state.pending = false; state.hint = 'open';
    if (trigger === 'pause' || sent) state.pauseDone = true;
    this.store.markHandled(chat, id, state.pauseDone);
  }
  async stop() {
    this.controller.abort();
    await Promise.allSettled([...this.running]);
  }
}
