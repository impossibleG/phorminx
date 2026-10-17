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
let conversationKey = '';

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

function renderTranscript(meeting) {
  const nextKey = JSON.stringify([meeting.transcript, meeting.translations, meeting.provisionalText, meeting.capture, meeting.translationLanguage]);
  if (nextKey === transcriptKey) return;
  transcriptKey = nextKey;
  const follow = nearBottom(transcript);
  const translated = new Map(meeting.translations.map((line) => [`${line.startSample}:${line.endSample}`, line]));
  const rows = document.createDocumentFragment();
  for (const line of meeting.transcript) {
    const article = document.createElement('article');
    article.className = 'phrase';
    const source = document.createElement('div');
    source.className = 'phrase-source';
    const label = line.source === 'microphone' ? 'MICROPHONE' : line.source === 'computer' ? 'COMPUTER' : 'CONVERSATION';
    appendText(source, line.source === 'microphone' ? 'you' : '', label, 'span');
    const content = document.createElement('div');
    content.className = 'phrase-text';
    appendText(content, 'original', line.text);
    const translation = translated.get(`${line.startSample}:${line.endSample}`);
    if (translation) appendText(content, 'translation', translation.text);
    else if (meeting.translationLanguage && frame.dataset.language !== 'original') appendText(content, 'pending', 'Translating…');
    article.append(source, content);
    rows.append(article);
  }
  if (meeting.provisionalText) appendText(rows, 'provisional', meeting.provisionalText, 'div');
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
  const nextKey = JSON.stringify(meeting.messages);
  if (nextKey === conversationKey) return;
  conversationKey = nextKey;
  const follow = nearBottom(conversation);
  const rows = document.createDocumentFragment();
  for (const line of meeting.messages) {
    const article = document.createElement('article');
    article.className = line.role === 'You' ? 'message from-you' : 'message from-phorminx';
    appendText(article, '', line.role === 'You' ? 'YOU' : 'PHORMINX', 'small');
    if (line.role === 'You' || !line.text) {
      appendText(article, '', line.text || (line.status === 'waiting' ? 'Thinking…' : ''), 'p');
    } else {
      const answer = document.createElement('div');
      answer.className = 'markdown';
      answer.append(renderMarkdown(line.text));
      article.append(answer);
    }
    if (line.status === 'failed' || line.status === 'stopped') appendText(article, 'status', line.status, 'div');
    rows.append(article);
  }
  conversation.replaceChildren(rows);
  if (follow) conversation.scrollTop = conversation.scrollHeight;
}

function render(meeting) {
  if (!layoutInitialized) {
    selectLayout('view', meeting.companionView);
    selectLayout('language', meeting.companionTranslationView);
    byId('room').style.setProperty('--split', `${meeting.companionSplitFraction / 10}%`);
    layoutInitialized = true;
  }
  currentCapture = meeting.capture;
  byId('session-title').textContent = meeting.title || 'Untitled session';
  byId('record-status').textContent = meeting.capture === 'listening' ? 'LISTENING' : meeting.capture.toUpperCase();
  const seconds = Math.floor(meeting.capturedSamples / 16000);
  byId('elapsed').textContent = `${String(Math.floor(seconds / 60)).padStart(2, '0')}:${String(seconds % 60).padStart(2, '0')}`;
  frame.classList.toggle('finished', meeting.capture === 'idle');
  for (const [id, muted] of [['mic', meeting.microphoneMuted], ['computer', meeting.computerAudioMuted]]) {
    const button = byId(id);
    button.setAttribute('aria-pressed', String(!muted));
    button.querySelector('em').textContent = muted ? 'off' : 'on';
  }
  const commentary = byId('commentary');
  commentary.classList.toggle('on', meeting.autoCommentaryEnabled);
  commentary.setAttribute('aria-checked', String(meeting.autoCommentaryEnabled));
  const language = meeting.translationLanguage || 'Translation';
  byId('translation-choice').textContent = language;
  byId('status-strip').textContent = meeting.notice || meeting.translationNotice || '';
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
byId('finish').addEventListener('click', () => { if (currentCapture !== 'idle') void action('stop').catch(() => {}); });
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
