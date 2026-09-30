import { randomUUID } from 'node:crypto';
import { OneBot } from './onebot.mjs';

// Separate from Engine: diagnostics never invoke the model or store chat history.
export class Diagnostics {
  constructor(load, makeBot = c => new OneBot(c.onebot, c.onebotToken), durationMs = 60000) {
    this.load = load; this.makeBot = makeBot; this.durationMs = durationMs;
    this.capture = null; this.sending = false;
  }
  async connect(onEvent = () => {}) {
    const c = this.load(), bot = this.makeBot(c), abort = new AbortController();
    bot.on('event', onEvent);
    let timer;
    const ready = new Promise((resolve, reject) => {
      bot.on('status', status => {
        if (status === 'connected') resolve();
        if (status === 'qq_offline' || status === 'wrong_qq_account') reject(Error(status));
      });
      timer = setTimeout(() => reject(Error('onebot_connection_timeout')), 10000);
    });
    const running = bot.start(abort.signal).catch(() => {});
    const close = async () => { abort.abort(); await running; };
    try { await ready; return { bot, close, secrets: [c.apiKey, c.onebotToken].filter(Boolean) }; }
    catch (e) { await close(); throw e; }
    finally { clearTimeout(timer); }
  }
  async send() {
    if (this.sending) throw Error('send_test_already_running');
    this.sending = true; let connection;
    try {
      connection = await this.connect();
      const { bot } = connection;
      const text = `[QQ Inner Agent diagnostic ${randomUUID()}] Self-account send test.`;
      // The caller cannot choose a recipient or message. Never retry an uncertain send.
      const result = await bot.send(`private:${bot.selfId}`, text);
      return { account: bot.selfId, messageId: result?.message_id ?? null, text,
        message: 'Bridge accepted the self-message. Check your QQ self-chat to confirm delivery.' };
    } finally { await connection?.close(); this.sending = false; }
  }
  status() {
    const c = this.capture;
    return c ? { state: c.state, account: c.account, until: c.until, events: c.events, error: c.error } : { state: 'idle', events: [] };
  }
  async listen() {
    if (['connecting', 'listening'].includes(this.capture?.state)) throw Error('receive_test_already_running');
    const c = this.capture = { state: 'connecting', events: [], account: '', until: null, error: null };
    try {
      const connection = await this.connect(event => {
        if (c.state !== 'listening' || !['message', 'message_sent'].includes(event.post_type)) return;
        if (String(event.self_id) !== c.account || String(event.user_id) !== c.account) return;
        if (!['private', 'group'].includes(event.message_type)) return;
        const segments = Array.isArray(event.message) ? event.message : [];
        const types = segments.length ? [...new Set(segments.map(s => String(s?.type || 'unknown')))]
          : [...new Set(['text', ...[...String(event.message || '').matchAll(/\[CQ:([a-z_]+)/g)].map(m => m[1])])];
        let text = segments.length ? segments.filter(s => s?.type === 'text').map(s => String(s.data?.text || '')).join('')
          : String(event.message || '').replace(/\[CQ:[^\]]*\]/g, '[attachment]');
        for (const secret of connection.secrets) text = text.split(secret).join('[redacted]');
        c.events.push({ receivedAt: new Date().toISOString(), messageId: String(event.message_id ?? ''),
          postType: event.post_type, chatType: event.message_type, types: types.slice(0, 20), text: text.slice(0, 1000) });
        if (c.events.length > 30) c.events.shift();
      });
      c.connection = connection; c.account = connection.bot.selfId;
      if (c.state === 'stopped') { await connection.close(); return this.status(); }
      c.state = 'listening'; c.until = Date.now() + this.durationMs;
      c.timer = setTimeout(() => { this.stop('finished').catch(() => {}); }, this.durationMs);
      c.timer.unref?.();
      return this.status();
    } catch (e) { c.state = 'failed'; c.error = e.code || e.message; throw e; }
  }
  async stop(state = 'stopped') {
    const c = this.capture;
    if (c) { clearTimeout(c.timer); c.state = state; await c.connection?.close(); }
    return this.status();
  }
}
