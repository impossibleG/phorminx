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

// List rows: counts by kind, the playing investigation first.
assert.equal(model.invKindSummary({}), 'Empty');
assert.equal(model.invKindSummary({note: 1, image: 2}), '1 note · 2 screenshots');
assert.equal(model.invKindSummary({voice: 1, text: 2, note: 3, link: 1, answer: 4}), '3 notes · 1 link · 1 voice note · 6 more');
const rows = model.invListRows([
  {id: 1, title: 'Old', updatedAtMs: 10, kinds: {}},
  {id: 2, title: 'New', updatedAtMs: 30, kinds: {note: 2}},
  {id: 3, title: 'Live', updatedAtMs: 20, kinds: {link: 1}},
], 3);
assert.deepEqual(rows.map((row) => [row.id, row.live]), [[3, true], [2, false], [1, false]]);
assert.equal(rows[1].summary, '2 notes');

// Segmented filter shows only kinds present, with counts.
assert.deepEqual(model.invFilterCounts([{kind: 'note'}, {kind: 'image'}, {kind: 'note'}]), [
  {kind: 'all', label: 'All', count: 3},
  {kind: 'note', label: 'Notes', count: 2},
  {kind: 'image', label: 'Screenshots', count: 1},
]);
assert.deepEqual(model.invFilterCounts([]), [{kind: 'all', label: 'All', count: 0}]);

// Relative times.
const at = new Date(2026, 9, 17, 15, 0).getTime();
assert.equal(model.invRelativeTime(at - 20_000, at), 'Just now');
assert.equal(model.invRelativeTime(at - 12 * 60_000, at), '12 min ago');
assert.equal(model.invRelativeTime(at - 3 * 3600_000, at), '3 h ago');
assert.equal(model.invRelativeTime(new Date(2026, 9, 16, 22, 0).getTime(), at), 'Yesterday');

// Capture window validation.
assert.equal(model.invCaptureProblem('new', {text: '  '}), 'Name the investigation.');
assert.equal(model.invCaptureProblem('new', {text: 'Supplier review'}), null);
assert.equal(model.invCaptureProblem('note', {text: ''}), 'Write the note first.');
assert.equal(model.invCaptureProblem('link', {url: 'https://example.com/a?b=1'}), null);
assert.equal(model.invCaptureProblem('link', {url: 'example.com'}), null);
assert.match(model.invCaptureProblem('link', {url: 'not a link'}), /full link/);
assert.match(model.invCaptureProblem('image', {}), /screenshot/);
assert.equal(model.invCaptureProblem('image', {data: 'data:image/png;base64,AA'}), null);

console.log('investigation-model tests passed');
