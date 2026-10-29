const invoke = window.__TAURI__?.core?.invoke;
const byId = (id) => document.getElementById(id);
const frame = byId('window-frame');
const transcript = byId('transcript-scroll');
const conversation = byId('conversation-scroll');
let revision = -1;
let refreshInFlight = false;
let currentCapture = 'idle';
let layoutInitialized = false;
let transcriptKey = '';
let finishRequested = false;
let sawRecording = false;
const CAPTURE_LABELS = {idle: 'ENDED', starting: 'STARTING…', listening: 'RECORDING', stopping: 'FINISHING…'};
let conversationKey = '';
let itemsKey = '';
let currentItems = [];
const dismissed = new Set();

function persistLayout() {
  if (!invoke) return;
  const split = Math.round((Number.parseFloat(byId('room').style.getPropertyValue('--split')) || 57) * 10);
  void invoke('meeting_layout', {view: frame.dataset.view, language: frame.dataset.language, split})
    .catch((error) => { byId('status-strip').textContent = String(error); });
}

function selectLayout(group, value) {
  const attribute = group === 'view' ? 'roomTab' : 'languageChoice';
  const selector = group === 'view' ? '[data-room-tab]' : '[data-language-choice]';
  frame.dataset[group] = value;
  document.querySelectorAll(selector).forEach((button) => button.classList.toggle('active', button.dataset[attribute] === value));
}

function nearBottom(element) {
  return element.scrollHeight - element.scrollTop - element.clientHeight < 72;
}

function appendText(parent, className, value, tag = 'p') {
  const element = document.createElement(tag);
  element.className = className;
  element.textContent = value;
  parent.append(element);
  return element;
}

const SOURCE_LABELS = {microphone: 'YOU', computer: 'COMPUTER'};

function renderTranscript(meeting) {
  const nextKey = JSON.stringify([meeting.transcript, meeting.translations, meeting.provisionalText, meeting.capture, meeting.translationActive]);
  if (nextKey === transcriptKey) return;
  transcriptKey = nextKey;
  const follow = nearBottom(transcript);
  const translated = new Map(meeting.translations.map((line) => [`${line.startSample}:${line.endSample}`, line]));
  const rows = document.createDocumentFragment();
  for (const line of meeting.transcript) {
    // Times stay in the saved text; the transcript shows only who spoke and what.
    const parts = transcriptParts(line.text, line.source);
    if (!parts.length) continue;
    const translation = translated.get(`${line.startSample}:${line.endSample}`);
    const translations = translation ? translationParts(translation.text, parts.length) : [];
    parts.forEach((part, index) => {
      const article = document.createElement('article');
      article.className = 'phrase';
      const source = document.createElement('div');
      source.className = 'phrase-source';
      appendText(source, part.source === 'microphone' ? 'you' : '', SOURCE_LABELS[part.source] || 'CONVERSATION', 'span');
      const content = document.createElement('div');
      content.className = 'phrase-text';
      const translatedText = translations[index] || '';
      // Without a translation the original is the only text, so it shows in every view.
      appendText(content, translatedText ? 'original' : 'original untranslated', part.text);
      if (translatedText) appendText(content, 'translation', translatedText);
      else if (!translation && meeting.translationActive && meeting.capture !== 'idle' && index === parts.length - 1 && frame.dataset.language !== 'original') appendText(content, 'pending', 'Translating…');
      article.append(source, content);
      rows.append(article);
    });
  }
  if (meeting.provisionalText) {
    const provisional = transcriptParts(meeting.provisionalText, '').map((part) => part.text).join(' ');
    if (provisional) appendText(rows, 'provisional', provisional, 'div');
  }
  if (meeting.capture === 'listening') {
    const tail = document.createElement('div');
    tail.className = 'listening-tail';
    tail.append(document.createElement('i'));
    appendText(tail, '', 'Listening…', 'span');
    rows.append(tail);
  }
  transcript.replaceChildren(rows);
  if (follow) transcript.scrollTop = transcript.scrollHeight;
}

