// Sparse multi-signal recall; all callers must restrict chat/subject before ranking.
export function tokens(text) {
  const result = [];
  for (const word of String(text).toLowerCase().match(/[\p{L}\p{N}]+/gu) || []) {
    if (/\p{Script=Han}/u.test(word)) {
      for (let i = 0; i < word.length - 1; i++) result.push(word.slice(i, i + 2));
    } else if (word.length > 1) result.push(word);
  }
  return result;
}
const overlap = (a, b) => {
  const x = new Set(tokens(a)), y = new Set(tokens(b));
  return x.size && y.size ? [...x].filter(t => y.has(t)).length / Math.sqrt(x.size * y.size) : 0;
};
export function rankMemories(rows, query, now, settings, { requireMatch = false } = {}) {
  const q = [...new Set(tokens(query))];
  const docs = rows.map(row => {
    const terms = tokens(`${row.slot || ''} ${(row.keywords || []).join(' ')} ${row.text}`), counts = new Map();
    for (const t of terms) counts.set(t, (counts.get(t) || 0) + 1);
    return { row, counts, length: terms.length };
  });
  const average = docs.reduce((sum, d) => sum + d.length, 0) / (docs.length || 1) || 1;
  const df = new Map(q.map(t => [t, docs.filter(d => d.counts.has(t)).length]));
  const scored = docs.map(({ row, counts, length }) => {
    let lexical = 0;
    for (const t of q) {
      const tf = counts.get(t) || 0;
      lexical += Math.log(1 + (docs.length - df.get(t) + .5) / (df.get(t) + .5)) * tf * 2.2 / (tf + 1.2 * (.25 + .75 * length / average));
    }
    const recency = 2 ** (-Math.max(0, now - row.updated) / (settings.recallHalfLifeDays * 86400));
    return { ...row, recall: { lexical, recency, confidence: row.confidence ?? .6, importance: row.importance ?? .5 } };
  }).filter(r => r.layer === 'owner_note' || ((r.confidence ?? .6) >= settings.minConfidence && (!requireMatch || r.recall.lexical > 0)));
  // Reciprocal-rank fusion avoids treating lexical scores as calibrated probabilities.
  for (const r of scored) r.saliency = 0;
  for (const [signal, weight] of [['lexical', 3], ['recency', 1], ['importance', 1], ['confidence', 1]]) {
    const sorted = [...scored].sort((a, b) => b.recall[signal] - a.recall[signal] || a.id.localeCompare(b.id));
    let rank = 1;
    sorted.forEach((r, i) => { if (i && r.recall[signal] !== sorted[i-1].recall[signal]) rank = i + 1; r.saliency += weight / (20 + rank); });
  }
  const selected = [], pending = [...scored];
  while (pending.length) {
    const score = r => r.saliency + (r.layer === 'owner_note' ? .04 : 0) - .08 * Math.max(0, ...selected.filter(s => s.subject === r.subject).map(s => overlap(s.text, r.text)));
    pending.sort((a, b) => score(b) - score(a) || a.id.localeCompare(b.id));
    const next = pending.shift();
    if (!selected.some(r => r.subject === next.subject && r.layer === next.layer && r.text === next.text)) selected.push(next);
  }
  return selected;
}
