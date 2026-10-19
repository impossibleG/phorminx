const assert = require('node:assert/strict');
const models = require('./model-picker.js');

const entry = (name, tags, installed = true, usedBy = []) => ({ name, family: 'ollama', tags, installed, usedBy, sizeBytes: null, note: '' });
const list = [
  entry('qwen2.5:7b', ['language', 'tools'], true, ['Formatting']),
  entry('nomic-embed-text:latest', ['embedding']),
  entry('gemma3:4b', ['language', 'vision'], false),
  { name: 'ggml-base.en.bin', family: 'whisper', tags: ['speech'], installed: true, usedBy: [], note: '' },
];

// A language slot lists only installed language models, with what uses them.
assert.deepEqual(models.options(list, 'language', '').map((o) => o.label), ['qwen2.5:7b · used by Formatting']);
// An embedding slot never offers a language model, and vice versa.
assert.deepEqual(models.options(list, 'embedding', '').map((o) => o.value), ['nomic-embed-text:latest']);
// A saved value that does not fit is kept and explained, never silently dropped.
assert.deepEqual(models.options(list, 'language', 'nomic-embed-text').at(-1), { value: 'nomic-embed-text', label: 'nomic-embed-text · not a language model', warning: true });
assert.equal(models.options(list, 'language', 'gemma3:4b').at(-1).label, 'gemma3:4b · not installed');
assert.equal(models.options(list, 'language', 'qwen2.5:7b').length, 1);
// Speech files are never offered to Ollama slots.
assert.ok(!models.options(list, 'speech', '').length);
assert.ok(models.sameModel('nomic-embed-text', 'nomic-embed-text:latest'));
assert.ok(!models.sameModel('qwen2.5:7b', 'qwen2.5:1.5b'));

console.log('model-picker.test.js: ok');
