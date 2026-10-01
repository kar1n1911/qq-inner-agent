export function parseLearning(value, history) {
  const humans = new Map(history.filter(m => !m.self).map(m => [m.id, m]));
  const evidence = ids => Array.isArray(ids) && ids.length >= 1 && ids.length <= 4 && ids.every(id => typeof id === 'string' && humans.has(id));
  const text = (v, max) => typeof v === 'string' && v.trim().length > 0 && v.length <= max;
  if (!value || !Array.isArray(value.memories) || value.memories.length > 3 ||
      !Array.isArray(value.forgetIds) || value.forgetIds.length > 3 || value.forgetIds.some(id => typeof id !== 'string' || id.length > 80) ||
      !(value.style === null || (text(value.style?.text, 600) && evidence(value.style.sourceIds)))) throw Error('invalid_learning');
  if (value.memories.some(m => !text(m?.text, 300) || !evidence(m.sourceIds))) throw Error('invalid_learning');
  // Sources are populated from actual messages, never from model-provided author names.
  const sources = ids => [...new Set(ids)].map(id => ({ id, sender: humans.get(id).sender, ts: humans.get(id).ts }));
  return { style: value.style && { text: value.style.text.trim(), sources: sources(value.style.sourceIds) },
    memories: value.memories.map(m => ({ text: m.text.trim(), sources: sources(m.sourceIds) })), forgetIds: value.forgetIds };
}
