import { readiness } from './config.mjs';
import { allowed, normalize, quiet, select, repeated } from './policy.mjs';
import { formation, evaluation, articulationFor, forecast } from './prompts.mjs';
import { forecastResult, sendingProbability } from './sending.mjs';
import { parseMemoryUpdates } from './memory.mjs';
import { ActivityRhythm } from './activity.mjs';
import { GroupOrientation } from './orientation.mjs';

const criteria = new Set(['relevance', 'information_gap', 'expected_impact', 'urgency', 'coherence', 'originality', 'balance', 'dynamics']);
const scoreOk = n => typeof n === 'number' && Number.isFinite(n) && n >= 1 && n <= 5;

export class Engine {
  constructor(config, store, provider, transport, options = {}) {
    this.config = config; this.store = store; this.provider = provider; this.transport = transport;
    this.now = options.now || (() => Date.now() / 1000); this.log = options.log || (() => {});
    this.random = options.random || Math.random;
    this.chats = new Map(); this.running = new Set(); this.controller = new AbortController();
    this.lastError = null; this.lastCycle = 0;
    this.activity = new ActivityRhythm(store, config.agent, options.activityRandom || Math.random);
    this.orientation = new GroupOrientation(store, config, provider, transport, this.now, this.controller.signal);
  }
  available(now) { return this.activity.snapshot(now).active; }
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
    if (a.observation.enabled && event.post_type === 'notice' && event.notice_type === 'group_increase' && (event.self_id == null || String(event.self_id) === String(this.transport.selfId)) && String(event.user_id) === String(this.transport.selfId) && a.allowedGroups.includes(String(event.group_id))) {
      const chat = `group:${event.group_id}`, previous = this.orientation.get(chat)?.epoch;
      this.orientation.joined(chat, Number(event.time) || now);
      const state = this.chats.get(chat);
      if (state && previous !== this.orientation.get(chat)?.epoch) { state.version++; state.pending = false; state.pauseDone = true; }
      return;
    }
    if (!this.available(now)) return;
    const m = normalize(event, this.transport.selfId, a, now);
    if (!m) return;
    const state = this.state(m.chat);
    if (!state || !this.store.message(m)) return;
    this.orientation.observe(m.chat);
    if (a.learning.enabled) this.store.memory.capture(m, now, a.memory);
    this.store.observe(m, now);
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
    if (!this.available(now)) {
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
    if (!state || !allowed(chat, a) || !this.available(now)) return;
    const version = state.version, id = state.lastId;
    if (!await this.orientation.beforeSpeak(chat)) { state.due = this.now() + 5; return; }
    if (version !== state.version || this.controller.signal.aborted || !this.available(this.now())) return;
    const activityStarted = this.activity.snapshot(this.now()).started;
    const orientationEpoch = this.orientation.get(chat)?.epoch;
    if (a.sending.enabled && this.store.assessment(chat, id)) { this.finish(state, chat, id, version, trigger, true); return; }
    const hint = trigger === 'pause' ? 'open' : state.hint;
    state.lastThink = now; this.lastCycle = now;
    const profile = this.store.learningState(chat);
    const obsolete = () => this.controller.signal.aborted || version !== state.version || activityStarted !== this.activity.snapshot(this.now()).started || profile.epoch !== this.store.learningState(chat).epoch || orientationEpoch !== this.orientation.get(chat)?.epoch;
    const history = this.store.history(chat, a.learning.enabled ? Math.max(a.historyLimit, a.learning.minMessages) : a.historyLimit);
    const last = history.filter(x => !x.self).at(-1);
    if (!last) return;
    const counts = this.store.counts(chat, now);
    const gated = counts.total >= a.maxMessagesPerHour || (hint !== 'self' &&
      (counts.proactive >= a.maxProactivePerHour || now - counts.last < a.proactiveCooldownSeconds));
    if (gated) { this.finish(state, chat, id, version, trigger); return; }
    const signal = this.controller.signal;
    const humans = history.filter(x => !x.self);
    const newHumans = humans.slice(humans.findLastIndex(x => x.id === profile.last_id) + 1);
    const learnNow = a.learning.enabled && last.id !== profile.last_id && newHumans.length >= a.learning.minMessages && now - profile.updated >= a.learning.intervalSeconds;
    const query = humans.slice(-3).map(x => x.text).join(' ');
    const memoryContext = () => a.learning.enabled ? this.store.memory.context(chat, last.sender, now, a.memory, query) : [];
    const retrieve = () => this.store.retrieveScoped(chat, last.sender, query, now, a.memory,
      { excludeIds: history.map(x => x.id), enabled: a.learning.enabled, limit: a.learning.retrievalLimit });
    const chatStyle = context => context.map(scope => ({ subject: scope.subject, traits: scope.traits.map(m => ({ key: m.slot, text: m.text, sources: m.sources })) }));
    const initialMemory = memoryContext();
    const payload = { persona: a.persona, name: a.name, trigger, addressedHint: hint,
      groupOrientation: this.orientation.profile(chat),
      chatStyle: chatStyle(initialMemory),
      memoryContext: initialMemory,
      learning: { requested: learnNow, subjects: initialMemory.map(s => s.subject), currentSpeaker: last.sender },
      history: history.map(x => ({ id: x.id, sender: x.sender, self: !!x.self, timestamp: x.ts, speaker: x.self ? a.name : x.name, text: x.text })),
      memories: retrieve(),
      retainedIdeas: this.store.reservoir(chat, now, a.thoughtTtlSeconds, a.thoughtLimit, last.sender),
      priorExpectation: this.store.expectation(chat, now),
    };
    const formed = await this.provider.json(formation, payload, signal);
    if (!Array.isArray(formed.candidates) || !['self', 'other', 'open'].includes(formed.allocation)) throw Object.assign(Error('Invalid formation'), { code: 'invalid_formation' });
    if (obsolete() || !this.available(this.now())) return;
    if (learnNow && formed.learning !== undefined) {
      try {
        const updates = parseMemoryUpdates(formed.learning.layers, history, chat, last.sender, a.memory);
        if (this.store.learn(chat, { style: null, memories: [], forgetIds: [] }, now, last.id, a.learning, profile.epoch, { updates, settings: a.memory })) {
          payload.memoryContext = memoryContext();
          payload.chatStyle = chatStyle(payload.memoryContext);
          payload.memories = retrieve();
          this.log('chat_learning_updated', { chat, updates: updates.length });
        }
      } catch { this.log('chat_learning_rejected', { chat }); }
    }
    for (const candidate of formed.candidates.slice(0, 3)) {
      if (!candidate || typeof candidate.text !== 'string' || !candidate.text.trim() || !['system1', 'system2'].includes(candidate.kind)) continue;
      const thought = { text: candidate.text.trim().slice(0, 300), kind: candidate.kind, subject: last.sender };
      const existing = this.store.reservoir(chat, now, a.thoughtTtlSeconds, a.thoughtLimit, last.sender);
      if (!existing.some(x => x.text === thought.text)) this.store.addThought(chat, thought, now);
    }
    if (obsolete() || !this.available(this.now())) return; // obsolete context or schedule
    const candidates = this.store.reservoir(chat, now, a.thoughtTtlSeconds, a.thoughtLimit, last.sender);
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
    if (obsolete() || !this.available(this.now())) return;
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
    let prediction = null;
    if (a.sending.enabled) {
      const timing = { proactive, age: Math.max(0, this.now() - state.lastHuman),
        ...this.store.sendingTiming(chat, this.now(), a.sending.recoverySeconds), score: selected.adjusted };
      prediction = forecastResult(await this.provider.json(forecast, { ...payload, retainedIdeas: undefined, selectedIdea: selected.text, timing }, signal));
      if (obsolete() || !this.available(this.now())) return;
      const gate = sendingProbability(a.sending, timing, prediction), draw = this.random();
      const admitted = !gate.veto && draw < gate.probability;
      this.store.assess(chat, id, this.now(), admitted ? 'admitted' : 'withheld', { ...gate, draw, timing, prediction });
      this.log('send_assessment', { chat, probability: gate.probability, admitted });
      if (!admitted) {
        this.store.decision(chat, gate.veto || 'probability_withhold', selected.adjusted, [], this.now());
        this.finish(state, chat, id, version, trigger, true); return;
      }
    }
    const response = await this.provider.json(articulationFor(a.replyLanguage), { persona: a.persona, name: a.name,
      history: payload.history, groupOrientation: payload.groupOrientation, chatStyle: payload.chatStyle, memories: payload.memories, memoryContext: payload.memoryContext, selectedIdea: selected.text, responsePlan: prediction,
      priorExpectation: payload.priorExpectation, assertiveTone: a.proactiveTone, maxCharacters: a.maxOutputChars }, signal).catch(error => {
        this.store.assessmentStatus(chat, id, 'generation_failed'); throw error;
      });
    if (typeof response.text !== 'string' || !response.text.trim() || /<\/?(?:think|analysis)>/i.test(response.text)) {
      this.store.assessmentStatus(chat, id, 'generation_failed');
      throw Object.assign(Error('Invalid articulation'), { code: 'invalid_articulation' });
    }
    const text = [...response.text.trim()].slice(0, a.maxOutputChars).join('');
    if (obsolete() || this.now() - state.lastHuman > a.activeWindowSeconds || !this.available(this.now())) {
      this.store.assessmentStatus(chat, id, 'cancelled'); return;
    }
    if (!this.transport.connected || !this.transport.online) {
      this.store.assessmentStatus(chat, id, 'cancelled');
      throw Object.assign(Error('Offline'), { code: 'qq_offline' });
    }
    if ((proactive && quiet(this.now(), a.quietHours)) || repeated(text, history)) {
      this.store.assessmentStatus(chat, id, 'cancelled');
      this.store.use(selected.id); this.finish(state, chat, id, version, trigger); return;
    }
    if (a.dryRun) {
      this.store.assessmentStatus(chat, id, 'dry_run');
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
      this.store.assessmentStatus(chat, id, 'sent');
      if (prediction) this.store.expect(chat, this.now(), a.sending.expectationSeconds, prediction);
      this.store.message({ chat, id: String(sent?.message_id ?? deliveryId), sender: this.transport.selfId, name: a.name, text, ts: this.now(), self: true });
      this.store.decision(chat, 'sent', selected.adjusted, selected.for, this.now());
      this.lastError = null; this.log('message_sent', { chat, proactive });
    } catch (error) {
      this.store.finishDelivery(deliveryId, error.uncertain ? 'uncertain' : 'failed');
      this.store.assessmentStatus(chat, id, error.uncertain ? 'uncertain' : 'failed');
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
