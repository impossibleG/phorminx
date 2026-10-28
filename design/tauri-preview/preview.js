const invoke = window.__TAURI__?.core?.invoke;
const byId = (id) => document.getElementById(id);
let revision = -1;
let refreshInFlight = false;
let settingsKey = '';
let transcriptKey = '';
let savedTranscriptKey = '';
let messagesKey = '';
let sessionsKey = '';

function updateSelect(select, names, selected, defaultLabel) {
  const fragment = document.createDocumentFragment();
  const initial = document.createElement('option');
  initial.value = '';
  initial.textContent = defaultLabel;
  fragment.append(initial);
  for (const name of names) {
    const option = document.createElement('option');
    option.value = name;
    option.textContent = name;
    fragment.append(option);
  }
  if (selected && !names.includes(selected)) {
    const missing = document.createElement('option');
    missing.value = selected;
    missing.textContent = `${selected} (unavailable)`;
    fragment.append(missing);
  }
  select.replaceChildren(fragment);
  select.value = selected || '';
}

function textRow(label, text) {
  const row = document.createElement('div');
  row.className = 'line';
  const heading = document.createElement('small');
  heading.textContent = label;
  row.append(heading);
  row.append(document.createTextNode(text));
  return row;
}

const SOURCE_LABELS = {microphone: 'YOU', computer: 'COMPUTER'};

function renderTranscript(meeting, target) {
  const fragment = document.createDocumentFragment();
  const translations = new Map(meeting.translations.map((line) => [`${line.startSample}:${line.endSample}`, line]));
  let shown = 0;
  for (const line of meeting.transcript) {
    // Times stay in the saved text; the transcript shows only who spoke and what.
    const parts = transcriptParts(line.text, line.source);
    const translation = translations.get(`${line.startSample}:${line.endSample}`);
    const translated = translation ? translationParts(translation.text, parts.length) : [];
    parts.forEach((part, index) => {
      shown += 1;
      fragment.append(textRow(SOURCE_LABELS[part.source] || 'CONVERSATION', part.text));
      if (translated[index]) {
        const row = document.createElement('div');
        row.className = 'translation';
        row.textContent = translated[index];
        fragment.append(row);
      }
    });
  }
  const provisional = target === byId('transcript') && meeting.provisionalText
    ? transcriptParts(meeting.provisionalText, '').map((part) => part.text).join(' ')
    : '';
  if (provisional) {
    const row = document.createElement('div');
    row.className = 'provisional';
    row.textContent = provisional;
    fragment.append(row);
  }
  target.replaceChildren(fragment);
  if (!shown && !provisional) target.append(textRow('', 'Nothing transcribed yet.'));
}

function clock(samples) {
  const seconds = Math.floor(samples / 16000);
  return `${String(Math.floor(seconds / 60)).padStart(2, '0')}:${String(seconds % 60).padStart(2, '0')}`;
}

// Start and Finish answer the click at once; the snapshot then confirms.
let pendingCapture = '';
let pendingSince = 0;
let lastCapture = 'idle';
let savedSessionId = null;

function renderCaptureControls(meeting) {
  if (pendingCapture === 'starting' && meeting.capture !== 'idle') pendingCapture = '';
  if (pendingCapture === 'stopping' && meeting.capture === 'idle') pendingCapture = '';
  // A start that failed reports through the notice and never leaves idle.
  if (pendingCapture && Date.now() - pendingSince > 10000) pendingCapture = '';
  const capture = pendingCapture || meeting.capture;
  const start = byId('start');
  const stop = byId('stop');
  start.disabled = capture !== 'idle';
  start.textContent = capture === 'starting' ? 'Starting…' : capture === 'idle' ? 'Start meeting' : 'Recording';
  start.classList.toggle('busy', capture === 'starting');
  stop.disabled = capture === 'idle' || capture === 'stopping';
  stop.textContent = capture === 'stopping' ? 'Finishing…' : 'Finish';
  stop.classList.toggle('busy', capture === 'stopping');
  const state = byId('meeting-state');
  state.textContent = capture === 'listening' ? `Recording ${clock(meeting.capturedSamples)}`
    : capture === 'starting' ? 'Starting…'
    : capture === 'stopping' ? 'Saving the last words…' : '';
  state.classList.toggle('recording', capture === 'listening');
  state.classList.toggle('busy', capture === 'starting' || capture === 'stopping');
  // A meeting that just ended in this window gets a saved notice with a way to it.
  if (lastCapture !== 'idle' && meeting.capture === 'idle' && meeting.sessionId && meeting.historyEnabled) {
    savedSessionId = meeting.sessionId;
    byId('meeting-saved-title').textContent = meeting.title || 'Untitled session';
    byId('meeting-saved').hidden = false;
  }
  if (meeting.capture !== 'idle') byId('meeting-saved').hidden = true;
  lastCapture = meeting.capture;
}

