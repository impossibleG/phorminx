const assert = require('node:assert/strict');
const {commentaryParts, hasItem, toLocalInput, fromLocalInput, planWhen, nextPlan} = require('./meeting-items.js');

// A final PIN line becomes a suggestion and leaves the text.
assert.deepEqual(commentaryParts('They agreed on Friday.\nPIN: Deadline is Friday'), {body: 'They agreed on Friday.', pins: ['Deadline is Friday']});
// Markdown emphasis around the marker is tolerated.
assert.deepEqual(commentaryParts('Note.\n**PIN:** Budget cap 40k'), {body: 'Note.', pins: ['Budget cap 40k']});
// Text that only mentions a pin stays as it is.
assert.deepEqual(commentaryParts('Use the PIN: 1234 is wrong'), {body: 'Use the PIN: 1234 is wrong', pins: []});
assert.deepEqual(commentaryParts('PIN: '), {body: 'PIN:', pins: []});
assert.deepEqual(commentaryParts(''), {body: '', pins: []});

assert.equal(hasItem([{text: 'Budget cap', done: false}], ' budget CAP '), true);
assert.equal(hasItem([], 'x'), false);

// Local datetime input round-trips.
const local = new Date(2026, 9, 29, 14, 5).getTime();
assert.equal(toLocalInput(local), '2026-10-29T14:05');
assert.equal(fromLocalInput('2026-10-29T14:05'), local);
assert.equal(fromLocalInput(''), null);
assert.equal(toLocalInput(null), '');

const now = new Date(2026, 9, 29, 9, 0).getTime();
assert.equal(planWhen(local, now), 'Today 14:05');
assert.equal(planWhen(new Date(2026, 9, 30, 8, 0).getTime(), now), 'Tomorrow 08:00');
assert.equal(planWhen(new Date(2026, 10, 3, 16, 30).getTime(), now), 'Tue 3 Nov 16:30');
assert.equal(planWhen(new Date(2027, 0, 4, 10, 0).getTime(), now), 'Mon 4 Jan 2027 10:00');
assert.equal(planWhen(null, now), 'Not scheduled');

const soon = {id: 1, sessionId: null, scheduledAtMs: now + 20 * 60000};
const later = {id: 2, sessionId: null, scheduledAtMs: now + 5 * 3600000};
const recorded = {id: 3, sessionId: 9, scheduledAtMs: now};
assert.equal(nextPlan([recorded, soon, later], now), soon);
assert.equal(nextPlan([later], now), null);
assert.equal(nextPlan([{id: 4, sessionId: null, scheduledAtMs: null}], now), null);

console.log('meeting-items tests passed');
