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

// An installed model is never reported missing because a curated entry shares its name.
assert.equal(models.options([entry('gemma3:4b', ['language']), entry('gemma3:4b', ['language', 'vision'], false)], 'language', 'gemma3:4b').length, 1);
assert.equal(models.options([entry('gemma3:4b', [], true), entry('gemma3:4b', ['language'], false)], 'language', 'gemma3:4b').at(-1).label, 'gemma3:4b · not a language model');

// The picker list splits installed from available and filters by kind and text.
const catalog = models.catalog(list, 'language');
assert.deepEqual(catalog.installed.map((item) => item.entry.name), ['qwen2.5:7b']);
assert.deepEqual(catalog.available.map((item) => item.entry.name), ['gemma3:4b']);
assert.deepEqual(models.catalog(list, 'language', 'vision').available.map((item) => item.entry.name), ['gemma3:4b']);
assert.deepEqual(models.catalog(list, 'language', 'formatting').installed.map((item) => item.entry.name), ['qwen2.5:7b']);
// "All kinds" shows misfits, marked so they cannot be picked; speech files never appear.
const all = models.catalog(list, 'language', '', 'all');
assert.deepEqual(all.installed.map((item) => [item.entry.name, item.fits]), [['qwen2.5:7b', true], ['nomic-embed-text:latest', false]]);

console.log('model-picker.test.js: ok');