function renderConversation(meeting) {
  const nextKey = JSON.stringify([meeting.messages, meeting.importantItems]);
  if (nextKey === conversationKey) return;
  conversationKey = nextKey;
  const follow = nearBottom(conversation);
  const rows = document.createDocumentFragment();
  for (const line of meeting.messages) {
    const commentary = line.kind === 'commentary';
    if (commentary && dismissed.has(line.text)) continue;
    const article = document.createElement('article');
    article.className = line.role === 'You' ? 'message from-you' : commentary ? 'message from-phorminx commentary' : 'message from-phorminx';
    const label = appendText(article, '', line.role === 'You' ? 'YOU' : commentary ? 'COMMENTARY' : 'PHORMINX', 'small');
    if (commentary) {
      const dismiss = document.createElement('button');
      dismiss.type = 'button';
      dismiss.className = 'dismiss';
      dismiss.textContent = 'Dismiss';
      dismiss.addEventListener('click', () => { dismissed.add(line.text); article.remove(); });
      label.append(dismiss);
    }
    const parts = commentary ? commentaryParts(line.text) : {body: line.text, pins: []};
    if (line.role === 'You' || !line.text) {
      appendText(article, '', line.text || (line.status === 'waiting' ? 'Thinking…' : ''), 'p');
    } else {
      const answer = document.createElement('div');
      answer.className = 'markdown';
      answer.append(renderMarkdown(parts.body));
      article.append(answer);
    }
    for (const pin of parts.pins) {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'pin-suggestion';
      const pinned = hasItem(meeting.importantItems, pin);
      button.disabled = pinned;
      button.textContent = pinned ? `Pinned · ${pin}` : `+ Pin as important · ${pin}`;
      button.addEventListener('click', () => { void action('pin-item', pin).catch(() => {}); });
      article.append(button);
    }
    if (line.status === 'failed' || line.status === 'stopped') appendText(article, 'status', line.status, 'div');
    rows.append(article);
  }
  conversation.replaceChildren(rows);
  if (follow) conversation.scrollTop = conversation.scrollHeight;
}

function renderItems(meeting) {
  const style = byId('commentary-style');
  if (!style.options.length) for (const [value, label] of COMMENTARY_STYLES) style.append(new Option(label, value));
  if (document.activeElement !== style) style.value = meeting.commentaryStyle || '';
  const nextKey = JSON.stringify([meeting.importantItems, meeting.planTitle]);
  if (nextKey === itemsKey) return;
  itemsKey = nextKey;
  currentItems = meeting.importantItems || [];
  const open = currentItems.filter((item) => !item.done).length;
  byId('items-count').textContent = currentItems.length ? ` ${open}/${currentItems.length}` : '';
  byId('items-plan').textContent = meeting.planTitle || '';
  const rows = document.createDocumentFragment();
  currentItems.forEach((item, index) => {
    const row = document.createElement('li');
    row.classList.toggle('done', item.done);
    const label = document.createElement('label');
    const check = document.createElement('input');
    check.type = 'checkbox';
    check.checked = item.done;
    check.addEventListener('change', () => saveItems(currentItems.map((other, at) => (at === index ? {...other, done: check.checked} : other))));
    label.append(check, document.createTextNode(item.text));
    const remove = document.createElement('button');
    remove.type = 'button';
    remove.textContent = '×';
    remove.setAttribute('aria-label', `Remove ${item.text}`);
    remove.addEventListener('click', () => saveItems(currentItems.filter((_, at) => at !== index)));
    row.append(label, remove);
    rows.append(row);
  });
  byId('items-list').replaceChildren(rows);
  if (!currentItems.length) appendText(byId('items-list'), 'empty', 'No items yet. Add what must not be forgotten; the assistant keeps them in mind.', 'li');
}

function saveItems(items) { void action('items', JSON.stringify(items)).catch(() => {}); }

