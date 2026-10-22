// Action editor as a four-step wizard: basics, request, payload, security.
// The form keeps its original field ids; this script drives them and checks
// them with the same rules the app applies when it saves an action.
(function () {
  const VARIABLES = [
    {name: 'text', label: 'Transcript', hint: 'What you said'},
    {name: 'delivery_id', label: 'Delivery ID', hint: 'Unique per send, kept on retry'},
  ];
  const VARIABLE_PATTERN = /\{\{(text|delivery_id)\}\}/g;
  const DEFAULT_TEMPLATE = '{\n  "text": "{{text}}",\n  "delivery_id": "{{delivery_id}}"\n}';
  const PAIRS = {'{': '}', '[': ']', '"': '"'};
  const METHODS = ['POST', 'GET', 'PUT', 'PATCH', 'DELETE'];
  const METHOD_HINTS = {
    POST: 'Sends the payload as JSON. Most webhooks expect POST.',
    GET: 'Sends what you said as text and delivery_id in the address. No payload is sent.',
    PUT: 'Sends the payload as JSON, replacing the resource at this address.',
    PATCH: 'Sends the payload as JSON, updating part of the resource at this address.',
    DELETE: 'Sends the payload as JSON with a DELETE request.',
  };
  // Headers the app sets itself or refuses, as checked in ActionConfig::validate.
  const RESERVED_HEADERS = ['authorization', 'host', 'content-length', 'transfer-encoding', 'connection', 'upgrade', 'expect', 'proxy-authorization', 'proxy-connection', 'content-type', 'idempotency-key'];
  const PRESETS = {
    webhook: {name: 'Send to webhook', method: 'POST', endpoint: 'https://', template: DEFAULT_TEMPLATE},
    slack: {name: 'Post to Slack', method: 'POST', endpoint: 'https://hooks.slack.com/services/', template: '{\n  "text": "{{text}}"\n}'},
    discord: {name: 'Post to Discord', method: 'POST', endpoint: 'https://discord.com/api/webhooks/', template: '{\n  "content": "{{text}}"\n}'},
    zapier: {name: 'Send to Zapier', method: 'POST', endpoint: 'https://hooks.zapier.com/hooks/catch/', template: DEFAULT_TEMPLATE},
  };

  // ---- Template text: variables, pairs and indentation, as pure functions. ----

  function tokenize(source) {
    const parts = [];
    let last = 0;
    for (const match of source.matchAll(VARIABLE_PATTERN)) {
      if (match.index > last) parts.push({type: 'text', value: source.slice(last, match.index)});
      parts.push({type: 'var', name: match[1], value: match[0]});
      last = match.index + match[0].length;
    }
    if (last < source.length) parts.push({type: 'text', value: source.slice(last)});
    return parts;
  }

  function variableSpans(source) {
    return [...source.matchAll(VARIABLE_PATTERN)].map((match) => [match.index, match.index + match[0].length]);
  }

  function insideString(source, offset) {
    let inString = false;
    for (let index = 0; index < offset && index < source.length; index++) {
      const char = source[index];
      if (inString && char === '\\') index++;
      else if (char === '"') inString = !inString;
    }
    return inString;
  }

  function replaceRange(source, start, end, text, caret = text.length) {
    const position = start + caret;
    return {source: source.slice(0, start) + text + source.slice(end), start: position, end: position};
  }

  function typeChar(state, char) {
    const {source, start, end} = state;
    const next = source[end] ?? '';
    const previous = source[start - 1] ?? '';
    const inString = insideString(source, start);
    if (start === end && next === char && (char === '}' || char === ']' || char === '"' && inString && previous !== '\\')) {
      return {source, start: start + 1, end: start + 1};
    }
    const pairs = char === '{' || !inString && (char === '[' || char === '"');
    if (pairs && previous !== '\\') {
      if (start !== end) return replaceRange(source, start, end, char + source.slice(start, end) + PAIRS[char], end - start + 2);
      return replaceRange(source, start, end, char + PAIRS[char], 1);
    }
    return replaceRange(source, start, end, char);
  }

  function deleteBackward(state) {
    const {source, start, end} = state;
    if (start !== end) return replaceRange(source, start, end, '');
    if (start === 0) return state;
    const span = variableSpans(source).find(([, spanEnd]) => spanEnd === start);
    if (span) return replaceRange(source, span[0], span[1], '');
    const previous = source[start - 1];
    if (PAIRS[previous] && source[start] === PAIRS[previous]) return replaceRange(source, start - 1, start + 1, '');
    const width = start > 1 && /[\udc00-\udfff]/.test(previous) && /[\ud800-\udbff]/.test(source[start - 2]) ? 2 : 1;
    return replaceRange(source, start - width, start, '');
  }

  function deleteForward(state) {
    const {source, start, end} = state;
    if (start !== end) return replaceRange(source, start, end, '');
    if (start >= source.length) return state;
    const span = variableSpans(source).find(([spanStart]) => spanStart === start);
    if (span) return replaceRange(source, span[0], span[1], '');
    const width = /[\ud800-\udbff]/.test(source[start]) ? 2 : 1;
    return replaceRange(source, start, start + width, '');
  }

  function newline(state) {
    const {source, start, end} = state;
    const lineStart = source.lastIndexOf('\n', start - 1) + 1;
    const indent = source.slice(lineStart, start).match(/^ */)[0];
    const previous = source.slice(lineStart, start).trimEnd().slice(-1);
    const next = source[end] ?? '';
    if ((previous === '{' || previous === '[') && next === PAIRS[previous]) {
      const text = `\n${indent}  \n${indent}`;
      return replaceRange(source, start, end, text, indent.length + 3);
    }
    return replaceRange(source, start, end, `\n${indent}${previous === '{' || previous === '[' ? '  ' : ''}`);
  }

  // A variable menu opens after "{{" or after "@" at the start of a word.
  function findTrigger(source, caret) {
    const before = source.slice(source.lastIndexOf('\n', caret - 1) + 1, caret);
    const braces = before.match(/\{\{([A-Za-z_]{0,20})$/);
    if (braces) return {kind: 'braces', from: caret - braces[0].length, query: braces[1].toLowerCase()};
    const at = before.match(/(^|[^\w@.])@([A-Za-z_]{0,20})$/);
    if (at) return {kind: 'at', from: caret - at[2].length - 1, query: at[2].toLowerCase()};
    return null;
  }

  function matchVariables(query) {
    const words = (variable) => [variable.name, variable.label.toLowerCase().replace(/ /g, '_'), variable.label.toLowerCase()];
    return VARIABLES.filter((variable) => !query || words(variable).some((word) => word.startsWith(query)));
  }

  function insertVariable(state, name, trigger = null) {
    const {source} = state;
    const from = trigger ? trigger.from : state.start;
    let to = state.end;
    if (trigger?.kind === 'braces' && source.startsWith('}}', to)) to += 2;
    const token = `{{${name}}}`;
    if (insideString(source, from)) return replaceRange(source, from, to, token);
    return replaceRange(source, from, to, `"${token}"`);
  }

  function lineAndColumn(source, position) {
    const before = source.slice(0, position);
    const line = before.split('\n').length;
    return {line, column: position - before.lastIndexOf('\n')};
  }

  function jsonErrorMessage(source, error) {
    const message = String(error?.message || error);
    let where = null;
    const lineMatch = message.match(/line (\d+) column (\d+)/);
    const positionMatch = message.match(/position (\d+)/);
    if (lineMatch) where = {line: Number(lineMatch[1]), column: Number(lineMatch[2])};
    else if (positionMatch) where = lineAndColumn(source, Number(positionMatch[1]));
    let reason = message.replace(/^JSON\.parse: /, '').replace(/ in JSON at position \d+.*$/, '').replace(/ \(line \d+ column \d+\)$/, '').replace(/ at line \d+ column \d+.*$/, '');
    if (/end of (JSON|data) input/i.test(message)) reason = 'Something is not closed yet';
    return where ? `Line ${where.line}, column ${where.column}: ${reason}.` : `${reason}.`;
  }

  // Mirrors validate_template in phorminx-assistant: variables only inside string values.
  function validateTemplate(source) {
    if (!source.trim()) return {error: 'Add the JSON to send.'};
    let value;
    try { value = JSON.parse(source); }
    catch (error) {
      if (variableSpans(source).some(([start]) => !insideString(source, start))) return {error: 'Put variables inside quotes, like "{{text}}".'};
      return {error: jsonErrorMessage(source, error)};
    }
    let problem = null;
    const visit = (item) => {
      if (problem) return;
      if (typeof item === 'string') {
        const rest = item.replace(VARIABLE_PATTERN, '');
        const unknown = rest.match(/\{\{[^{}]*\}\}/);
        if (unknown) problem = `${unknown[0]} is not a variable. Use Transcript or Delivery ID.`;
        else if (rest.includes('{{') || rest.includes('}}')) problem = 'A value has {{ or }} that is not a variable.';
      } else if (Array.isArray(item)) item.forEach(visit);
      else if (item && typeof item === 'object') {
        for (const [key, child] of Object.entries(item)) {
          if (key.includes('{{')) { problem = 'Variables cannot be used as names, only as values.'; return; }
          visit(child);
        }
      }
    };
    visit(value);
    if (problem) return {error: problem};
    if (!source.includes('{{text}}')) return {warning: 'What you said is not in this payload. Insert Transcript to send it.'};
    return {ok: true};
  }

  function formatTemplate(source) {
    return JSON.stringify(JSON.parse(source), null, 2);
  }

  // ---- Field checks, mirroring the app's save rules. ----

  function utf8Length(text) {
    return new TextEncoder().encode(text).length;
  }

  function checkName(name) {
    const trimmed = name.trim();
    if (!trimmed) return {error: 'Give the action a name.'};
    if (/[\u0000-\u001f\u007f]/.test(name)) return {error: 'Remove the line breaks or tabs from the name.'};
    if (utf8Length(name) > 120) return {error: 'Use a shorter name.'};
    return {ok: true, note: `${utf8Length(name)} of 120`};
  }

  function normalizeEndpoint(value) {
    return value.replace(/^\s+/, '').replace(/^https?:\/\/(?=https?:\/\/)/i, '');
  }

  function checkEndpoint(value) {
    const endpoint = value.trim();
    if (!endpoint || /^https?:\/\/$/i.test(endpoint)) return {error: 'Enter the address to send to.'};
    if (/\s/.test(endpoint)) return {error: 'Remove the spaces from the address.'};
    if (endpoint.includes('@')) return {error: 'Put user names, passwords and tokens in Security, not in the address.'};
    if (/[#\\]/.test(endpoint)) return {error: 'The address cannot contain # or \\.'};
    if (/[^\x21-\x7e]|[<>"{}|^`]/.test(endpoint)) return {error: 'Encode special characters in the address, for example a space as %20.'};
    let url;
    try { url = new URL(endpoint); }
    catch { return {error: 'This is not a complete web address yet.'}; }
    const host = url.hostname.replace(/^\[|\]$/g, '');
    const loopback = /^127\.\d+\.\d+\.\d+$/.test(host) || host === '::1';
    if (url.protocol === 'http:' && !loopback) return {error: 'Use https://. Plain http:// only works for 127.0.0.1 on this computer.'};
    if (url.protocol !== 'https:' && url.protocol !== 'http:') return {error: 'Use an address that starts with https://.'};
    if (!url.hostname || !/^https?:\/\/[^/?]/i.test(endpoint)) return {error: 'Add the site, for example https://example.com/notes.'};
    if (url.port === '0') return {error: 'Port 0 cannot be used.'};
    if (url.hostname.indexOf('.') < 0 && !loopback && url.hostname !== 'localhost') return {warning: `Sends to ${url.host}. Check the address: it has no domain ending.`};
    return {ok: true, host: url.host, note: `Sends to ${url.host}`};
  }

  function checkHeaders(rows) {
    const filled = rows.filter((row) => row.name.trim() || row.value);
    if (filled.length > 24) return {error: 'Use 24 headers or fewer.'};
    const seen = new Set();
    for (const row of filled) {
      const name = row.name.trim();
      const lower = name.toLowerCase();
      if (!name) return {error: 'Every header needs a name.', row};
      if (name.length > 128 || !/^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/.test(name)) return {error: `"${name}" is not a valid header name. Use letters, digits and dashes.`, row};
      if (RESERVED_HEADERS.includes(lower)) return {error: lower === 'authorization' ? 'Use the Authorization field above instead of a header.' : `Phorminx sets ${name} itself, so it cannot be added here.`, row};
      if (seen.has(lower)) return {error: `${name} is listed twice.`, row};
      if (/[\r\n]/.test(row.value)) return {error: `Remove the line break from the ${name} value.`, row};
      seen.add(lower);
    }
    return {ok: true, count: filled.length};
  }

  function headersJson(rows) {
    const filled = rows.filter((row) => row.name.trim());
    if (!filled.length) return '';
    return JSON.stringify(Object.fromEntries(filled.map((row) => [row.name.trim(), row.value])));
  }

  function base64Utf8(text) {
    let binary = '';
    for (const byte of new TextEncoder().encode(text)) binary += String.fromCharCode(byte);
    return btoa(binary);
  }

  function buildAuthorization(scheme, token, user, password) {
    if (scheme === 'Bearer') return token.trim() ? `Bearer ${token.trim().replace(/^bearer\s+/i, '')}` : '';
    if (scheme === 'Basic') return user || password ? `Basic ${base64Utf8(`${user}:${password}`)}` : '';
    if (scheme === 'custom') return token.trim();
    return '';
  }

  function checkAuthorization(scheme, token, user, password, hasCredential) {
    const value = scheme === 'Basic' ? `${user}${password}` : token;
    if (/[\r\n]/.test(value)) return {error: 'Remove the line break from the credential.'};
    if (scheme === 'Basic' && user.includes(':')) return {error: 'The user name cannot contain a colon.'};
    if (scheme && scheme !== 'keep' && !value.trim()) {
      return hasCredential ? {ok: true, note: 'Empty, so the saved credential is kept.'} : {error: scheme === 'Basic' ? 'Enter the user name and password, or choose None.' : 'Paste the token, or choose None.'};
    }
    return {ok: true};
  }

  const core = {VARIABLES, DEFAULT_TEMPLATE, PRESETS, tokenize, insideString, typeChar, deleteBackward, deleteForward, newline, findTrigger, matchVariables, insertVariable, validateTemplate, formatTemplate, checkName, normalizeEndpoint, checkEndpoint, checkHeaders, headersJson, buildAuthorization, checkAuthorization};
  if (typeof module === 'object' && module.exports) module.exports = core;
  if (typeof document === 'undefined' || !document.getElementById('action-form')) return;

  // ---- Wiring the form. ----

  const byId = (id) => document.getElementById(id);
  const form = byId('action-form');
  const steps = [...form.querySelectorAll('.wizard-step')];
  const editor = byId('action-template-editor');
  const menu = byId('action-var-menu');
  const wizard = {step: 0, visited: new Set([0]), item: null, actions: [], headerRows: [], history: [], future: [], menuIndex: 0, trigger: null, showErrors: new Set()};

  function element(tag, className = '', text = '') {
    const item = document.createElement(tag);
    if (className) item.className = className;
    if (text) item.textContent = text;
    return item;
  }

  function setHint(name, result, fallback = '') {
    const hint = form.querySelector(`[data-hint="${name}"]`);
    if (!hint) return;
    const showError = Boolean(result?.error && wizard.showErrors.has(name));
    hint.textContent = showError ? result.error : result?.warning || result?.note || fallback;
    hint.classList.toggle('error', showError);
    hint.classList.toggle('warning', Boolean(!result?.error && result?.warning));
  }

  // ---- Pill editor: the textarea keeps the JSON; the editor shows variables as pills. ----

  function templateSource() { return byId('action-template').value; }

  function pill(name) {
    const variable = VARIABLES.find((item) => item.name === name);
    const span = element('span', 'var-pill', variable.label);
    span.contentEditable = 'false';
    span.dataset.var = name;
    span.title = `{{${name}}} · ${variable.hint}`;
    return span;
  }

  function renderEditor(source, caret = null) {
    const fragment = document.createDocumentFragment();
    for (const part of tokenize(source)) fragment.append(part.type === 'var' ? pill(part.name) : document.createTextNode(part.value));
    const end = element('br');
    end.dataset.end = '';
    fragment.append(end);
    editor.replaceChildren(fragment);
    if (caret !== null) setCaret(caret);
  }

  function serializeNode(item) {
    if (item.nodeType === Node.TEXT_NODE) return item.data.replace(/ /g, ' ');
    if (item.dataset?.var) return `{{${item.dataset.var}}}`;
    if (item.nodeName === 'BR') return 'end' in item.dataset ? '' : '\n';
    const inner = [...item.childNodes].map(serializeNode).join('');
    return item.nodeName === 'DIV' || item.nodeName === 'P' ? `\n${inner}` : inner;
  }

  function serializeEditor() {
    return [...editor.childNodes].map(serializeNode).join('');
  }

  function offsetOf(container, offset) {
    if (!editor.contains(container)) return null;
    let total = 0;
    const walk = (parent) => {
      for (const child of parent.childNodes) {
        if (child === container) return true;
        if (child.contains(container)) {
          if (child.dataset?.var) { total += serializeNode(child).length; return true; }
          if (walk(child)) return true;
        }
        total += serializeNode(child).length;
      }
      return false;
    };
    if (container === editor) {
      for (const child of [...editor.childNodes].slice(0, offset)) total += serializeNode(child).length;
      return total;
    }
    if (container.nodeType === Node.TEXT_NODE) {
      walk(editor);
      return total + offset;
    }
    if (container.dataset?.var) {
      walk(editor);
      return total + (offset > 0 ? serializeNode(container).length : 0);
    }
    walk(editor);
    for (const child of [...container.childNodes].slice(0, offset)) total += serializeNode(child).length;
    return total;
  }

  function selectionOffsets() {
    const selection = window.getSelection();
    if (!selection.rangeCount) return null;
    const range = selection.getRangeAt(0);
    const start = offsetOf(range.startContainer, range.startOffset);
    const end = offsetOf(range.endContainer, range.endOffset);
    return start === null || end === null ? null : {start: Math.min(start, end), end: Math.max(start, end)};
  }

  function setCaret(position) {
    const range = document.createRange();
    let remaining = position;
    let placed = false;
    for (const child of editor.childNodes) {
      const length = serializeNode(child).length;
      if (child.nodeType === Node.TEXT_NODE && remaining <= length) { range.setStart(child, remaining); placed = true; break; }
      if (remaining < length || remaining === 0) { range.setStartBefore(child); placed = true; break; }
      remaining -= length;
      if (remaining === 0 && child.nextSibling?.nodeType !== Node.TEXT_NODE) { range.setStartAfter(child); placed = true; break; }
    }
    if (!placed) range.setStart(editor, editor.childNodes.length ? editor.childNodes.length - 1 : 0);
    range.collapse(true);
    const selection = window.getSelection();
    selection.removeAllRanges();
    selection.addRange(range);
  }

  function commitTemplate(next, record = true) {
    const previous = templateSource();
    if (record && next.source !== previous) {
      wizard.showErrors.add('template');
      wizard.history.push({source: previous, start: next.before ?? next.start});
      if (wizard.history.length > 200) wizard.history.shift();
      wizard.future = [];
    }
    byId('action-template').value = next.source;
    renderEditor(next.source, next.start);
    updateMenu();
    refresh();
  }

  function applyEdit(operation) {
    const offsets = selectionOffsets() || {start: templateSource().length, end: templateSource().length};
    const state = {source: templateSource(), ...offsets};
    const next = operation(state);
    if (next) commitTemplate({...next, before: offsets.start});
  }

  function undo(redo = false) {
    const from = redo ? wizard.future : wizard.history;
    const to = redo ? wizard.history : wizard.future;
    const entry = from.pop();
    if (!entry) return;
    to.push({source: templateSource(), start: selectionOffsets()?.start ?? 0});
    commitTemplate({source: entry.source, start: Math.min(entry.start, entry.source.length)}, false);
  }

  editor.addEventListener('beforeinput', (event) => {
    if (event.isComposing || event.inputType === 'insertCompositionText') return;
    const type = event.inputType;
    if (type === 'insertText' && event.data) {
      event.preventDefault();
      applyEdit((state) => [...event.data].reduce((current, char) => typeChar(current, char), state));
    } else if (type === 'insertParagraph' || type === 'insertLineBreak') {
      event.preventDefault();
      applyEdit(newline);
    } else if (type === 'deleteContentBackward') {
      event.preventDefault();
      applyEdit(deleteBackward);
    } else if (type === 'deleteContentForward') {
      event.preventDefault();
      applyEdit(deleteForward);
    } else if (type === 'insertFromPaste' || type === 'insertFromDrop' || type === 'insertReplacementText') {
      event.preventDefault();
      const text = (event.dataTransfer?.getData('text/plain') ?? event.data ?? '').replace(/\r\n?/g, '\n');
      applyEdit((state) => replaceRange(state.source, state.start, state.end, text));
    } else if (type === 'deleteByCut') {
      event.preventDefault();
      applyEdit((state) => replaceRange(state.source, state.start, state.end, ''));
    } else if (type === 'historyUndo' || type === 'historyRedo') {
      event.preventDefault();
      undo(type === 'historyRedo');
    }
  });

  // Anything the browser edits itself (word deletion, composition) is read back from the page.
  editor.addEventListener('input', (event) => {
    const offsets = selectionOffsets();
    const source = serializeEditor();
    if (source === templateSource()) return;
    if (!event.isComposing) wizard.history.push({source: templateSource(), start: offsets?.start ?? 0});
    byId('action-template').value = source;
    if (event.isComposing) return;
    renderEditor(source, offsets?.start ?? source.length);
    updateMenu();
    refresh();
  });
  editor.addEventListener('compositionend', () => {
    const offsets = selectionOffsets();
    renderEditor(templateSource(), offsets?.start ?? templateSource().length);
    refresh();
  });

  editor.addEventListener('copy', (event) => {
    const offsets = selectionOffsets();
    if (!offsets || offsets.start === offsets.end) return;
    event.preventDefault();
    event.clipboardData.setData('text/plain', templateSource().slice(offsets.start, offsets.end));
  });
  editor.addEventListener('cut', (event) => {
    const offsets = selectionOffsets();
    if (!offsets || offsets.start === offsets.end) return;
    event.clipboardData.setData('text/plain', templateSource().slice(offsets.start, offsets.end));
  });

  editor.addEventListener('keydown', (event) => {
    if (!menu.hidden) {
      const options = [...menu.children];
      if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
        event.preventDefault();
        wizard.menuIndex = (wizard.menuIndex + (event.key === 'ArrowDown' ? 1 : options.length - 1)) % options.length;
        paintMenu();
        return;
      }
      if (event.key === 'Enter' || event.key === 'Tab') {
        event.preventDefault();
        chooseVariable(options[wizard.menuIndex].dataset.var);
        return;
      }
      if (event.key === 'Escape') {
        event.preventDefault();
        hideMenu();
        return;
      }
    }
    const mod = event.ctrlKey || event.metaKey;
    if (mod && event.key.toLowerCase() === 'z') { event.preventDefault(); undo(event.shiftKey); }
    else if (mod && event.key.toLowerCase() === 'y') { event.preventDefault(); undo(true); }
    else if (event.key === 'Enter' && event.ctrlKey) { event.preventDefault(); goTo(wizard.step + 1); }
    else if (event.key === 'F' && event.shiftKey && event.altKey) { event.preventDefault(); formatEditor(); }
  });
  editor.addEventListener('keyup', (event) => { if (event.key.startsWith('Arrow') || event.key === 'Home' || event.key === 'End') updateMenu(); });
  editor.addEventListener('click', updateMenu);
  editor.addEventListener('blur', () => setTimeout(() => { if (!menu.contains(document.activeElement)) hideMenu(); }, 120));

  function hideMenu() {
    menu.hidden = true;
    wizard.trigger = null;
    editor.removeAttribute('aria-activedescendant');
  }

  function paintMenu() {
    [...menu.children].forEach((option, index) => option.setAttribute('aria-selected', String(index === wizard.menuIndex)));
    const active = menu.children[wizard.menuIndex];
    if (active) editor.setAttribute('aria-activedescendant', active.id);
  }

  function updateMenu() {
    const offsets = selectionOffsets();
    const trigger = offsets && offsets.start === offsets.end ? findTrigger(templateSource(), offsets.start) : null;
    const matches = trigger ? matchVariables(trigger.query) : [];
    if (!trigger || !matches.length) { hideMenu(); return; }
    if (wizard.trigger?.from !== trigger.from) wizard.menuIndex = 0;
    wizard.trigger = trigger;
    wizard.menuIndex = Math.min(wizard.menuIndex, matches.length - 1);
    menu.replaceChildren(...matches.map((variable) => {
      const option = element('div', 'var-option');
      option.id = `action-var-${variable.name}`;
      option.setAttribute('role', 'option');
      option.dataset.var = variable.name;
      option.append(element('span', 'var-pill', variable.label), element('small', '', variable.hint));
      option.addEventListener('mousedown', (event) => { event.preventDefault(); chooseVariable(variable.name); });
      return option;
    }));
    const range = window.getSelection().getRangeAt(0).cloneRange();
    const rect = range.getClientRects()[0] || editor.getBoundingClientRect();
    const box = editor.parentElement.getBoundingClientRect();
    menu.style.left = `${Math.max(0, Math.min(rect.left - box.left, box.width - 220))}px`;
    menu.style.top = `${rect.bottom - box.top + 4}px`;
    menu.hidden = false;
    paintMenu();
  }

  function chooseVariable(name) {
    const trigger = wizard.trigger;
    hideMenu();
    applyEdit((state) => insertVariable(state, name, trigger));
    editor.focus();
  }

  function formatEditor() {
    const result = validateTemplate(templateSource());
    if (result.error) {
      wizard.showErrors.add('template');
      refresh();
      return;
    }
    commitTemplate({source: formatTemplate(templateSource()), start: 0, before: 0});
    editor.focus();
    setCaret(0);
  }

  form.querySelectorAll('[data-insert-var]').forEach((button) => button.addEventListener('mousedown', (event) => {
    event.preventDefault();
    if (document.activeElement !== editor) { editor.focus(); setCaret(templateSource().length); }
    chooseVariable(button.dataset.insertVar);
  }));
  form.querySelectorAll('[data-insert-var]').forEach((button) => button.addEventListener('keydown', (event) => {
    if (event.key !== 'Enter' && event.key !== ' ') return;
    event.preventDefault();
    editor.focus();
    setCaret(templateSource().length);
    chooseVariable(button.dataset.insertVar);
  }));
  byId('action-template-format').addEventListener('click', formatEditor);

  // ---- Choice controls that drive the hidden fields. ----

  function renderSlots() {
    const taken = new Map(wizard.actions.filter((action) => action.id !== wizard.item?.id && action.launcherSlot).map((action) => [action.launcherSlot, action.name]));
    const current = byId('action-slot').value;
    const choices = [{value: '', label: 'None'}, ...[3, 4, 5, 6, 7, 8, 9].map((slot) => ({value: String(slot), label: String(slot)}))];
    byId('action-slot-choices').replaceChildren(...choices.map((choice) => {
      const button = element('button', 'chip', choice.label);
      button.type = 'button';
      button.setAttribute('role', 'radio');
      button.setAttribute('aria-checked', String(current === choice.value));
      const owner = taken.get(Number(choice.value));
      if (owner) { button.disabled = true; button.title = `Used by ${owner}`; button.setAttribute('aria-label', `${choice.label}, used by ${owner}`); }
      button.addEventListener('click', () => { byId('action-slot').value = choice.value; renderSlots(); refresh(); });
      return button;
    }));
    const used = [...taken.entries()].sort((a, b) => a[0] - b[0]).map(([slot, name]) => `${slot} is ${name}`).join(', ');
    setHint('slot', null, `Press the launcher shortcut, then this key, to record for the action. 1 and 2 are dictation and meetings${used ? `; ${used}` : ''}.`);
  }

  function renderMethods() {
    const method = byId('action-method').value;
    byId('action-method-choices').replaceChildren(...METHODS.map((name) => {
      const button = element('button', '', name);
      button.type = 'button';
      button.setAttribute('role', 'radio');
      button.setAttribute('aria-checked', String(method === name));
      const blocked = name === 'GET' && byId('action-payload-mode').value === 'ai-json';
      if (blocked) button.title = 'Local AI payloads need a method that sends a body';
      button.addEventListener('click', () => {
        byId('action-method').value = name;
        if (name === 'GET' && byId('action-payload-mode').value === 'ai-json') setMode('template', 'Switched to Template: GET cannot carry what the local AI writes.');
        renderMethods();
        refresh();
      });
      return button;
    }));
  }

  function setMode(mode, notice = '') {
    byId('action-payload-mode').value = mode;
    form.querySelectorAll('[data-mode]').forEach((button) => button.setAttribute('aria-checked', String(button.dataset.mode === mode)));
    form.querySelectorAll('[data-mode-panel]').forEach((panel) => { panel.hidden = panel.dataset.modePanel !== mode; });
    wizard.modeNotice = notice;
    renderMethods();
  }

  form.querySelectorAll('[data-mode]').forEach((button) => button.addEventListener('click', () => {
    const mode = button.dataset.mode;
    let notice = '';
    if (mode === 'ai-json' && byId('action-method').value === 'GET') {
      byId('action-method').value = 'POST';
      notice = 'Switched the method to POST, because the local AI writes a body and GET cannot send one.';
    }
    setMode(mode, notice);
    refresh();
  }));

  function fillModels(models) {
    if (window.PhorminxModels) return;
    byId('action-model-options').replaceChildren(...models.map((model) => {
      const option = element('option');
      option.value = model.name;
      return option;
    }));
  }

  form.querySelectorAll('[data-preset]').forEach((button) => button.addEventListener('click', () => {
    const preset = PRESETS[button.dataset.preset];
    if (!byId('action-name').value.trim()) byId('action-name').value = preset.name;
    byId('action-method').value = preset.method;
    byId('action-endpoint').value = preset.endpoint;
    setMode('template');
    commitTemplate({source: preset.template, start: 0, before: 0});
    form.querySelectorAll('[data-preset]').forEach((chip) => chip.setAttribute('aria-pressed', String(chip === button)));
    renderMethods();
    refresh();
  }));

  // ---- Security: authorization and header rows. ----

  function renderAuthSchemes() {
    const select = byId('action-auth-scheme');
    const options = wizard.item?.hasCredential
      ? [['keep', 'Keep the saved credential'], ['Bearer', 'Replace with a bearer token'], ['Basic', 'Replace with user and password'], ['custom', 'Replace with a custom value']]
      : [['', 'None'], ['Bearer', 'Bearer token'], ['Basic', 'User and password (Basic)'], ['custom', 'Custom value']];
    select.replaceChildren(...options.map(([value, label]) => { const option = element('option', '', label); option.value = value; return option; }));
    select.value = options[0][0];
  }

  function syncAuth() {
    const scheme = byId('action-auth-scheme').value;
    const token = byId('action-auth-token');
    form.querySelector('[data-auth="token"]').hidden = !(scheme === 'Bearer' || scheme === 'custom');
    form.querySelector('[data-auth="basic"]').hidden = scheme !== 'Basic';
    byId('action-auth-reveal').hidden = !scheme || scheme === 'keep';
    token.placeholder = scheme === 'custom' ? 'The whole value, for example Token abc123' : 'Paste the token';
    byId('action-auth').value = buildAuthorization(scheme, token.value, byId('action-auth-user').value, byId('action-auth-password').value);
  }

  byId('action-auth-scheme').addEventListener('change', () => { syncAuth(); refresh(); });
  ['action-auth-token', 'action-auth-user', 'action-auth-password'].forEach((id) => byId(id).addEventListener('input', () => {
    const token = byId('action-auth-token');
    if (id === 'action-auth-token' && byId('action-auth-scheme').value === 'custom' && /^bearer\s+\S/i.test(token.value)) {
      byId('action-auth-scheme').value = 'Bearer';
    }
    syncAuth();
    refresh();
  }));
  byId('action-auth-reveal').addEventListener('click', () => {
    const reveal = byId('action-auth-reveal').getAttribute('aria-pressed') !== 'true';
    byId('action-auth-reveal').setAttribute('aria-pressed', String(reveal));
    byId('action-auth-reveal').textContent = reveal ? 'Hide' : 'Show';
    byId('action-auth-token').type = reveal ? 'text' : 'password';
    byId('action-auth-password').type = reveal ? 'text' : 'password';
    form.querySelectorAll('.header-value').forEach((input) => { input.type = reveal ? 'text' : 'password'; });
  });

  function renderHeaderRows(focusIndex = null) {
    const reveal = byId('action-auth-reveal').getAttribute('aria-pressed') === 'true';
    byId('action-header-rows').replaceChildren(...wizard.headerRows.map((row, index) => {
      const line = element('div', 'header-row');
      const name = element('input', 'header-name');
      name.value = row.name;
      name.placeholder = 'Name, for example X-API-Key';
      name.setAttribute('aria-label', `Header ${index + 1} name`);
      name.setAttribute('list', 'action-header-names');
      name.spellcheck = false;
      name.autocomplete = 'off';
      const value = element('input', 'header-value');
      value.type = reveal ? 'text' : 'password';
      value.autocomplete = 'new-password';
      value.value = row.value;
      value.placeholder = 'Value';
      value.setAttribute('aria-label', `Header ${index + 1} value`);
      const remove = element('button', 'header-remove', 'Remove');
      remove.type = 'button';
      remove.setAttribute('aria-label', `Remove header ${index + 1}`);
      name.addEventListener('input', () => { row.name = name.value; syncHeaders(); refresh(); });
      value.addEventListener('input', () => { row.value = value.value; syncHeaders(); refresh(); });
      remove.addEventListener('click', () => { wizard.headerRows.splice(index, 1); renderHeaderRows(); syncHeaders(); refresh(); byId('action-header-add').focus(); });
      line.append(name, value, remove);
      return line;
    }));
    if (focusIndex !== null) byId('action-header-rows').children[focusIndex]?.querySelector('input').focus();
  }

  function syncHeaders() { byId('action-headers').value = headersJson(wizard.headerRows); }

  byId('action-header-add').addEventListener('click', () => {
    if (wizard.headerRows.length >= 24) return;
    wizard.headerRows.push({name: '', value: ''});
    renderHeaderRows(wizard.headerRows.length - 1);
    refresh();
  });

  // ---- Endpoint helpers. ----

  const endpoint = byId('action-endpoint');
  endpoint.addEventListener('focus', () => { if (!endpoint.value) endpoint.value = 'https://'; });
  endpoint.addEventListener('blur', () => { if (/^https?:\/\/$/i.test(endpoint.value)) endpoint.value = ''; refresh(); });
  endpoint.addEventListener('input', () => {
    const normalized = normalizeEndpoint(endpoint.value);
    if (normalized !== endpoint.value) {
      endpoint.value = normalized;
      endpoint.setSelectionRange(normalized.length, normalized.length);
    }
    refresh();
  });
  endpoint.addEventListener('paste', (event) => {
    const text = event.clipboardData.getData('text/plain').trim();
    if (/^https?:\/\//i.test(text) && /^https?:\/\/$/i.test(endpoint.value)) {
      event.preventDefault();
      endpoint.value = text;
      refresh();
    }
  });

  ['action-name', 'action-model', 'action-prompt'].forEach((id) => byId(id).addEventListener('input', refresh));
  ['action-name', 'action-endpoint', 'action-model', 'action-prompt'].forEach((id) => byId(id).addEventListener('blur', () => {
    const hint = {['action-name']: 'name', ['action-endpoint']: 'endpoint', ['action-model']: 'model', ['action-prompt']: 'prompt'}[id];
    if (byId(id).value.trim()) wizard.showErrors.add(hint);
    refresh();
  }));

  // ---- Step checks, progress and navigation. ----

  function endpointChanged() {
    return Boolean(wizard.item) && endpoint.value.trim() !== wizard.item.endpoint;
  }

  function stepChecks() {
    const mode = byId('action-payload-mode').value;
    const method = byId('action-method').value;
    const models = window.PhorminxModels ? [] : [...byId('action-model-options').children].map((option) => option.value);
    const model = byId('action-model').value.trim();
    const changedEndpoint = endpointChanged();
    const modelCheck = !model ? {error: 'Choose the local model that writes the JSON.'}
      : /\s/.test(model) ? {error: 'Model names have no spaces.'}
      : models.length && !models.includes(model) ? {warning: `${model} is not installed. Install it on the Models page first.`}
      : {ok: true, note: models.length ? 'Installed on this computer.' : ''};
    const promptCheck = byId('action-prompt').value.trim() ? {ok: true, note: 'Say which fields to fill and what goes in each.'} : {error: 'Tell the model what JSON to write, for example the field names and what goes in each.'};
    const templateCheck = validateTemplate(templateSource());
    const endpointCheck = checkEndpoint(endpoint.value);
    if (!endpointCheck.error && changedEndpoint && (wizard.item.hasCredential || wizard.item.headerNames?.length)) {
      endpointCheck.warning = `${endpointCheck.note}. Changing the address removes the saved authorization and headers; add them again in Security.`;
    }
    if (!endpointCheck.error && method === 'GET') endpointCheck.note = `${endpointCheck.note}. What you said is added as ?text=…&delivery_id=…`;
    const scheme = byId('action-auth-scheme').value;
    const authCheck = checkAuthorization(scheme === 'keep' ? '' : scheme, byId('action-auth-token').value, byId('action-auth-user').value, byId('action-auth-password').value, Boolean(wizard.item?.hasCredential) && !changedEndpoint);
    const headerCheck = checkHeaders(wizard.headerRows);
    return [
      {name: checkName(byId('action-name').value), slot: {ok: true}},
      {method: {ok: true}, endpoint: endpointCheck},
      mode === 'template' ? {template: method === 'GET' && !templateCheck.error ? {ok: true, note: 'GET sends no body, so this template is only kept for later.'} : templateCheck} : {model: modelCheck, prompt: promptCheck},
      {auth: authCheck, headers: headerCheck},
    ];
  }

  function stepErrors(checks, index) {
    return Object.entries(checks[index]).filter(([, result]) => result?.error);
  }

  function renderReview(checks) {
    const mode = byId('action-payload-mode').value;
    const scheme = byId('action-auth-scheme').value;
    const slot = byId('action-slot').value;
    const keeps = !endpointChanged();
    const authText = byId('action-auth').value ? {Bearer: 'Bearer token', Basic: 'User and password', custom: 'Custom value'}[scheme]
      : wizard.item?.hasCredential ? keeps ? 'Saved credential kept' : 'None, the address changed' : 'None';
    const headerCount = checkHeaders(wizard.headerRows).count || 0;
    const saved = wizard.item?.headerNames?.length && keeps ? wizard.item.headerNames : [];
    const rows = [
      ['Name', byId('action-name').value.trim() || '—'],
      ['Launcher key', slot ? `Launcher, then ${slot}` : 'None'],
      ['Sends', `${byId('action-method').value} ${checks[1].endpoint.host || endpoint.value.trim() || '—'}`],
      ['Payload', mode === 'template' ? 'Template' : `Local AI · ${byId('action-model').value.trim() || 'no model yet'}`],
      ['Authorization', authText],
      ['Headers', headerCount ? `${headerCount} new, replacing any saved ones` : saved.length ? `Kept: ${saved.join(', ')}` : 'None'],
    ];
    byId('action-review').replaceChildren(...rows.flatMap(([term, value]) => [element('dt', '', term), element('dd', '', value)]));
  }

  function refresh() {
    syncAuth();
    syncHeaders();
    const checks = stepChecks();
    setHint('name', checks[0].name);
    setHint('method', null, METHOD_HINTS[byId('action-method').value]);
    setHint('endpoint', checks[1].endpoint, 'Starts with https://. Paste the address the service gave you.');
    setHint('mode', null, wizard.modeNotice || (byId('action-payload-mode').value === 'template' ? 'Type {{ or @ in the template to add a variable.' : 'Runs on this computer, never a paid provider.'));
    if (checks[2].template) setHint('template', menu.hidden ? checks[2].template : {},  'Type {{ or @ to add a variable. Brackets and quotes close themselves; Format JSON tidies it.');
    if (checks[2].model) { setHint('model', checks[2].model); setHint('prompt', checks[2].prompt); }
    const saved = wizard.item?.headerNames?.length && !endpointChanged() ? wizard.item.headerNames.join(', ') : '';
    const authNote = !wizard.item?.hasCredential ? 'Sent as the Authorization header. Stored encrypted on this computer.'
      : endpointChanged() ? 'The address changed, so the saved credential will be removed. Add it again here if the new address needs it.'
      : 'A credential is saved for this action. It is stored encrypted and never shown.';
    setHint('auth', checks[3].auth, authNote);
    setHint('headers', checks[3].headers, wizard.item?.headerNames?.length && endpointChanged() ? 'The address changed, so the saved headers will be removed. Add them again here if needed.' : saved ? `Saved headers: ${saved}. They stay unless you add headers here, which replace them all.` : 'Values are stored encrypted, like the authorization.');
    form.querySelectorAll('.header-row').forEach((line, index) => line.classList.toggle('invalid', checks[3].headers.row === wizard.headerRows[index] && wizard.showErrors.has('headers')));

    let complete = 0;
    steps.forEach((step, index) => {
      const errors = stepErrors(checks, index);
      const done = !errors.length && (wizard.visited.has(index) && index !== wizard.step || Boolean(wizard.item) || index === wizard.step && index === steps.length - 1);
      if (!errors.length && (wizard.visited.has(index) || wizard.item)) complete++;
      const button = form.querySelector(`[data-step-go="${index}"]`);
      const shownError = errors.length && wizard.visited.has(index) && index !== wizard.step;
      button.dataset.state = index === wizard.step ? 'current' : shownError ? 'error' : done ? 'done' : 'todo';
      if (index === wizard.step) button.setAttribute('aria-current', 'step'); else button.removeAttribute('aria-current');
      button.querySelector('.wizard-mark').textContent = button.dataset.state === 'done' ? '✓' : button.dataset.state === 'error' ? '!' : String(index + 1);
      const state = form.querySelector(`[data-step-state="${index}"]`);
      state.textContent = shownError ? 'Needs attention' : done ? 'Done' : ['Name and launcher key', 'Method and address', 'What gets sent', 'Sign-in and headers'][index];
    });
    const percent = Math.round(complete / steps.length * 100);
    const progress = byId('action-progress');
    progress.setAttribute('aria-valuenow', String(percent));
    progress.setAttribute('aria-valuetext', `${complete} of ${steps.length} steps complete`);
    progress.firstElementChild.style.width = `${percent}%`;
    renderReview(checks);
    const result = byId('action-result');
    if (result.dataset.wizard && !steps.some((step, index) => stepErrors(checks, index).length)) { result.textContent = ''; delete result.dataset.wizard; }
    return checks;
  }

  function goTo(index, {force = false} = {}) {
    if (index < 0 || index >= steps.length) return;
    if (index > wizard.step && !force) {
      const checks = refresh();
      const errors = stepErrors(checks, wizard.step);
      if (errors.length) {
        errors.forEach(([name]) => wizard.showErrors.add(name));
        refresh();
        focusFirstError();
        return;
      }
    }
    wizard.step = index;
    wizard.visited.add(index);
    steps.forEach((step, stepIndex) => { step.hidden = stepIndex !== index; });
    const last = index === steps.length - 1;
    byId('action-back').hidden = index === 0;
    byId('action-next').hidden = last;
    byId('action-save').hidden = !last && !wizard.item;
    byId('action-next').classList.toggle('primary', byId('action-save').hidden);
    byId('action-next').textContent = last ? 'Next' : `Next: ${['Request', 'Payload', 'Security'][index]}`;
    refresh();
    if (index === 2 && byId('action-payload-mode').value === 'template') renderEditor(templateSource());
  }

  function focusFirstError() {
    const hint = steps[wizard.step].querySelector('.field-hint.error');
    const target = hint?.closest('label, .field')?.querySelector('input:not([type="hidden"]), textarea:not([hidden]), [contenteditable="true"], button');
    target?.focus();
  }

  form.querySelectorAll('[data-step-go]').forEach((button) => button.addEventListener('click', () => goTo(Number(button.dataset.stepGo), {force: true})));
  byId('action-back').addEventListener('click', () => goTo(wizard.step - 1));
  byId('action-next').addEventListener('click', () => {
    const before = wizard.step;
    goTo(wizard.step + 1);
    if (wizard.step !== before) steps[wizard.step].querySelector('input:not([type="hidden"]), [contenteditable="true"], button')?.focus();
  });

  form.addEventListener('keydown', (event) => {
    if (event.key !== 'Enter' || event.target.tagName !== 'INPUT' || event.isComposing) return;
    if (wizard.step < steps.length - 1) {
      event.preventDefault();
      byId('action-next').click();
    }
  });

  // Runs before the save handler in product.js and stops it when a step has a problem.
  document.addEventListener('submit', (event) => {
    if (event.target !== form) return;
    syncAuth();
    syncHeaders();
    const checks = refresh();
    const failing = steps.findIndex((step, index) => stepErrors(checks, index).length);
    if (failing < 0) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    steps.forEach((step, index) => { stepErrors(checks, index).forEach(([name]) => wizard.showErrors.add(name)); wizard.visited.add(index); });
    goTo(failing, {force: true});
    byId('action-result').textContent = 'Fix the marked step before saving.';
    byId('action-result').dataset.wizard = 'true';
    focusFirstError();
  }, true);

  // Called by openActionEditor after it fills the fields for a new or saved action.
  function open(item = null, context = {}) {
    wizard.item = item;
    wizard.actions = context.actions || [];
    wizard.visited = new Set([0]);
    wizard.showErrors = new Set();
    wizard.history = [];
    wizard.future = [];
    wizard.modeNotice = '';
    wizard.headerRows = [];
    byId('action-presets').hidden = Boolean(item);
    form.querySelectorAll('[data-preset]').forEach((chip) => chip.setAttribute('aria-pressed', 'false'));
    if (!item && !byId('action-template').value) byId('action-template').value = DEFAULT_TEMPLATE;
    byId('action-auth-token').value = '';
    byId('action-auth-user').value = '';
    byId('action-auth-password').value = '';
    byId('action-auth-reveal').setAttribute('aria-pressed', 'false');
    byId('action-auth-reveal').textContent = 'Show';
    byId('action-auth-token').type = 'password';
    byId('action-auth-password').type = 'password';
    fillModels(context.models || []);
    renderAuthSchemes();
    renderHeaderRows();
    renderSlots();
    setMode(byId('action-payload-mode').value || 'template');
    renderEditor(templateSource());
    hideMenu();
    goTo(0, {force: true});
  }

  window.PhorminxActionWizard = {open};
})();
