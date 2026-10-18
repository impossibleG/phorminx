const assert = require('node:assert/strict');
const model = require('./investigation-model.js');

// Labels mirror the Rust card_label rules.
assert.equal(model.invCardLabel({kind: 'link', url: 'https://example.com', title: '', body: ''}), 'https://example.com');
assert.equal(model.invCardLabel({kind: 'link', url: 'https://example.com', linkTitle: 'Example', title: '', body: ''}), 'Example');
assert.equal(model.invCardLabel({kind: 'note', title: '  My   title ', body: 'x'}), 'My title');
assert.equal(model.invCardLabel({kind: 'image', title: '', body: ''}), 'Screenshot');
assert.equal([...model.invCardLabel({kind: 'note', title: '', body: `\n${'é'.repeat(100)}`})].length, 80);
assert.equal(model.invKindLabel('voice'), 'Voice note');
assert.equal(model.invKindLabel('other'), 'Card');

// Citations split into text and id groups; malformed brackets stay text.
assert.deepEqual(model.invSplitCitations('Rose [#7] then fell [#3, #9]. [x] [#]'), [
  {text: 'Rose '},
  {cite: [7], raw: '[#7]'},
  {text: ' then fell '},
  {cite: [3, 9], raw: '[#3, #9]'},
  {text: '. [x] [#]'},
]);
assert.deepEqual(model.invSplitCitations(''), []);

// Filtering by kind and by the host's exact-search matches.
const items = [{id: 1, kind: 'note'}, {id: 2, kind: 'link'}, {id: 3, kind: 'note'}];
assert.deepEqual(model.invVisibleItems(items, 'all', null).map((i) => i.id), [1, 2, 3]);
assert.deepEqual(model.invVisibleItems(items, 'note', null).map((i) => i.id), [1, 3]);
assert.deepEqual(model.invVisibleItems(items, 'all', new Set([2, 3])).map((i) => i.id), [2, 3]);

// Day labels.
const now = new Date(2026, 9, 17, 9, 0).getTime();
assert.equal(model.invDayLabel(new Date(2026, 9, 17, 0, 5).getTime(), now), 'Today');
assert.equal(model.invDayLabel(new Date(2026, 9, 16, 23, 59).getTime(), now), 'Yesterday');
assert.notEqual(model.invDayLabel(new Date(2026, 9, 12).getTime(), now), 'Yesterday');

// Zoom keeps the pointer's world point fixed and clamps.
const view = {x: 100, y: 50, k: 1};
const before = model.invScreenToWorld(view, 300, 200);
const zoomed = model.invZoomAt(view, 1.5, 300, 200);
const after = model.invScreenToWorld(zoomed, 300, 200);
assert.ok(Math.abs(before.x - after.x) < 1e-9 && Math.abs(before.y - after.y) < 1e-9);
assert.equal(model.invZoomAt({x: 0, y: 0, k: 2}, 10, 0, 0).k, 2.5);
assert.equal(model.invClampZoom(NaN), 1);

// Fit centres the boxes and never zooms past 1.2.
const fit = model.invFitView([{x: 0, y: 0, w: 100, h: 50}, {x: 300, y: 200, w: 100, h: 50}], 1000, 800);
assert.equal(fit.k, 1.2);
assert.ok(Math.abs(fit.x + 200 * fit.k - 500) < 1e-9);
assert.deepEqual(model.invFitView([], 100, 100), {x: 40, y: 40, k: 1});

// Edges end on box borders.
const path = model.invEdgePath({x: 0, y: 0, w: 100, h: 40}, {x: 300, y: 0, w: 100, h: 40});
assert.ok(path.startsWith('M100.0,20.0'), path);
assert.ok(path.endsWith('300.0,20.0'), path);
assert.deepEqual(model.invMidpoint({x: 0, y: 0, w: 10, h: 10}, {x: 10, y: 10, w: 10, h: 10}), {x: 10, y: 10});

// Layout parsing rejects junk.
assert.deepEqual(model.invParseLayout('{"x":1,"y":2,"k":9}'), {x: 1, y: 2, k: 2.5});
assert.equal(model.invParseLayout('{"x":"a"}'), null);
assert.equal(model.invParseLayout('not json'), null);

// Shortcut capture text matches the native parser's canonical form.
assert.equal(model.invShortcutFromEvent({code: 'KeyI', ctrlKey: true, altKey: true, shiftKey: true}), 'Ctrl+Alt+Shift+I');
assert.equal(model.invShortcutFromEvent({code: 'F9'}), 'F9');
assert.equal(model.invShortcutFromEvent({code: 'ShiftLeft', shiftKey: true}), null);
assert.equal(model.invFormatBytes(12_609_983), '12.6 MB');

console.log('investigation-model tests passed');