function render(meeting) {
  if (!layoutInitialized) {
    selectLayout('view', meeting.companionView);
    selectLayout('language', meeting.companionTranslationView);
    byId('room').style.setProperty('--split', `${meeting.companionSplitFraction / 10}%`);
    layoutInitialized = true;
  }
  if (meeting.capture !== 'idle') sawRecording = true;
  currentCapture = meeting.capture;
  byId('session-title').textContent = meeting.title || 'Untitled session';
  const recordState = document.querySelector('.record-state');
  const finishing = finishRequested && meeting.capture !== 'idle' || meeting.capture === 'stopping';
  const shown = finishing ? 'stopping' : meeting.capture;
  byId('record-status').textContent = CAPTURE_LABELS[shown] || shown.toUpperCase();
  recordState.classList.toggle('recording', shown === 'listening');
  recordState.classList.toggle('busy', shown === 'starting' || shown === 'stopping');
  const seconds = Math.floor(meeting.capturedSamples / 16000);
  byId('elapsed').textContent = `${String(Math.floor(seconds / 60)).padStart(2, '0')}:${String(seconds % 60).padStart(2, '0')}`;
  frame.classList.toggle('finished', meeting.capture === 'idle');
  const finish = byId('finish');
  finish.textContent = meeting.capture === 'idle' ? 'Finished' : finishing ? 'Finishing…' : 'Finish';
  finish.disabled = meeting.capture === 'idle' || finishing;
  finish.classList.toggle('busy', finishing);
  if (meeting.capture === 'idle') finishRequested = false;
  // Only a meeting that recorded in this window ends with the saved card.
  byId('end-card').hidden = !(sawRecording && meeting.capture === 'idle' && meeting.sessionId && meeting.historyEnabled);
  for (const [id, muted] of [['mic', meeting.microphoneMuted], ['computer', meeting.computerAudioMuted]]) {
    const button = byId(id);
    button.setAttribute('aria-pressed', String(!muted));
    button.querySelector('em').textContent = muted ? 'off' : 'on';
  }
  const commentary = byId('commentary');
  commentary.classList.toggle('on', meeting.autoCommentaryEnabled);
  commentary.setAttribute('aria-checked', String(meeting.autoCommentaryEnabled));
  byId('commentary-detail').textContent = meeting.commentaryModel || 'Choose a local model';
  commentary.title = meeting.commentaryModel ? `Commentary with ${meeting.commentaryModel}` : 'Choose a local Ollama model in Meeting settings';
  const interval = byId('commentary-interval');
  const intervalSecs = String(meeting.autoCommentaryIntervalSecs);
  if (![...interval.options].some((option) => option.value === intervalSecs)) interval.append(new Option(`${intervalSecs} s`, intervalSecs));
  if (document.activeElement !== interval) interval.value = intervalSecs;
  const translation = byId('translation');
  const configured = Boolean(meeting.translationLanguage && meeting.translationModel);
  translation.classList.toggle('on', meeting.translationActive);
  translation.setAttribute('aria-checked', String(meeting.translationActive));
  translation.setAttribute('aria-disabled', String(!configured));
  const detail = configured ? `${meeting.translationLanguage} · ${meeting.translationModel}` : 'Set up in Meeting settings';
  byId('translation-detail').textContent = detail;
  translation.title = configured ? `Translation to ${detail}` : 'Choose a language and model in Meeting settings';
  frame.classList.toggle('translation-off', !meeting.translationActive && !meeting.translations.length);
  const language = meeting.translationLanguage || 'Translation';
  byId('translation-choice').textContent = language;
  byId('status-strip').textContent = meeting.notice || meeting.translationNotice || '';
  renderItems(meeting);
  renderTranscript(meeting);
  renderConversation(meeting);
}

async function refresh() {
  if (!invoke || refreshInFlight) return;
  refreshInFlight = true;
  try {
    const meeting = await invoke('meeting_snapshot', {since: revision < 0 ? null : revision});
    if (meeting) { revision = meeting.revision; render(meeting); }
  } catch (error) {
    byId('status-strip').textContent = String(error);
  } finally {
    refreshInFlight = false;
  }
}

async function action(kind, value) {
  if (!invoke) throw new Error('Tauri bridge unavailable.');
  try {
    await invoke('meeting_action', {kind, value: value ?? null});
    await refresh();
  } catch (error) {
    byId('status-strip').textContent = String(error);
    throw error;
  }
}

