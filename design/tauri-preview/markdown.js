// Small, inert Markdown presentation for local assistant replies. Build DOM
// nodes from text; never interpret model output as HTML or load remote images.
function renderMarkdown(text) {
  const fragment = document.createDocumentFragment();
  const lines = String(text || '').replace(/\r\n?/g, '\n').split('\n');
  const inlinePattern = /(`[^`\n]+`|\*\*[^*\n]+\*\*|__[^_\n]+__|~~[^~\n]+~~|\*[^*\n]+\*|_[^_\n]+_)/g;

  function inline(parent, value, depth = 0) {
    if (depth >= 4) { parent.append(document.createTextNode(value)); return; }
    let cursor = 0;
    for (const match of value.matchAll(inlinePattern)) {
      if (match.index > cursor) parent.append(document.createTextNode(value.slice(cursor, match.index)));
      const token = match[0];
      const delimiter = token.startsWith('**') || token.startsWith('__') || token.startsWith('~~') ? 2 : 1;
      const tag = token[0] === '`' ? 'code' : token.startsWith('~~') ? 'del' : delimiter === 2 ? 'strong' : 'em';
      const element = document.createElement(tag);
      const content = token.slice(delimiter, -delimiter);
      if (tag === 'code') element.textContent = content;
      else inline(element, content, depth + 1);
      parent.append(element);
      cursor = match.index + token.length;
    }
    if (cursor < value.length) parent.append(document.createTextNode(value.slice(cursor)));
  }

  function block(tag, value) {
    const element = document.createElement(tag);
    inline(element, value);
    fragment.append(element);
  }

  function beginsBlock(line) {
    return /^\s*(?:```|#{1,6}\s|>\s?|[-*+]\s|\d+[.)]\s|---+\s*$)/.test(line);
  }

  for (let index = 0; index < lines.length;) {
    const line = lines[index];
    if (!line.trim()) { index++; continue; }
    if (/^\s*```/.test(line)) {
      index++;
      const code = [];
      while (index < lines.length && !/^\s*```/.test(lines[index])) code.push(lines[index++]);
      if (index < lines.length) index++;
      const pre = document.createElement('pre');
      const content = document.createElement('code');
      content.textContent = code.join('\n');
      pre.append(content);
      fragment.append(pre);
      continue;
    }
    const heading = line.match(/^\s*(#{1,6})\s+(.+)$/);
    if (heading) { block(`h${heading[1].length}`, heading[2]); index++; continue; }
    if (/^\s*---+\s*$/.test(line)) { fragment.append(document.createElement('hr')); index++; continue; }
    const list = line.match(/^\s*([-*+]|\d+[.)])\s+(.+)$/);
    if (list) {
      const ordered = /\d/.test(list[1][0]);
      const element = document.createElement(ordered ? 'ol' : 'ul');
      while (index < lines.length) {
        const item = lines[index].match(/^\s*([-*+]|\d+[.)])\s+(.+)$/);
        if (!item || /\d/.test(item[1][0]) !== ordered) break;
        const row = document.createElement('li');
        inline(row, item[2]);
        element.append(row);
        index++;
      }
      fragment.append(element);
      continue;
    }
    if (/^\s*>\s?/.test(line)) {
      const quote = [];
      while (index < lines.length && /^\s*>\s?/.test(lines[index])) quote.push(lines[index++].replace(/^\s*>\s?/, ''));
      block('blockquote', quote.join(' '));
      continue;
    }
    const paragraph = [line.trim()];
    index++;
    while (index < lines.length && lines[index].trim() && !beginsBlock(lines[index])) paragraph.push(lines[index++].trim());
    block('p', paragraph.join(' '));
  }
  return fragment;
}
