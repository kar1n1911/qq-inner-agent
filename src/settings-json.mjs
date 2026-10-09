import { isDeepStrictEqual } from 'node:util';

const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);

// Append defaults to the file's own object order, including at nested levels.
export function withDefaults(value, shape) {
  const result = structuredClone(value);
  for (const [key, fallback] of Object.entries(shape)) {
    if (!Object.hasOwn(result, key)) Object.defineProperty(result, key, { value: structuredClone(fallback), writable: true, enumerable: true, configurable: true });
    else if (object(result[key]) && object(fallback)) result[key] = withDefaults(result[key], fallback);
  }
  return result;
}

// Parse offsets, not a reserialized tree. JSON.parse first validates the grammar;
// the token walk retains whitespace, escapes, number spelling and property order.
export function editJson(source, target) {
  const original = JSON.parse(source);
  const tokens = [...source.matchAll(/"(?:\\[\s\S]|[^"\\])*"|-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?|true|false|null|[{}\[\],:]/g)];
  let index = 0;
  function node() {
    const token = tokens[index++], start = token.index, children = new Map();
    if (token[0] === '{') {
      while (tokens[index][0] !== '}') {
        const key = JSON.parse(tokens[index++][0]); index++; // colon
        if (children.has(key)) throw Error('Duplicate configuration key');
        children.set(key, node());
        if (tokens[index][0] === ',') index++;
      }
      return { start, end: tokens[index++].index + 1, children };
    }
    if (token[0] === '[') {
      while (tokens[index][0] !== ']') { node(); if (tokens[index][0] === ',') index++; }
      return { start, end: tokens[index++].index + 1 };
    }
    return { start, end: start + token[0].length };
  }
  const tree = node(), edits = [], newline = source.includes('\r\n') ? '\r\n' : '\n';
  const unit = source.match(/\n([\t ]+)"/)?.[1] || '  ';
  const indentAt = offset => source.slice(source.lastIndexOf('\n', offset - 1) + 1, offset).match(/^[\t ]*/)[0];
  const render = (value, indent) => JSON.stringify(value, null, unit).replace(/\n/g, newline + indent);
  function walk(n, before, after) {
    if (isDeepStrictEqual(before, after)) return;
    if (object(before) && object(after)) {
      const additions = [];
      for (const [key, value] of Object.entries(after)) {
        if (n.children.has(key)) walk(n.children.get(key), before[key], value);
        else additions.push([key, value]);
      }
      if (additions.length) {
        const children = [...n.children.values()], last = children.at(-1);
        const indent = children.length ? indentAt(children[0].start) : indentAt(n.start) + unit;
        const text = additions.map(([key, value]) => `${JSON.stringify(key)}: ${render(value, indent)}`).join(',' + newline + indent);
        edits.push({ start: last?.end ?? n.start + 1, end: last?.end ?? n.start + 1,
          text: (last ? ',' : '') + newline + indent + text + (last ? '' : newline + indentAt(n.start)) });
      }
    } else edits.push({ start: n.start, end: n.end, text: render(after, indentAt(n.start)) });
  }
  walk(tree, original, target);
  for (const edit of edits.sort((a, b) => b.start - a.start)) source = source.slice(0, edit.start) + edit.text + source.slice(edit.end);
  return source;
}