for (const button of document.querySelectorAll('[data-room-tab]')) {
  button.addEventListener('click', () => {
    selectLayout('view', button.dataset.roomTab);
    persistLayout();
  });
}
for (const button of document.querySelectorAll('[data-language-choice]')) {
  button.addEventListener('click', () => {
    selectLayout('language', button.dataset.languageChoice);
    persistLayout();
  });
}
byId('mic').addEventListener('click', () => { void action('mute-microphone').catch(() => {}); });
byId('computer').addEventListener('click', () => { void action('mute-computer').catch(() => {}); });
byId('commentary').addEventListener('click', () => { void action('commentary').catch(() => {}); });
byId('hide').addEventListener('click', () => { void action('hide-companion').catch(() => {}); });
byId('translation').addEventListener('click', () => { void action('translation').catch(() => {}); });
byId('commentary-style').addEventListener('change', (event) => { void action('commentary-style', event.target.value).catch(() => {}); });
byId('items-toggle').addEventListener('click', () => {
  const drawer = byId('items-drawer');
  drawer.hidden = !drawer.hidden;
  byId('items-toggle').setAttribute('aria-expanded', String(!drawer.hidden));
  if (!drawer.hidden) byId('items-text').focus();
});
byId('items-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  const input = byId('items-text');
  const text = input.value.trim();
  if (!text) return;
  try { await action('pin-item', text); input.value = ''; } catch (_) { /* The notice explains. */ }
});
byId('commentary-interval').addEventListener('change', (event) => { void action('commentary-interval', event.target.value).catch(() => {}); });
byId('end-close').addEventListener('click', () => { void action('hide-companion').catch(() => {}); });
byId('finish').addEventListener('click', () => {
  if (currentCapture === 'idle' || finishRequested) return;
  // Respond at once; the backend can take a few seconds to flush the last words.
  finishRequested = true;
  const finish = byId('finish');
  finish.textContent = 'Finishing…';
  finish.disabled = true;
  finish.classList.add('busy');
  byId('record-status').textContent = CAPTURE_LABELS.stopping;
  void action('stop').catch(() => { finishRequested = false; finish.disabled = false; finish.textContent = 'Finish'; finish.classList.remove('busy'); });
});
byId('composer').addEventListener('submit', async (event) => {
  event.preventDefault();
  const input = byId('question');
  const text = input.value.trim();
  if (!text) return;
  input.disabled = true;
  try { await action('ask', text); input.value = ''; }
  catch (_) { /* Keep the unsent question available for retry. */ }
  finally { input.disabled = false; input.focus(); }
});

const divider = byId('divider');
let dragging = false;
function setSplit(clientX) {
  const room = byId('room');
  const bounds = room.getBoundingClientRect();
  if (!bounds.width) return;
  const percentage = Math.max(28, Math.min(72, ((clientX - bounds.left) / bounds.width) * 100));
  room.style.setProperty('--split', `${percentage}%`);
}
divider.addEventListener('pointerdown', (event) => { dragging = true; divider.setPointerCapture(event.pointerId); setSplit(event.clientX); });
divider.addEventListener('pointermove', (event) => { if (dragging) setSplit(event.clientX); });
divider.addEventListener('pointerup', () => { dragging = false; persistLayout(); });
divider.addEventListener('pointercancel', () => { dragging = false; });
divider.addEventListener('dblclick', () => { byId('room').style.setProperty('--split', '57%'); persistLayout(); });
divider.addEventListener('keydown', (event) => {
  if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
  event.preventDefault();
  const room = byId('room');
  const current = Number.parseFloat(room.style.getPropertyValue('--split')) || 57;
  room.style.setProperty('--split', `${Math.max(28, Math.min(72, current + (event.key === 'ArrowRight' ? 2 : -2)))}%`);
  persistLayout();
});

if (!invoke) byId('status-strip').textContent = 'Open this companion from Phorminx.';
else {
  void invoke('meeting_theme').then((theme) => {
    frame.classList.toggle('dark', theme === 'dark');
    frame.classList.toggle('light', theme !== 'dark');
  }).catch(() => {});
  void refresh();
  setInterval(() => { void refresh(); }, 250);
}
