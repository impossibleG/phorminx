const assert = require('node:assert/strict');
const path = require('node:path');

const wizard = require(path.join(__dirname, 'action-wizard.js'));
const at = (source, start, end = start) => ({source, start, end});
const type = (state, text) => [...text].reduce((current, char) => wizard.typeChar(current, char), state);

// Variables become pills and back without loss.
assert.deepEqual(wizard.tokenize('{"a":"x {{text}} y {{delivery_id}}"}').map((part) => part.type), ['text', 'var', 'text', 'var', 'text']);
assert.deepEqual(wizard.tokenize('{{other}}'), [{type: 'text', value: '{{other}}'}]);

// Brackets and quotes close themselves, and typing the closer steps over it.
assert.deepEqual(wizard.typeChar(at('', 0), '{'), at('{}', 1));
assert.deepEqual(type(at('', 0), '{"'), at('{""}', 2));
assert.deepEqual(type(at('', 0), '{"a'), at('{"a"}', 3));
assert.deepEqual(type(at('', 0), '{"a"'), at('{"a"}', 4));
assert.deepEqual(type(at('', 0), '{"a":['), at('{"a":[]}', 6));
assert.deepEqual(type(at('', 0), '{"a":[]'), at('{"a":[]}', 7));
// Inside a string, [ stays single and a quote closes the string.
assert.deepEqual(wizard.typeChar(at('""', 1), '['), at('"["', 2));
// An escaped quote is typed as is.
assert.deepEqual(wizard.typeChar(at('"a\\"', 3), '"'), at('"a\\""', 4));

// Backspace removes an empty pair, and a whole variable at once.
assert.deepEqual(wizard.deleteBackward(at('{}', 1)), at('', 0));
assert.deepEqual(wizard.deleteBackward(at('"{{text}}"', 9)), at('""', 1));
assert.deepEqual(wizard.deleteForward(at('"{{text}}"', 1)), at('""', 1));
assert.deepEqual(wizard.deleteBackward(at('ab', 2)), at('a', 1));

// Enter between brackets opens an indented line.
assert.deepEqual(wizard.newline(at('{}', 1)), at('{\n  \n}', 4));
assert.deepEqual(wizard.newline(at('{\n  "a": 1', 10)), at('{\n  "a": 1\n  ', 13));

// "{{" and "@" open the variable menu; e-mail addresses do not.
assert.deepEqual(wizard.findTrigger('"{{tr}}"', 5), {kind: 'braces', from: 1, query: 'tr'});
assert.deepEqual(wizard.findTrigger('"@de"', 4), {kind: 'at', from: 1, query: 'de'});
assert.equal(wizard.findTrigger('"me@example"', 11), null);
assert.deepEqual(wizard.matchVariables('tr').map((variable) => variable.name), ['text']);
assert.deepEqual(wizard.matchVariables('deli').map((variable) => variable.name), ['delivery_id']);

// Choosing a variable replaces the trigger, and adds quotes outside a string.
assert.deepEqual(wizard.insertVariable(at('"{{tr}}"', 5), 'text', wizard.findTrigger('"{{tr}}"', 5)), at('"{{text}}"', 9));
assert.deepEqual(wizard.insertVariable(at('{"a":@', 6), 'text', wizard.findTrigger('{"a":@', 6)), at('{"a":"{{text}}"', 15));
assert.deepEqual(wizard.insertVariable(at('{"a":}', 5), 'delivery_id'), at('{"a":"{{delivery_id}}"}', 22));
// Typed in a string, "{{" pairs to "{{}}" and the menu fills the middle.
const braces = type(at('{"a":""}', 6), '{{');
assert.deepEqual(braces, at('{"a":"{{}}"}', 8));
assert.deepEqual(wizard.insertVariable(braces, 'text', wizard.findTrigger(braces.source, braces.start)), at('{"a":"{{text}}"}', 14));

