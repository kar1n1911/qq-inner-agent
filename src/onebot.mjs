import { EventEmitter } from 'node:events';
import { randomUUID } from 'node:crypto';
import { setTimeout as sleep } from 'node:timers/promises';

export class OneBotError extends Error {
  constructor(code, uncertain = false) { super(code); this.code = code; this.uncertain = uncertain; }
}
export class OneBot extends EventEmitter {
  constructor(config, token, options = {}) {
    super(); this.config = config; this.token = token;
    this.WebSocket = options.WebSocket || globalThis.WebSocket;
    this.socket = null; this.pending = new Map(); this.connected = false;
    this.online = false; this.selfId = String(config.selfId || ''); this.reconnects = 0;
  }
  async start(signal) {
    let delay = 1;
    while (!signal.aborted) {
      const started = Date.now();
      try { await this.session(signal); }
      catch (e) { if (!signal.aborted) this.emit('status', e.code || 'connection_failed'); }
      this.connected = false; this.online = false;
      for (const { reject } of this.pending.values()) reject(new OneBotError('connection_lost', true));
      this.pending.clear(); this.socket?.close(); this.socket = null;
      if (signal.aborted) break;
      this.reconnects++;
      if (Date.now() - started > 30_000) delay = 1;
      await sleep((delay + Math.random()) * 1000, undefined, { signal }).catch(() => {});
      delay = Math.min(this.config.reconnectMaxSeconds, delay * 2);
    }
  }
  async session(signal) {
    const url = new URL(this.config.url);
    if (this.token) url.searchParams.set('access_token', this.token);
    const ws = this.socket = new this.WebSocket(url);
    const early = [];
    let closedResolve;
    const closed = new Promise(r => { closedResolve = r; });
    ws.addEventListener('close', () => {
      this.connected = false; this.online = false;
      for (const { reject } of this.pending.values()) reject(new OneBotError('connection_lost', true));
      this.pending.clear(); closedResolve();
    });
    // Never log WebSocket error objects: they may contain the token-bearing URL.
    // The native implementation closes failed connections itself. Calling close()
    // from a handshake error callback can recursively dispatch another error.
    ws.addEventListener('error', () => { this.emit('status', 'websocket_error'); });
    ws.addEventListener('message', event => {
      let data;
      try {
        if (typeof event.data !== 'string' || event.data.length > 1_000_000) return;
        data = JSON.parse(event.data);
      } catch { return; }
      if (!data || typeof data !== 'object') return;
      if (data.echo != null && this.pending.has(String(data.echo))) {
        const p = this.pending.get(String(data.echo)); this.pending.delete(String(data.echo));
        if (data.status === 'ok' && Number(data.retcode) === 0) p.resolve(data.data);
        else p.reject(new OneBotError(`onebot_action_failed_${Number(data.retcode) || 'unknown'}`));
      } else if (data.post_type) {
        if (this.connected) this.emit('event', data);
        else if (early.length < 200) early.push(data);
      }
    });
    const abort = () => ws.close();
    signal.addEventListener('abort', abort, { once: true });
    let timer;
    try {
      await new Promise((resolve, reject) => {
        const t = setTimeout(() => { ws.close(); reject(new OneBotError('connect_timeout')); }, this.config.requestTimeoutSeconds * 1000);
        ws.addEventListener('open', () => { clearTimeout(t); resolve(); }, { once: true });
        ws.addEventListener('close', () => { clearTimeout(t); reject(new OneBotError('connect_closed')); }, { once: true });
      });
      const login = await this.call('get_login_info', {});
      if (!login?.user_id) throw new OneBotError('missing_account');
      if (this.config.selfId && String(login.user_id) !== String(this.config.selfId)) throw new OneBotError('wrong_qq_account');
      this.selfId = String(login.user_id);
      const status = await this.call('get_status', {});
      this.online = status?.online === true;
      this.connected = true;
      this.emit('status', this.online ? 'connected' : 'qq_offline');
      for (const event of early) this.emit('event', event);
      let checking = false;
      timer = setInterval(async () => {
        if (checking) return;
        checking = true;
        try { const s = await this.call('get_status', {}); this.online = s?.online === true; }
        catch { this.online = false; ws.close(); }
        finally { checking = false; }
      }, this.config.heartbeatSeconds * 1000);
      await closed;
    } finally { clearInterval(timer); signal.removeEventListener('abort', abort); }
  }
  async call(action, params) {
    const ws = this.socket;
    if (!ws || ws.readyState !== 1) throw new OneBotError('not_connected');
    const echo = randomUUID();
    let timeout;
    try {
      return await new Promise((resolve, reject) => {
        this.pending.set(echo, { resolve, reject });
        timeout = setTimeout(() => {
          this.pending.delete(echo); reject(new OneBotError('action_timeout', true));
        }, this.config.requestTimeoutSeconds * 1000);
        try { ws.send(JSON.stringify({ action, params, echo })); }
        catch { this.pending.delete(echo); reject(new OneBotError('send_failed', true)); }
      });
    } finally { clearTimeout(timeout); this.pending.delete(echo); }
  }
  async send(chat, text) {
    if (!this.connected || !this.online) throw new OneBotError('qq_offline');
    const [type, id] = chat.split(':');
    if (!['group', 'private'].includes(type) || !/^[1-9]\d*$/.test(id)) throw new OneBotError('invalid_chat');
    // Array text segments make model-written CQ codes inert text.
    return this.call(type === 'group' ? 'send_group_msg' : 'send_private_msg', {
      [type === 'group' ? 'group_id' : 'user_id']: Number(id),
      message: [{ type: 'text', data: { text } }], auto_escape: true,
    });
  }
}
