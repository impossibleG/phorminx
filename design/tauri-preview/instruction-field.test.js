const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const context = {window: {}};
vm.createContext(context);
vm.runInContext(fs.readFileSync(path.join(__dirname, 'instruction-field.js'), 'utf8'), context);
const text = context.window.InstructionFieldText;

// Stored text round-trips through parts, references included.
const stored = 'Keep names. @{prompt:3} Then @{prompt:12}\nThanks';
const parts = text.parse(stored);
assert.deepEqual(JSON.parse(JSON.stringify(parts)), [
  {type: 'text', value: 'Keep names. '},
  {type: 'prompt', id: 3},
  {type: 'text', value: ' Then '},
  {type: 'prompt', id: 12},
  {type: 'text', value: '\nThanks'},
]);
assert.equal(text.serialize(parts), stored);
assert.equal(text.serialize(text.parse('')), '');
assert.equal(text.serialize(text.parse('mail @{prompt:x}')), 'mail @{prompt:x}');

// "@" opens the menu only at a word start, and the query follows it.
assert.equal(text.mentionQuery('Use @'), '');
assert.equal(text.mentionQuery('Use @mee'), 'mee');
assert.equal(text.mentionQuery('@mee'), 'mee');
assert.equal(text.mentionQuery('mail me@example'), null);
assert.equal(text.mentionQuery('Use @mee ting'), null);

const prompts = [
  {id: 1, name: 'Meeting notes', tags: ['work'], body: 'Summarize decisions.'},
  {id: 2, name: 'Email', tags: ['writing'], body: 'Write an email.'},
];
assert.deepEqual(text.filter(prompts, 'MEET').map((prompt) => prompt.id), [1]);
assert.deepEqual(text.filter(prompts, 'writ').map((prompt) => prompt.id), [2]);
assert.equal(text.filter(prompts, '').length, 2);

console.log('instruction-field: ok');