function render(meeting) {
  if (document.body.dataset.shell !== 'product') {
    byId('status').textContent = `${meeting.capture} · ${meeting.notice || meeting.translationNotice || 'Local meeting worker ready'}`;
  }
  renderCaptureControls(meeting);
  const translation = byId('translation');
  translation.textContent = meeting.translationActive ? `Translation on · ${meeting.translationLanguage}` : 'Translation off';
  translation.setAttribute('aria-pressed', String(meeting.translationActive));
  translation.title = meeting.translationLanguage && meeting.translationModel
    ? `${meeting.translationLanguage} · ${meeting.translationModel}`
    : 'Choose a language and model in Meeting settings';
  byId('commentary').textContent = meeting.autoCommentaryEnabled ? 'Commentary on' : 'Commentary off';
  byId('commentary').setAttribute('aria-pressed', String(meeting.autoCommentaryEnabled));
  byId('commentary').title = meeting.commentaryModel ? `Commentary with ${meeting.commentaryModel}` : 'Choose a local Ollama model in Meeting settings';
  const nextTranscriptKey = JSON.stringify([meeting.transcript, meeting.translations, meeting.provisionalText]);
  if (nextTranscriptKey !== transcriptKey) {
    transcriptKey = nextTranscriptKey;
    renderTranscript(meeting, byId('transcript'));
  }
  const nextMessagesKey = JSON.stringify(meeting.messages);
  if (nextMessagesKey !== messagesKey) {
    messagesKey = nextMessagesKey;
    const messages = document.createDocumentFragment();
    for (const line of meeting.messages) {
      if (line.role === 'You' || !line.text) {
        messages.append(textRow(line.role.toUpperCase(), line.text || (line.status === 'waiting' ? 'Thinking…' : '')));
      } else {
        const row = document.createElement('div');
        row.className = 'line';
        const heading = document.createElement('small');
        heading.textContent = line.role.toUpperCase();
        const answer = document.createElement('div');
        answer.className = 'markdown';
        answer.append(renderMarkdown(line.text));
        row.append(heading, answer);
        messages.append(row);
      }
    }
    byId('messages').replaceChildren(messages);
  }

  const nextSessionsKey = JSON.stringify([meeting.sessions, meeting.savedSession, meeting.sessionId]);
  if (nextSessionsKey !== sessionsKey) {
    sessionsKey = nextSessionsKey;
    const sessions = document.createDocumentFragment();
    for (const session of meeting.sessions) {
      const button = document.createElement('button');
      button.type = 'button';
      button.classList.toggle('active', meeting.savedSession && meeting.sessionId === session.id);
      button.textContent = session.title || 'Untitled session';
      const source = document.createElement('small');
      source.textContent = session.source;
      button.append(source);
      button.addEventListener('click', () => { void action('select', String(session.id)); });
      sessions.append(button);
    }
    byId('sessions').replaceChildren(sessions);
    if (!meeting.sessions.length) byId('sessions').append(textRow('', byId('query').value.trim() ? 'No matches. Try another phrase.' : 'No saved meetings yet.'));
  }
  byId('saved-title').textContent = meeting.savedSession ? meeting.title || 'Untitled session' : 'Select a session';
  const nextSavedTranscriptKey = JSON.stringify([meeting.savedSession, meeting.sessionId, meeting.transcript, meeting.translations]);
  if (nextSavedTranscriptKey !== savedTranscriptKey) {
    savedTranscriptKey = nextSavedTranscriptKey;
    if (meeting.savedSession) renderTranscript(meeting, byId('saved-transcript'));
    else byId('saved-transcript').replaceChildren(textRow('', 'Select a session to read its transcript.'));
  }

  const nextSettingsKey = JSON.stringify([
    meeting.inputDevices, meeting.outputDevices, meeting.availableModels,
    meeting.selectedInputDevice, meeting.selectedOutputDevice,
    meeting.translationLanguage, meeting.translationModel,
  ]);
  if (nextSettingsKey !== settingsKey) {
    settingsKey = nextSettingsKey;
    updateSelect(byId('output-device'), meeting.outputDevices, meeting.selectedOutputDevice, 'Windows default');
    updateSelect(byId('input-device'), meeting.inputDevices, meeting.selectedInputDevice, 'Windows default');
    updateSelect(byId('translation-model'), meeting.availableModels, meeting.translationModel, 'Choose an installed model');
    setTranslationLanguage(meeting.translationLanguage || '');
  }
  byId('translation-notice').textContent = meeting.translationNotice;
}

async function refresh() {
  if (!invoke || refreshInFlight) return;
  refreshInFlight = true;
  try {
    const meeting = await invoke('meeting_snapshot', {since: revision < 0 ? null : revision});
    if (meeting) { revision = meeting.revision; render(meeting); }
  } catch (error) { byId('status').textContent = String(error); }
  finally { refreshInFlight = false; }
}

async function action(kind, value) {
  if (!invoke) { byId('status').textContent = 'Tauri bridge unavailable.'; return false; }
  try { await invoke('meeting_action', {kind, value: value ?? null}); await refresh(); return true; }
  catch (error) { byId('status').textContent = String(error); return false; }
}