// Template rules match the app's validate_template.
assert.equal(wizard.validateTemplate(wizard.DEFAULT_TEMPLATE).ok, true);
assert.match(wizard.validateTemplate('').error, /Add the JSON/);
assert.match(wizard.validateTemplate('{"a": {{text}}}').error, /inside quotes/);
assert.match(wizard.validateTemplate('{"a": "{{name}}"}').error, /not a variable/);
assert.match(wizard.validateTemplate('{"{{text}}": "x"}').error, /names/);
assert.match(wizard.validateTemplate('{"a": "x"}').warning, /Transcript/);
assert.match(wizard.validateTemplate('{"a": "{{text}}",\n}').error, /^Line 2/);
assert.equal(wizard.formatTemplate('{"a":"{{text}}","b":[1,2]}'), '{\n  "a": "{{text}}",\n  "b": [\n    1,\n    2\n  ]\n}');

// Endpoint rules match the app: https, or http only for a loopback address.
assert.equal(wizard.checkEndpoint('https://example.com/notes').host, 'example.com');
assert.match(wizard.checkEndpoint('https://').error, /Enter the address/);
assert.match(wizard.checkEndpoint('http://example.com').error, /https/);
assert.equal(wizard.checkEndpoint('http://127.0.0.1:8080/hook').ok, true);
assert.equal(wizard.checkEndpoint('http://[::1]:8080/hook').ok, true);
assert.match(wizard.checkEndpoint('http://localhost:8080').error, /127\.0\.0\.1/);
assert.match(wizard.checkEndpoint('https://user:pw@example.com').error, /Security/);
assert.match(wizard.checkEndpoint('https://example.com/#x').error, /#/);
assert.match(wizard.checkEndpoint('https://exa mple.com').error, /spaces/);
assert.equal(wizard.normalizeEndpoint('https://https://example.com'), 'https://example.com');

// Names: required, at most 120 bytes.
assert.match(wizard.checkName('  ').error, /name/);
assert.match(wizard.checkName('é'.repeat(61)).error, /shorter/);
assert.equal(wizard.checkName('Send a note').ok, true);

// Headers: valid names, no reserved or duplicate ones, at most 24.
assert.equal(wizard.checkHeaders([{name: 'X-API-Key', value: 'a'}]).ok, true);
assert.match(wizard.checkHeaders([{name: 'Authorization', value: 'a'}]).error, /Authorization field/);
assert.match(wizard.checkHeaders([{name: 'Content-Type', value: 'a'}]).error, /sets Content-Type/);
assert.match(wizard.checkHeaders([{name: 'X-A', value: '1'}, {name: 'x-a', value: '2'}]).error, /twice/);
assert.match(wizard.checkHeaders([{name: 'Bad Name', value: '1'}]).error, /valid header name/);
assert.match(wizard.checkHeaders(Array.from({length: 25}, (_, index) => ({name: `X-${index}`, value: ''}))).error, /24/);
assert.equal(wizard.headersJson([{name: ' X-A ', value: '1'}, {name: '', value: ''}]), '{"X-A":"1"}');
assert.equal(wizard.headersJson([]), '');

// Authorization values.
assert.equal(wizard.buildAuthorization('Bearer', ' Bearer abc ', '', ''), 'Bearer abc');
assert.equal(wizard.buildAuthorization('Basic', '', 'ana', 'sé'), `Basic ${Buffer.from('ana:sé').toString('base64')}`);
assert.equal(wizard.buildAuthorization('custom', ' Token x ', '', ''), 'Token x');
assert.equal(wizard.buildAuthorization('', 'x', '', ''), '');
assert.match(wizard.checkAuthorization('Bearer', '', '', '', false).error, /token/);
assert.equal(wizard.checkAuthorization('Bearer', '', '', '', true).ok, true);
assert.match(wizard.checkAuthorization('Basic', '', 'a:b', 'c', false).error, /colon/);

console.log('Action wizard editing and validation checks passed.');
