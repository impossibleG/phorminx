const assert = require('node:assert/strict');
const {transcriptParts, translationParts} = require('./transcript-lines.js');

// Dual capture: one segment holds several stamped source lines.
assert.deepEqual(
  transcriptParts('[00:03.120 · Computer audio] Good morning\n[00:04.500 · You / microphone] Hi there', 'conversation'),
  [{source: 'computer', text: 'Good morning'}, {source: 'microphone', text: 'Hi there'}],
);
// Single-source text without a prefix keeps its own source and is unchanged.
assert.deepEqual(transcriptParts('Plain words', 'microphone'), [{source: 'microphone', text: 'Plain words'}]);
// Brackets that are not a time prefix stay in the text.
assert.deepEqual(transcriptParts('[laughs] okay', 'computer'), [{source: 'computer', text: '[laughs] okay'}]);
// Long meetings: minutes can exceed two digits.
assert.deepEqual(transcriptParts('[125:09.001 · Computer audio] late', 'conversation'), [{source: 'computer', text: 'late'}]);
// Empty lines are dropped.
assert.deepEqual(transcriptParts('\n[00:01.000 · Computer audio]   \n', 'conversation'), []);
// A translated label still loses its time prefix.
assert.deepEqual(transcriptParts('[00:01.000 · Áudio do computador] Olá', 'conversation'), [{source: 'conversation', text: 'Olá'}]);

assert.deepEqual(
  translationParts('[00:03.120 · Computer audio] Bom dia\n[00:04.500 · You / microphone] Oi', 2),
  ['Bom dia', 'Oi'],
);
// A translation that merged lines is shown once, after the last part.
assert.deepEqual(translationParts('Bom dia, oi', 2), ['', 'Bom dia, oi']);
console.log('transcript-lines tests passed');
