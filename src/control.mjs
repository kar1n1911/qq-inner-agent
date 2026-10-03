import net from 'node:net';
import path from 'node:path';
import { EventEmitter } from 'node:events';
import { randomUUID } from 'node:crypto';

export const MAX_LINE = 1024 * 1024;
const error = code => Object.assign(new Error(code), { code });

export class ControlClient extends EventEmitter {
  constructor(dataDir, { timeoutMs = 2000, retryMs = 100, maxRetryMs = 5000 } = {}) {
    super();
    Object.assign(this, { socketPath: path.join(dataDir, 'control.sock'), timeoutMs, retryMs, maxRetryMs });
    this.available = false; this.closed = false; this.pending = new Map(); this.delay = retryMs;
    this.connect();
  }
  connect() {
    if (this.closed) return;
    const socket = this.socket = net.createConnection(this.socketPath);
    socket.unref();
    let chunks = [], size = 0;
    const connecting = setTimeout(() => socket.destroy(error('control_connect_timeout')), this.timeoutMs);
    connecting.unref();
    socket.once('connect', () => {
      clearTimeout(connecting); this.available = true; this.delay = this.retryMs; this.emit('available', true);
    });
    socket.on('data', chunk => {
      // 分帧边界按字节计算；半包（含 UTF-8）保留，粘包逐行处理，EOF 不解析残行。
      let start = 0;
      while (start < chunk.length) {
        const end = chunk.indexOf(10, start), stop = end < 0 ? chunk.length : end;
        size += stop - start;
        if (size > MAX_LINE) { socket.destroy(error('control_line_too_long')); return; }
        chunks.push(chunk.subarray(start, stop));
        if (end < 0) break;
        let value;
        try { value = JSON.parse(Buffer.concat(chunks, size).toString('utf8')); }
        catch { socket.destroy(error('control_invalid_json')); return; }
        chunks = []; size = 0; start = end + 1;
        if (!value || typeof value !== 'object' || Array.isArray(value)) { socket.destroy(error('control_invalid_response')); return; }
        if (typeof value.id === 'string') {
          const pending = this.pending.get(value.id);
          if (!pending) continue;
          this.pending.delete(value.id); clearTimeout(pending.timer);
          if (value.ok === true) pending.resolve(value.result);
          else pending.reject(Object.assign(error(value.error?.code || 'control_invalid_response'), { message: value.error?.message || 'control_invalid_response' }));
        } else if (typeof value.event === 'string') this.emit('event', value.event, value.data);
      }
    });
    socket.on('error', () => { this.available = false; });
    socket.once('close', () => {
      clearTimeout(connecting); this.available = false;
      for (const p of this.pending.values()) { clearTimeout(p.timer); p.reject(error('control_disconnected')); }
      this.pending.clear(); this.emit('available', false);
      // 断线重连采用有上限的指数退避；不重放请求，避免重复发送或重复写入。
      if (!this.closed) {
        this.retry = setTimeout(() => this.connect(), this.delay); this.retry.unref();
        this.delay = Math.min(this.maxRetryMs, this.delay * 2);
      }
    });
  }
  subscribe(event, listener) {
    const dispatch = (name, data) => { if (name === event) listener(data); };
    this.on('event', dispatch);
    return () => this.off('event', dispatch);
  }
  async request(method, params = {}, { timeoutMs = this.timeoutMs } = {}) {
    if (!this.available || this.socket.destroyed) throw error('control_unavailable');
    const id = randomUUID(), line = JSON.stringify({ id, method, params });
    if (Buffer.byteLength(line) > MAX_LINE) throw error('control_line_too_long');
    // 慢读服务端不能导致无限写队列；已有请求照常等响应，新请求快速失败。
    if (this.pending.size >= 32 || this.socket.writableNeedDrain) throw error('control_busy');
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id); reject(error('control_timeout'));
        // 超时只结束当前等待；迟到响应按 id 丢弃，不打断同连接上的其它请求。
      }, timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      this.socket.write(line + '\n');
    });
  }
  close() {
    this.closed = true; this.available = false; clearTimeout(this.retry);
    for (const p of this.pending.values()) { clearTimeout(p.timer); p.reject(error('control_closed')); }
    this.pending.clear(); this.socket.destroy();
  }
}
