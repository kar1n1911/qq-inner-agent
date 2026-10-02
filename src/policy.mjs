import { similarity } from './store.mjs';

export function allowed(chat, a) {
  const [kind, id] = chat.split(':');
  return kind === 'group' ? a.allowedGroups.includes(id) : kind === 'private' && a.allowedUsers.includes(id);
}
export function quiet(now, hours) {
  if (!hours || hours.start === hours.end) return false;
  const h = Number(new Intl.DateTimeFormat('en-GB', { timeZone: hours.timezone, hour: '2-digit', hourCycle: 'h23' }).format(new Date(now * 1000)));
  return hours.start < hours.end ? h >= hours.start && h < hours.end : h >= hours.start || h < hours.end;
}
export function activeAt(now, schedule) {
  if (!schedule?.enabled) return true;
  const parts = new Intl.DateTimeFormat('en-GB', { timeZone: schedule.timezone, hour: '2-digit', minute: '2-digit', hourCycle: 'h23' }).formatToParts(new Date(now * 1000));
  const minute = Number(parts.find(p => p.type === 'hour').value) * 60 + Number(parts.find(p => p.type === 'minute').value);
  const minutes = value => { const [h, m] = value.split(':').map(Number); return h * 60 + m; };
  const start = minutes(schedule.activeStart), end = minutes(schedule.inactiveStart);
  return start < end ? minute >= start && minute < end : minute >= start || minute < end;
}
function cqDecode(text) { return text.replace(/&#44;/g, ',').replace(/&#91;/g, '[').replace(/&#93;/g, ']').replace(/&amp;/g, '&'); }
export function normalize(event, selfId, a, now) {
  if (event.post_type !== 'message' || !['group', 'private'].includes(event.message_type)) return null;
  if (!event.user_id || !selfId || String(event.user_id) === selfId || (event.self_id && String(event.self_id) !== selfId)) return null;
  if (a.ignoredUsers.includes(String(event.user_id))) return null;
  const id = event.message_type === 'group' ? event.group_id : event.user_id;
  if (!id || event.message_id == null) return null;
  const chat = `${event.message_type}:${id}`;
  if (!allowed(chat, a)) return null;
  const ts = Number(event.time || now);
  // Don't wake the agent with offline history or future-dated packets.
  if (!Number.isFinite(ts) || now - ts > a.activeWindowSeconds || ts > now + 60) return null;
  let text = '', atSelf = false, atOther = false;
  if (Array.isArray(event.message)) {
    for (const seg of event.message) {
      if (seg.type === 'text' && typeof seg.data?.text === 'string') text += seg.data.text;
      else if (seg.type === 'at') {
        if (String(seg.data?.qq) === selfId) atSelf = true;
        else if (String(seg.data?.qq) !== 'all') atOther = true;
        text += ` [@${String(seg.data?.qq || '')}] `;
      } else if (seg.type === 'face' && /^\d{1,5}$/.test(String(seg.data?.id))) text += ` [QQface:${seg.data.id}] `;
      else if (seg.type === 'reply') text += ' [reply] ';
      else text += ` [${String(seg.type || 'attachment').slice(0, 24)}] `;
    }
  } else if (typeof event.message === 'string') {
    text = event.message.replace(/\[CQ:at,qq=(\d+|all)(?:,[^\]]*)?\]/g, (_, id) => {
      if (id === selfId) atSelf = true; else if (id !== 'all') atOther = true;
      return ` [@${id}] `;
    }).replace(/\[CQ:face,id=(\d{1,5})\]/g, '[QQface:$1]').replace(/\[CQ:[^\]]+\]/g, '[attachment]');
    text = cqDecode(text);
  }
  text = text.trim().slice(0, a.maxInputChars);
  if (!text) return null;
  const named = a.aliases.some(alias => text.toLowerCase().startsWith(alias.toLowerCase() + ':') || text.toLowerCase().startsWith(alias.toLowerCase() + '：') || text.toLowerCase().startsWith('@' + alias.toLowerCase() + ' '));
  const addressed = event.message_type === 'private' || atSelf || named;
  return { chat, id: String(event.message_id), sender: String(event.user_id), name: String(event.sender?.card || event.sender?.nickname || event.user_id).slice(0, 80), text, ts: Math.min(ts, now), self: false,
    hint: addressed ? 'self' : atOther ? 'other' : 'open' };
}
export function select(rated, allocation, a, turnsSilent = 0, random = Math.random) {
  if (!rated.length) return null;
  const pool = rated.map(x => ({ ...x, adjusted: Math.min(5, x.motivation * Math.min(1.2, 1.02 ** Math.max(0, turnsSilent))) }))
    .sort((x, y) => y.adjusted - x.adjusted);
  if (allocation === 'self') return pool[0];
  if (!a.proactive) return null;
  const appropriate = pool.filter(x => x.relevance >= 3 && x.originality >= 3);
  const threshold = allocation === 'other' ? a.interruptThreshold : a.threshold;
  const top = appropriate.find(x => x.adjusted >= threshold);
  if (top) return top;
  if (allocation === 'open' && random() < a.system1Probability) return appropriate.find(x => x.kind === 'system1') || null;
  return null;
}
// Reply-length buckets. Uniform reply length is one of the strongest "AI" tells:
// a person's messages vary a lot from one turn to the next, while a model left to
// itself converges on one comfortable size. Asking it to "keep it short" does not
// fix that; sampling an explicit target per turn and naming it in the prompt does.
// A direct request must never be answered with a throwaway one-liner, so `tiny` is
// excluded when the agent is addressed.
const LENGTH_BUCKETS = {
  addressed: [['short', 0.70], ['medium', 0.28], ['long', 0.02]],
  open: [['tiny', 0.35], ['short', 0.45], ['medium', 0.18], ['long', 0.02]],
};
export function pickLengthTarget(hint, random = Math.random) {
  const buckets = hint === 'self' ? LENGTH_BUCKETS.addressed : LENGTH_BUCKETS.open;
  const draw = random();
  let cumulative = 0;
  for (const [name, weight] of buckets) { cumulative += weight; if (draw < cumulative) return name; }
  return 'short';
}
export function repeated(text, history) {
  return history.filter(x => x.self).some(x => x.text.trim() === text.trim() || similarity(text, x.text) > 0.88);
}