for (const button of document.querySelectorAll('[data-route]')) {
  button.addEventListener('click', () => {
    if (document.body.dataset.shell === 'product') return;
    document.querySelectorAll('[data-route]').forEach((peer) => peer.classList.toggle('active', peer === button));
    document.querySelectorAll('.page').forEach((page) => { page.hidden = page.id !== button.dataset.route; });
    if (button.dataset.route === 'saved') void action('refresh');
  });
}
byId('start').addEventListener('click', async () => {
  pendingCapture = 'starting';
  pendingSince = Date.now();
  byId('meeting-saved').hidden = true;
  byId('start').disabled = true;
  byId('start').textContent = 'Starting…';
  if (!await action('start')) { pendingCapture = ''; byId('start').disabled = false; byId('start').textContent = 'Start meeting'; }
});
byId('stop').addEventListener('click', async () => {
  pendingCapture = 'stopping';
  pendingSince = Date.now();
  byId('stop').disabled = true;
  byId('stop').textContent = 'Finishing…';
  if (!await action('stop')) pendingCapture = '';
});
byId('translation').addEventListener('click', () => { void action('translation'); });
byId('meeting-saved-close').addEventListener('click', () => { byId('meeting-saved').hidden = true; });
byId('meeting-saved-open').addEventListener('click', () => {
  byId('meeting-saved').hidden = true;
  if (typeof showRoute === 'function') showRoute('saved');
  if (savedSessionId) void action('select', String(savedSessionId));
});
byId('companion').addEventListener('click', () => { void action('companion'); });
byId('commentary').addEventListener('click', () => { void action('commentary'); });
byId('search').addEventListener('submit', (event) => { event.preventDefault(); void action('search', byId('query').value.trim()); });
byId('devices-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  if (!invoke) return;
  try {
    await invoke('meeting_save_devices', {
      outputDevice: byId('output-device').value || null,
      microphoneDevice: byId('input-device').value || null,
    });
    await refresh();
  } catch (error) { byId('status').textContent = String(error); }
});
// Live translation targets. The value is the name the translation model is told.
const TRANSLATION_LANGUAGES = ['Arabic', 'Bengali', 'Chinese (Simplified)', 'Chinese (Traditional)', 'Czech', 'Danish', 'Dutch', 'English', 'Finnish', 'French', 'German', 'Greek', 'Hebrew', 'Hindi', 'Hungarian', 'Indonesian', 'Italian', 'Japanese', 'Korean', 'Norwegian', 'Persian', 'Polish', 'Portuguese (Brazil)', 'Portuguese (Portugal)', 'Romanian', 'Russian', 'Spanish', 'Swedish', 'Tagalog', 'Thai', 'Turkish', 'Ukrainian', 'Urdu', 'Vietnamese'];
// Earlier versions took free text; common spellings map onto the list.
const LANGUAGE_ALIASES = {portuguese: 'Portuguese (Brazil)', 'português': 'Portuguese (Brazil)', 'portugues': 'Portuguese (Brazil)', 'pt-br': 'Portuguese (Brazil)', 'pt': 'Portuguese (Brazil)', 'pt-pt': 'Portuguese (Portugal)', 'brazilian portuguese': 'Portuguese (Brazil)', chinese: 'Chinese (Simplified)', mandarin: 'Chinese (Simplified)', 'zh': 'Chinese (Simplified)', en: 'English', es: 'Spanish', 'español': 'Spanish', espanol: 'Spanish', fr: 'French', 'français': 'French', de: 'German', deutsch: 'German', it: 'Italian', ja: 'Japanese', ko: 'Korean', ru: 'Russian', farsi: 'Persian', filipino: 'Tagalog'};

function canonicalLanguage(value) {
  const key = value.trim().toLowerCase();
  if (!key) return '';
  return TRANSLATION_LANGUAGES.find((name) => name.toLowerCase() === key) || LANGUAGE_ALIASES[key] || value.trim();
}

function setTranslationLanguage(saved) {
  const select = byId('translation-language');
  const value = canonicalLanguage(saved);
  const names = [...TRANSLATION_LANGUAGES];
  // Keep a language saved earlier that is not on the list, so saving never drops it.
  if (value && !names.includes(value)) names.unshift(value);
  const options = [new Option('Off', '')];
  for (const name of names) options.push(new Option(TRANSLATION_LANGUAGES.includes(name) ? name : `${name} (saved earlier)`, name));
  select.replaceChildren(...options);
  select.value = value;
}
setTranslationLanguage('');

byId('translation-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  if (!invoke) return;
  try {
    await invoke('meeting_save_translation', {
      language: byId('translation-language').value,
      model: byId('translation-model').value,
    });
    await refresh();
  } catch (error) { byId('status').textContent = String(error); }
});
byId('ask').addEventListener('submit', async (event) => {
  event.preventDefault();
  const input = byId('question');
  const value = input.value.trim();
  if (!value) return;
  input.disabled = true;
  const sent = await action('ask', value);
  if (sent) input.value = '';
  input.disabled = false;
  input.focus();
});

if (invoke) { void refresh(); setInterval(() => { void refresh(); }, 250); }
else byId('status').textContent = 'Open this preview in the Phorminx Tauri application.';
