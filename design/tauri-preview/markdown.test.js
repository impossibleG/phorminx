const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

class Node {
  constructor(tag = '#fragment', value = '') { this.tag = tag; this.value = value; this.children = []; }
  append(...children) { this.children.push(...children); }
  set textContent(value) { this.children = []; this.value = value; }
  get textContent() { return this.value + this.children.map((child) => child.textContent).join(''); }
}

const document = {
  createDocumentFragment: () => new Node(),
  createElement: (tag) => new Node(tag),
  createTextNode: (text) => new Node('#text', text),
};
const sandbox = { document };
const source = fs.readFileSync(path.join(__dirname, 'markdown.js'), 'utf8');
vm.runInNewContext(`${source}\nglobalThis.renderMarkdown = renderMarkdown;`, sandbox);

const rendered = sandbox.renderMarkdown('# Agenda\n\n- **Launch** on Friday\n- Review `scope`\n\n> Check owner\n\n```js\nconst safe = true;\n```');
assert.deepEqual(rendered.children.map((child) => child.tag), ['h1', 'ul', 'blockquote', 'pre']);
assert.equal(rendered.children[1].children[0].children[0].tag, 'strong');
assert.equal(rendered.children[1].children[1].children[1].tag, 'code');
assert.match(rendered.textContent, /Launch on Friday/);

const hostile = sandbox.renderMarkdown('<img src=x onerror=alert(1)> **safe** <script>alert(2)</script>');
assert.equal(hostile.children[0].tag, 'p');
assert.equal(hostile.children[0].children[0].tag, '#text');
assert.match(hostile.textContent, /<img src=x onerror=alert\(1\)>/);
assert.equal(hostile.children.some((child) => child.tag === 'img' || child.tag === 'script'), false);

console.log('Markdown presentation and inert HTML checks passed.');
