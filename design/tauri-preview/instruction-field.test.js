const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const context = {window: {}};
vm.createContext(context);
vm.runInContext(fs.readFileSync(path.join(__dirname, 'instruction-field.js'), 'utf8'), context);
const text = context.window.InstructionFieldText;
const plain = (value) => JSON.parse(JSON.stringify(value));

// References are found in order, once each, by name (any case) or old id.
assert.deepEqual(plain(text.references('Keep names. @[Short email] then @[short email], @{prompt:12}\n@[Lists]')), [
  {name: 'Short email'},
  {id: 12},
  {name: 'Lists'},
]);
assert.deepEqual(plain(text.references('mail me@example.com @[] @[ ] @[a[b] @[open @{prompt:x} @{prompt:0}')), []);
assert.deepEqual(plain(text.references(`@[${'n'.repeat(81)}]`)), []);
assert.equal(text.token(' Short email '), '@[Short email]');

const prompts = [
  {id: 1, name: 'Meeting notes', tags: ['work'], body: 'Summarize decisions.'},
  {id: 2, name: 'Email', tags: ['writing'], body: 'Write an email.'},
];
assert.equal(text.find(prompts, {name: 'meeting NOTES'}).id, 1);
assert.equal(text.find(prompts, {id: 2}).name, 'Email');
assert.equal(text.find(prompts, {name: 'Gone'}), undefined);

// "@" opens suggestions only at a word start; the query follows it.
assert.deepEqual(plain(text.mention('Use @')), {query: '', start: 4});
assert.deepEqual(plain(text.mention('Use @mee')), {query: 'mee', start: 4});
assert.deepEqual(plain(text.mention('@mee')), {query: 'mee', start: 0});
assert.equal(text.mention('mail me@example'), null);
assert.equal(text.mention('Use @mee ting'), null);
assert.equal(text.mention('Use @[Email] '), null);

assert.deepEqual(text.filter(prompts, 'MEET').map((prompt) => prompt.id), [1]);
assert.deepEqual(text.filter(prompts, 'writ').map((prompt) => prompt.id), [2]);
assert.equal(text.filter(prompts, '').length, 2);

// Token ranges index the textarea value directly.
const value = 'Use @[Email] and @{prompt:1}.';
assert.deepEqual(plain(text.tokens(value)), [
  {name: 'Email', start: 4, end: 12},
  {id: 1, start: 17, end: 28},
]);
const tokens = text.tokens(value);

// The caret never rests inside a reference.
assert.equal(text.snap(4, tokens), 4);
assert.equal(text.snap(12, tokens), 12);
assert.equal(text.snap(6, tokens), 4);
assert.equal(text.snap(10, tokens), 12);
assert.equal(text.snap(10, tokens, -1), 4);
assert.equal(text.snap(5, tokens, 1), 12);
assert.equal(text.snap(14, tokens, 1), 14);

// Selections that touch a reference cover all of it.
assert.deepEqual(plain(text.expand(6, 8, tokens)), [4, 12]);
assert.deepEqual(plain(text.expand(0, 6, tokens)), [0, 12]);
assert.deepEqual(plain(text.expand(10, 20, tokens)), [4, 28]);
assert.deepEqual(plain(text.expand(12, 17, tokens)), [12, 17]);
assert.deepEqual(plain(text.expand(0, value.length, tokens)), [0, value.length]);

// Backspace after, or Delete before, a reference removes it whole.
assert.deepEqual(plain(text.deletion(12, 'Backspace', tokens)), {name: 'Email', start: 4, end: 12});
assert.equal(text.deletion(4, 'Backspace', tokens), null);
assert.deepEqual(plain(text.deletion(4, 'Delete', tokens)), {name: 'Email', start: 4, end: 12});
assert.equal(text.deletion(12, 'Delete', tokens), null);
assert.equal(text.deletion(14, 'Backspace', tokens), null);

// Old id references become readable names when the prompt exists.
assert.equal(text.upgradeLegacy('A @{prompt:2} B @{prompt:9}', prompts), 'A @[Email] B @{prompt:9}');

// The component must not replace the textarea with an editable element.
const source = fs.readFileSync(path.join(__dirname, 'instruction-field.js'), 'utf8');
assert.ok(!/contentEditable|contenteditable/.test(source));

console.log('instruction-field: ok');
