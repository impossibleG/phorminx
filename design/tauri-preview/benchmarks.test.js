const assert = require('node:assert/strict');
const bench = require('./benchmarks.js');

const run = (id, feature, model, config, metrics) => ({
  id, feature, model, backend: 'cpu', config, repeats: 1,
  metrics: Object.fromEntries(Object.entries(metrics).map(([name, [value, unit]]) => [name, { value, unit }])),
});

const runs = [
  run('a', 'formatting', 'm1', { mode: 'light', passes: 1, language: 'en', start: 'warm' }, { latency: [1, 's'], quality: [90, 'score'] }),
  run('b', 'formatting', 'm1', { mode: 'light', passes: 1, language: 'pt', start: 'warm' }, { latency: [3, 's'], quality: [70, 'score'] }),
  run('c', 'formatting', 'm2', { mode: 'light', passes: 2, language: 'en', start: 'cold' }, { latency: [9, 's'] }),
  run('d', 'dictation', 'v', { engine: 'instant', language: 'en', start: 'warm' }, { wer: [5, '%'] }),
];

// Units.
assert.equal(bench.format(1.234, 's'), '1.23 s');
assert.equal(bench.format(12.34, 's'), '12.3 s');
assert.equal(bench.format(150, 's'), '150 s');
assert.equal(bench.format(4.25, '%'), '4.3%');
assert.equal(bench.format(42.4, '%'), '42%');
assert.equal(bench.format(0.2, 'x'), '0.20×');
assert.equal(bench.format(87.6, 'score'), '88/100');
assert.equal(bench.format(null, 's'), '—');

// Filters keep the feature and every selected value; missing keys pass.
assert.deepEqual(bench.filterRuns(runs, 'formatting', {}).map((r) => r.id), ['a', 'b', 'c']);
assert.deepEqual(bench.filterRuns(runs, 'formatting', { language: new Set(['en']) }).map((r) => r.id), ['a', 'c']);
assert.deepEqual(bench.filterRuns(runs, 'formatting', { model: new Set(['m2']) }).map((r) => r.id), ['c']);
assert.deepEqual(bench.filterRuns(runs, 'dictation', { mode: new Set(['strong']) }).map((r) => r.id), ['d']);
assert.deepEqual(bench.where(runs, { start: 'warm', language: 'en' }).map((r) => r.id), ['a', 'd']);

// Averages skip runs without the metric.
assert.equal(bench.average(runs.slice(0, 3), 'latency'), 13 / 3);
assert.equal(bench.average(runs.slice(0, 3), 'quality'), 80);
assert.equal(bench.average([], 'quality'), null);

// Descriptions name the model, every config value and the backend.
assert.equal(bench.describe(runs[2]), 'm2 · Light · 2 passes · English · Cold · cpu');

// Models keep first-seen order, so colors follow the model, not its rank.
assert.deepEqual(bench.modelOrder({ runs }), ['m1', 'm2', 'v']);

// Trend points come from history, oldest first.
const file = { id: 'new', date: '2026-10-19', history: [
  { id: 'new', date: '2026-10-19', appVersion: '0.2.3', keyMetrics: { k: 2 } },
  { id: 'old', date: '2026-09-01', appVersion: '0.2.0', keyMetrics: { k: 3 } },
  { id: 'none', date: '2026-08-01', appVersion: '0.1.0', keyMetrics: {} },
] };
assert.deepEqual(bench.trendPoints(file, 'k').map((p) => [p.id, p.value]), [['old', 3], ['new', 2]]);

// Word diff marks insertions and deletions, ignoring case and punctuation.
assert.deepEqual(bench.wordDiff('so um the cat', 'So the cat.'), [
  { type: 'same', text: 'So' },
  { type: 'del', text: 'um' },
  { type: 'same', text: 'the cat.' },
]);
assert.deepEqual(bench.wordDiff('', 'new'), [{ type: 'ins', text: 'new' }]);

console.log('benchmarks.test.js: ok');
