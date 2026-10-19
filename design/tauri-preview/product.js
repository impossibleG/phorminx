const tauriInvoke = window.__TAURI__?.core?.invoke;
const hostInvoke = tauriInvoke && ((command, args) => tauriInvoke(command.replace(/^plugin:phorminx-host\|/, ''), args));
const node = (id) => document.getElementById(id);
let appView = null;
let currentRoute = 'home';
let selectedHistoryId = null;
let appRefreshInFlight = false;
let lastHostRoute = null;
let knownActions = [];
let pendingSetupAction = null;
let capturingShortcut = false;
let preferencesDirty = false;
let dictationDirty = false;
let privacyDirty = false;
let customInstructionsLoaded = false;
let historyGeneration = null;
let historyHits = null;
let historySearchTimer = null;
let providerDirty = false;

function makeElement(tag, className, text) {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text !== undefined) element.textContent = text;
  return element;
}

function showRoute(route, persist = true) {
  if (currentRoute === 'settings' && route !== 'settings' && capturingShortcut) void endShortcutCapture();
  // The meeting chat log already owns id="messages", so the Messages page is #messaging.
  const pageId = route === 'settings' ? 'app-settings' : route === 'messages' ? 'messaging' : route;
  const page = node(pageId);
  if (!page) return;
  currentRoute = route;
  for (const section of document.querySelectorAll('main > .page')) section.hidden = section !== page;
  for (const button of document.querySelectorAll('.side [data-route]')) {
    const belongsToMeetings = ['live', 'saved', 'meeting-settings'].includes(route) && button.dataset.route === 'live';
    const editorParent = route === 'action-editor' && button.dataset.route === 'actions' || route === 'lexicon-editor' && button.dataset.route === 'lexicon' || route === 'profile-editor' && button.dataset.route === 'profiles';
    button.classList.toggle('active', button.dataset.route === route || belongsToMeetings || editorParent);
  }
  for (const button of document.querySelectorAll('.meeting-tabs [data-route]')) {
    const active = button.dataset.route === route;
    button.classList.toggle('active', active);
    button.setAttribute('aria-selected', String(active));
  }
  if (route === 'saved') void action('refresh');
  if (route === 'actions') void refreshActions();
  if (route === 'models' || route === 'benchmarks') void window.PhorminxModels?.refresh();
  if (route === 'setup') { void refreshSetup(); void refreshUpdates(); void refreshRepair(); }
  if (route === 'history' && node('history-query').value.trim()) void startHistorySearch();
  if (route === 'meeting-settings') void refreshProvider();
  if (route === 'prompts') window.dispatchEvent(new Event('prompts:show'));
  if (route === 'benchmarks') void showBenchmarks();
  document.dispatchEvent(new CustomEvent('phorminx:route', {detail: route}));
  window.phorminxMessages?.routeChanged(route);
  // Prompts, Investigations, Messages and Benchmarks are pages of this window
  // only; the host keeps the last other route.
  if (route === 'feedback') void refreshFeedbackHistory();
  if (persist && hostInvoke && !['prompts', 'investigations', 'messages', 'benchmarks', 'feedback'].includes(route)) {
    const hostRoute = ['live', 'saved', 'meeting-settings'].includes(route) ? 'meetings' : route === 'action-editor' ? 'actions' : route === 'lexicon-editor' ? 'lexicon' : route === 'profile-editor' ? 'profiles' : route;
    lastHostRoute = hostRoute;
    void hostInvoke('plugin:phorminx-host|host_select_route', {route: hostRoute}).catch((error) => { node('status').textContent = String(error); });
  }
}

function showSettingsTab(tab) {
  for (const button of document.querySelectorAll('[data-settings-tab]')) {
    const active = button.dataset.settingsTab === tab;
    button.classList.toggle('active', active);
    button.setAttribute('aria-selected', String(active));
  }
  for (const panel of document.querySelectorAll('.settings-panel')) panel.hidden = panel.id !== `settings-${tab}`;
  if (tab === 'dictation') void refreshInputDevices();
  if (tab === 'updates') void refreshUpdates();
  if (tab === 'dictation' && !customInstructionsLoaded && hostInvoke) {
    customInstructionsLoaded = true;
    void hostInvoke('plugin:phorminx-host|host_custom_instructions')
      .then((text) => { if (!dictationDirty) node('custom-instructions').value = text || ''; })
      .catch((error) => {
        customInstructionsLoaded = false;
        node('dictation-result').textContent = String(error);
      });
  }
}

function renderUpdates(view) {
  node('update-installed').textContent = view.installedVersion;
  node('update-daily').checked = view.dailyChecks;
  node('update-automatic').checked = view.automaticInstall;
  node('update-automatic').disabled = !view.configured || view.scheduled;
  node('setup-auto-updates').checked = view.automaticInstall;
  node('setup-auto-updates').disabled = !view.configured || view.scheduled;
  node('update-available').textContent = view.availableVersion
    ? `Version ${view.availableVersion} is available${view.downloaded ? ' and verified' : ''}.`
    : 'No newer verified release is ready.';
  node('update-last-check').textContent = view.lastSuccessfulCheck
    ? `Last successful check: ${new Date(view.lastSuccessfulCheck * 1000).toLocaleString()}`
    : 'Not checked yet.';
  node('update-check').disabled = !view.configured || view.busy || view.scheduled;
  node('update-download').hidden = !view.availableVersion || view.downloaded;
  node('update-download').disabled = view.busy || view.scheduled;
  node('update-install').hidden = !view.downloaded;
  node('update-install').disabled = view.busy || view.scheduled;
  node('update-restart').hidden = !view.downloaded;
  node('update-restart').disabled = view.busy;
  node('update-notice').textContent = view.configured
    ? view.notice
    : 'This development build has no production release-signing key. Signed updates will be enabled in a public release.';
  node('setup-update-note').textContent = view.configured
    ? (view.automaticInstall ? 'Automatic updates are on.' : 'Automatic updates are off. You can turn them on later in Settings.')
    : 'Signed updates are unavailable in this development build.';
}

async function refreshUpdates() {
  if (!hostInvoke) return;
  try { renderUpdates(await hostInvoke('host_update_snapshot')); }
  catch (error) { node('update-notice').textContent = String(error); node('setup-update-note').textContent = String(error); }
}

async function updateAction(kind, enabled) {
  try { renderUpdates(await hostInvoke('host_update_action', {kind, enabled})); }
  catch (error) { node('update-notice').textContent = String(error); node('setup-update-note').textContent = String(error); await refreshUpdates(); }
}

async function refreshInputDevices() {
  if (!hostInvoke) return;
  const select = node('dictation-microphone');
  const selected = dictationDirty ? select.value : appView?.settings.microphone || '';
  try {
    const names = await hostInvoke('host_input_devices');
    const options = [makeElement('option', '', 'Windows default')];
    options[0].value = '';
    for (const name of names) {
      const option = makeElement('option', '', name);
      option.value = name;
      options.push(option);
    }
    if (selected && !names.includes(selected)) {
      const missing = makeElement('option', '', `${selected} · unavailable`);
      missing.value = selected;
      options.push(missing);
    }
    select.replaceChildren(...options);
    select.value = selected;
  } catch (error) { node('dictation-result').textContent = String(error); }
}

async function endShortcutCapture() {
  if (!capturingShortcut) return;
  capturingShortcut = false;
  node('capture-shortcut').textContent = 'Capture keys';
  node('launcher-shortcut').readOnly = false;
  try { await hostInvoke('plugin:phorminx-host|host_action', {kind: 'end-shortcut-capture'}); }
  catch (error) { node('preferences-result').textContent = String(error); }
}

function shortcutKey(event) {
  if (/^Key[A-Z]$/.test(event.code)) return event.code.slice(3);
  if (/^Digit[0-9]$/.test(event.code)) return event.code.slice(5);
  if (/^F(?:[1-9]|1[0-9]|2[0-4])$/.test(event.code)) return event.code;
  if (event.code === 'Space') return 'Space';
  if (event.code === 'Enter') return 'Enter';
  return null;
}

function dateLabel(timestamp) {
  const date = new Date(timestamp);
  return Number.isNaN(date.getTime()) ? 'Unknown date' : new Intl.DateTimeFormat(undefined, {dateStyle: 'medium', timeStyle: 'short'}).format(date);
}

function renderHome(view, runtimeStatus = 'checking') {
  node('runtime-state').textContent = runtimeStatus.charAt(0).toUpperCase() + runtimeStatus.slice(1);
  node('shortcut-display').textContent = view.settings.launcherShortcut;
  const systems = document.createDocumentFragment();
  for (const system of view.systems) {
    const row = makeElement('div', 'system-row');
    row.dataset.state = system.state;
    row.append(makeElement('span', 'system-name', system.name), makeElement('span', 'system-message', system.message));
    systems.append(row);
  }
  node('systems').replaceChildren(systems);
  const recent = document.createDocumentFragment();
  for (const item of view.history.slice(0, 4)) {
    const row = makeElement('div', 'recent-row');
    row.append(makeElement('time', '', dateLabel(item.createdAtMs)), makeElement('span', '', item.preview));
    recent.append(row);
  }
  if (!view.history.length) recent.append(makeElement('p', 'empty-state', view.historyEnabled ? 'No dictations yet.' : 'History is off.'));
  node('recent-history').replaceChildren(recent);
}

function renderHistory(view) {
  const query = node('history-query').value.trim().toLocaleLowerCase();
  const list = document.createDocumentFragment();
  const entries = query && historyHits !== null
    ? historyHits.map((hit) => ({id: hit.historyId, createdAtMs: hit.createdAtMs, preview: hit.passage}))
    : view.history.filter((entry) => entry.preview.toLocaleLowerCase().includes(query));
  for (const item of entries) {
    const button = makeElement('button', item.id === selectedHistoryId ? 'active' : '');
    button.type = 'button';
    button.append(makeElement('small', '', dateLabel(item.createdAtMs)), makeElement('span', '', item.preview));
    const note = view.history.find((entry) => entry.id === item.id)?.formattingNote;
    if (note) button.append(makeElement('em', 'record-note', note));
    button.addEventListener('click', () => { void selectHistory(item.id); });
    list.append(button);
  }
  if (!list.childNodes.length) {
    const empty = !view.historyEnabled ? 'History is off.' : query ? 'No matches. Try another phrase.' : 'No dictations yet.';
    list.append(makeElement('div', 'record-row', empty));
  }
  node('history-list').replaceChildren(list);
}

async function startHistorySearch() {
  const query = node('history-query').value.trim();
  if (!query) {
    historyGeneration = null;
    historyHits = null;
    node('history-status').textContent = '';
    if (appView) renderHistory(appView);
    return;
  }
  historyHits = null;
  historyGeneration = null;
  node('history-status').textContent = 'Searching locally…';
  if (appView) renderHistory(appView);
  try {
    const generation = await hostInvoke('plugin:phorminx-host|host_library_search', {
      query,
      semantic: node('history-mode').value === 'semantic',
    });
    if (query === node('history-query').value.trim()) historyGeneration = generation;
  } catch (error) { node('history-status').textContent = String(error); }
}

async function refreshLibrary() {
  if (!hostInvoke || currentRoute !== 'history' || historyGeneration === null) return;
  try {
    const update = await hostInvoke('plugin:phorminx-host|host_library_update');
    if (!update || update.generation !== historyGeneration) return;
    historyHits = update.hits;
    node('history-status').textContent = update.status;
    if (appView) renderHistory(appView);
  } catch (error) { node('history-status').textContent = String(error); }
}

function renderLexicon(view) {
  const fragment = document.createDocumentFragment();
  for (const item of view.lexicon) {
    const button = makeElement('button');
    button.type = 'button';
    button.append(makeElement('small', '', `${item.enabled ? 'ACTIVE' : 'OFF'} · ${item.language || 'ANY LANGUAGE'}`), makeElement('span', '', `${item.alias} → ${item.canonical}`));
    button.addEventListener('click', () => openLexiconEditor(item));
    fragment.append(button);
  }
  if (!view.lexicon.length) fragment.append(makeElement('div', 'record-row', 'No lexicon entries.'));
  node('lexicon-list').replaceChildren(fragment);
}

function openLexiconEditor(item = null) {
  node('lexicon-form').reset();
  node('lexicon-editor-title').textContent = item ? 'Edit entry' : 'New entry';
  node('lexicon-id').value = item?.id || '';
  node('lexicon-alias').value = item?.alias || '';
  node('lexicon-canonical').value = item?.canonical || '';
  node('lexicon-language').value = item?.language || '';
  node('lexicon-app').value = item?.appExecutable || '';
  node('lexicon-case').value = item?.casePolicy || 'preserve-input';
  node('lexicon-enabled').checked = item?.enabled ?? true;
  node('delete-lexicon').hidden = !item;
  node('lexicon-result').textContent = '';
  showRoute('lexicon-editor');
  node('lexicon-alias').focus();
}

function renderProfiles(view) {
  const fragment = document.createDocumentFragment();
  for (const item of view.profiles) {
    const button = makeElement('button');
    button.type = 'button';
    button.append(makeElement('small', '', item.executable), makeElement('span', '', item.summary));
    button.addEventListener('click', async () => {
      try {
        const detail = await hostInvoke('plugin:phorminx-host|host_profile_detail', {executable: item.executable});
        if (detail) openProfileEditor(detail);
      } catch (error) { node('status').textContent = String(error); }
    });
    fragment.append(button);
  }
  if (!view.profiles.length) fragment.append(makeElement('div', 'record-row', 'No application profiles.'));
  node('profile-list').replaceChildren(fragment);
}

// Below this size (roughly a 4B model) local models tend to return plain
// prose for Structured and Custom instructions; see the formatting bench.
const STRUCTURED_MODEL_BYTES = 4 * 1024 * 1024 * 1024;

function updateFormattingHint() {
  const hint = node('formatting-hint');
  const strength = node('formatting-strength').value;
  const name = node('formatting-model').value;
  const model = appView?.ollamaModels.find((entry) => entry.name === name);
  const small = model?.sizeBytes && model.sizeBytes < STRUCTURED_MODEL_BYTES;
  const restructures = strength === 'structured' || strength === 'custom';
  hint.textContent = restructures && small
    ? `${name} is a small model. Structured and Custom work best with a 7B-class model such as qwen2.5:7b-instruct.`
    : '';
  hint.hidden = !hint.textContent;
}

function renderModels(view, notice) {
  node('selected-whisper').textContent = `Selected: ${view.settings.accurateModel}`;
  node('vosk-state').textContent = view.systems.find((system) => system.name === 'Vosk')?.message || 'Checking assets…';
  const fragment = document.createDocumentFragment();
  for (const model of view.ollamaModels) {
    const row = makeElement('div', 'record-row');
    const size = model.sizeBytes ? ` · ${(model.sizeBytes / 1048576).toFixed(0)} MiB` : '';
    const structured = model.sizeBytes && model.sizeBytes >= STRUCTURED_MODEL_BYTES ? ' · good for Structured' : '';
    row.append(makeElement('small', '', model.selected ? 'SELECTED' : 'INSTALLED'), makeElement('span', '', `${model.name}${size}${structured}`));
    fragment.append(row);
  }
  if (!view.ollamaModels.length) fragment.append(makeElement('div', 'record-row', view.systems.find((system) => system.name === 'Ollama')?.message || 'No local models found.'));
  node('ollama-models')?.replaceChildren(fragment);
  node('model-result').textContent = notice || '';
}

function renderSetup(view) {
  node('setup-stage').textContent = view.stage;
  node('setup-summary').textContent = view.summary;
  node('setup-start').hidden = !view.canStart;
  const fragment = document.createDocumentFragment();
  for (const item of view.actions) {
    const row = makeElement('div', 'setup-action');
    const description = makeElement('div', 'setup-action-description');
    description.append(makeElement('strong', '', item.title), makeElement('p', '', item.detail));
    if (item.progressPercent !== null) description.append(makeElement('small', '', `${item.progressPercent}%`));
    const button = makeElement('button', '', item.running ? 'Cancel' : item.complete ? 'Complete' : item.canRetry ? 'Retry' : 'Start');
    button.type = 'button';
    button.disabled = !item.running && (!item.available || item.complete);
    button.addEventListener('click', async () => {
      if (item.running) {
        try { await hostInvoke('plugin:phorminx-host|host_cancel_setup_action', {id: item.id}); await refreshSetup(); }
        catch (error) { node('setup-summary').textContent = String(error); }
      } else if (item.consent.length) {
        pendingSetupAction = item.id;
        node('setup-consent-title').textContent = item.title;
        node('setup-consent-details').replaceChildren(...item.consent.map((line) => makeElement('p', '', line)));
        node('setup-consent').returnValue = '';
        node('setup-consent').showModal();
      } else void startSetupAction(item.id, false);
    });
    row.append(description, button);
    fragment.append(row);
  }
  if (!view.actions.length) fragment.append(makeElement('div', 'record-row', 'No setup changes proposed.'));
  node('setup-list').replaceChildren(fragment);
}

const REPAIR_STATES = {ok: ['✓', 'Ready'], missing: ['!', 'Missing'], unused: ['–', 'Not set up']};

async function refreshRepair() {
  if (!hostInvoke) return;
  node('repair-status').textContent = 'Checking…';
  try {
    renderRepair(await hostInvoke('host_repair_status'));
    node('repair-status').textContent = '';
  } catch (error) { node('repair-status').textContent = String(error); }
}

function renderRepair(items) {
  const list = node('repair-list');
  list.replaceChildren();
  for (const item of items) {
    const row = makeElement('div', `repair-row repair-${item.state}`);
    row.setAttribute('role', 'listitem');
    const [mark, word] = REPAIR_STATES[item.state] || ['?', item.state];
    const badge = makeElement('span', 'repair-badge', `${mark} ${word}`);
    const text = makeElement('div', 'repair-text');
    text.append(makeElement('small', '', item.workflow.toUpperCase()), makeElement('strong', '', item.label), makeElement('span', '', item.detail));
    row.append(badge, text);
    if (item.fix) {
      const fix = makeElement('button', '', item.fix === 'setup' ? 'Run setup again' : item.fix === 'ocr' ? 'Install' : 'Open Models');
      fix.type = 'button';
      fix.addEventListener('click', () => {
        if (item.fix === 'setup') node('bootstrap-consent').showModal();
        else if (item.fix === 'models') showRoute('models');
        else void hostInvoke('investigation_models', {kind: 'install'})
          .then(() => { node('repair-status').textContent = 'Downloading the screenshot text models…'; })
          .catch((error) => { node('repair-status').textContent = String(error); });
      });
      row.append(fix);
    }
    list.append(row);
  }
}

async function refreshSetup() {
  if (!hostInvoke) return;
  try {
    renderSetup(await hostInvoke('plugin:phorminx-host|host_setup_snapshot'));
    renderBootstrap(await hostInvoke('host_bootstrap_snapshot'));
  }
  catch (error) { node('setup-summary').textContent = String(error); }
}

let lastBootstrapState = '';

function renderBootstrap(view) {
  node('bootstrap-card').hidden = !view.available;
  if (!view.available) return;
  const working = view.state === 'working' || view.state === 'cancelling';
  if (view.repair) {
    node('bootstrap-label').textContent = 'REPAIR';
    node('bootstrap-title').textContent = 'Run setup again';
    node('bootstrap-intro').textContent = 'Reinstalls whatever is missing from the starter (speech recognition, GPU acceleration, Ollama, qwen2.5:1.5b and nomic-embed-text) and skips what is already there. Settings and saved text are kept.';
  }
  node('bootstrap-start').disabled = working;
  node('bootstrap-start').textContent = ['failed', 'cancelled'].includes(view.state) ? 'Retry' : view.repair ? 'Run setup again' : 'Prepare everything';
  // A repair can always run again; the first-run starter is done once complete.
  node('bootstrap-start').hidden = view.state === 'complete' && !view.repair;
  if (lastBootstrapState === 'working' && view.state !== 'working') void refreshRepair();
  lastBootstrapState = view.state;
  node('bootstrap-cancel').hidden = !working;
  node('bootstrap-cancel').disabled = view.state === 'cancelling';
  node('bootstrap-progress').textContent = working ? `Step ${view.step} of ${view.total}` : view.state === 'complete' ? 'Complete' : '';
  node('bootstrap-message').textContent = view.message;
  if (working) {
    node('setup-start').hidden = true;
    for (const button of node('setup-list').querySelectorAll('button')) button.disabled = true;
  }
}

async function refreshProvider() {
  if (!hostInvoke) return;
  try {
    const provider = await hostInvoke('meeting_provider');
    if (providerDirty) return;
    node('provider-kind').value = provider.kind;
    node('provider-model').value = provider.model;
    node('provider-fallback').value = provider.fallbackModel;
    node('provider-endpoint').value = provider.endpoint;
    node('provider-preset').value = provider.preset;
    node('provider-key').value = '';
    node('provider-key').placeholder = provider.hasCredential ? 'Saved key retained when blank' : 'Optional for local Ollama';
    node('provider-clear-key').checked = false;
  } catch (error) { node('provider-result').textContent = String(error); }
}

async function startSetupAction(id, consentConfirmed) {
  try {
    await hostInvoke('plugin:phorminx-host|host_start_setup_action', {id, consentConfirmed});
    await refreshSetup();
  } catch (error) { node('setup-summary').textContent = String(error); }
}

function openProfileEditor(item = null) {
  node('profile-form').reset();
  node('profile-editor-title').textContent = item ? 'Edit profile' : 'New profile';
  node('profile-original').value = item?.executable || '';
  node('profile-executable').value = item?.executable || '';
  node('profile-style').value = item?.formattingStyle || 'light';
  node('profile-language').value = item?.language || '';
  node('profile-insertion').value = item?.insertionPreference || 'automatic';
  node('profile-instructions').value = item?.customInstructions || '';
  node('profile-deny').checked = item?.deny || false;
  node('delete-profile').hidden = !item;
  node('profile-result').textContent = '';
  showRoute('profile-editor');
  node('profile-executable').focus();
}

async function selectHistory(id) {
  selectedHistoryId = id;
  if (appView) renderHistory(appView);
  const item = appView?.history.find((entry) => entry.id === id);
  for (const option of node('history-variant').options) {
    option.disabled = item ? !item.variants[option.value] : option.value !== 'output';
  }
  if (node('history-variant').selectedOptions[0]?.disabled) node('history-variant').value = 'output';
  node('history-note').textContent = item?.formattingNote || '';
  node('history-note').hidden = !item?.formattingNote;
  await loadHistoryText();
}

async function loadHistoryText() {
  const id = selectedHistoryId;
  if (id === null) return;
  const variant = node('history-variant').value;
  node('history-detail').textContent = 'Opening text…';
  try {
    const text = await hostInvoke('plugin:phorminx-host|host_history_text', {id, variant});
    if (selectedHistoryId === id && node('history-variant').value === variant) node('history-detail').textContent = text ?? 'This text version is unavailable.';
  } catch (error) {
    if (selectedHistoryId === id && node('history-variant').value === variant) node('history-detail').textContent = String(error);
  }
}

function renderList(targetId, items, emptyText, format) {
  const fragment = document.createDocumentFragment();
  for (const item of items) {
    const row = makeElement('div', 'record-row');
    const [heading, body] = format(item);
    row.append(makeElement('small', '', heading), makeElement('span', '', body));
    fragment.append(row);
  }
  if (!items.length) fragment.append(makeElement('div', 'record-row', emptyText));
  node(targetId).replaceChildren(fragment);
}

function renderActions(actions) {
  const fragment = document.createDocumentFragment();
  for (const item of actions) {
    const row = makeElement('div', 'action-row');
    const label = item.launcherSlot ? `${item.launcherSlot} · ${item.method}` : item.method;
    const text = makeElement('div', 'action-description');
    text.append(makeElement('small', '', label), makeElement('strong', '', item.name), makeElement('span', '', item.endpoint));
    const start = makeElement('button', '', 'Record for action');
    start.type = 'button';
    start.addEventListener('click', async () => {
      start.disabled = true;
      const started = await action('trigger-action', item.id);
      if (!started) start.disabled = false;
    });
    const edit = makeElement('button', '', 'Edit');
    edit.type = 'button';
    edit.addEventListener('click', () => openActionEditor(item));
    const buttons = makeElement('div', 'action-row-buttons');
    buttons.append(edit, start);
    row.append(text, buttons);
    fragment.append(row);
  }
  if (!actions.length) fragment.append(makeElement('div', 'record-row', 'No saved actions.'));
  node('actions-list').replaceChildren(fragment);
}

async function refreshActions() {
  if (!hostInvoke) return;
  try {
    knownActions = await hostInvoke('meeting_actions');
    renderActions(knownActions);
  }
  catch (error) { node('actions-list').replaceChildren(makeElement('div', 'record-row', String(error))); }
}

function openActionEditor(item = null) {
  node('action-form').reset();
  node('action-editor-title').textContent = item ? 'Edit action' : 'New action';
  node('action-id').value = item?.id || '';
  node('action-name').value = item?.name || '';
  node('action-slot').value = item?.launcherSlot || '';
  node('action-method').value = item?.method || 'POST';
  node('action-endpoint').value = item?.endpoint || '';
  node('action-payload-mode').value = item?.payloadMode || 'template';
  node('action-template').value = item?.payloadTemplate || '';
  node('action-model').value = item?.payloadModel || '';
  node('action-prompt').value = item?.payloadPrompt || '';
  node('action-auth').value = '';
  node('action-headers').value = '';
  node('delete-action').hidden = !item;
  node('action-result').textContent = '';
  showRoute('action-editor');
  node('action-name').focus();
}

async function waitForAction(predicate, afterRevision = null) {
  for (let attempt = 0; attempt < 25; attempt++) {
    await new Promise((resolve) => setTimeout(resolve, 120));
    if (afterRevision !== null) {
      const meeting = await hostInvoke('meeting_snapshot', {since: null});
      if (!meeting || meeting.actionRevision <= afterRevision) continue;
    }
    const actions = await hostInvoke('meeting_actions');
    if (predicate(actions)) return true;
  }
  return false;
}

function renderApp(view, runtimeStatus, notice) {
  appView = view;
  renderHome(view, runtimeStatus);
  renderHistory(view);
  renderLexicon(view);
  renderProfiles(view);
  renderModels(view, notice);
  if (!preferencesDirty) {
    node('launcher-shortcut').value = view.settings.launcherShortcut;
    node('app-theme').value = view.settings.theme;
    node('recording-mode').value = view.settings.recordingMode;
    node('show-live-text').checked = view.settings.showLiveText !== false;
  }
  if (!dictationDirty) {
    node('recognition-mode').value = view.settings.recognitionMode;
    node('accurate-backend').value = view.settings.accurateBackend;
    node('recognition-language').value = view.settings.language;
    node('minimum-rms').value = view.settings.minimumRms;
    node('formatting-strength').value = view.settings.formattingStrength;
    node('model-residency').value = view.settings.modelResidency;
    node('formatting-passes').value = String(view.settings.formattingPasses || 2);
    const select = node('formatting-model');
    const options = [makeElement('option', '', 'No model')];
    options[0].value = '';
    for (const model of view.ollamaModels) {
      const option = makeElement('option', '', model.name);
      option.value = model.name;
      options.push(option);
    }
    if (view.settings.formattingModel && !view.ollamaModels.some((model) => model.name === view.settings.formattingModel)) {
      const option = makeElement('option', '', `${view.settings.formattingModel} · unavailable`);
      option.value = view.settings.formattingModel;
      options.push(option);
    }
    select.replaceChildren(...options);
    select.value = view.settings.formattingModel || '';
  }
  updateFormattingHint();
  if (!privacyDirty) {
    node('history-retention').value = view.settings.historyRetention;
    node('embedding-model').value = view.settings.embeddingModel || '';
    node('launch-at-login').checked = view.settings.launchAtLogin;
    node('direct-shortcut').value = view.settings.directShortcut || '';
  }
  document.documentElement.dataset.theme = view.settings.theme === 'system'
    ? (window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light')
    : view.settings.theme;
}

async function refreshApp() {
  if (!hostInvoke || appRefreshInFlight) return;
  appRefreshInFlight = true;
  try {
    const [host, view] = await Promise.all([
      hostInvoke('plugin:phorminx-host|host_snapshot'),
      hostInvoke('plugin:phorminx-host|host_app_snapshot'),
    ]);
    renderApp(view, host.runtimeStatus, host.notice);
    node('status').textContent = host.notice || `${host.runtimeStatus} · Local`;
    if (host.route !== lastHostRoute) {
      const target = host.route.startsWith('settings') ? 'settings' : host.route === 'meetings' ? 'live' : host.route;
      showRoute(node(target) || target === 'settings' ? target : 'home', false);
      lastHostRoute = host.route;
    }
  } catch (error) { node('status').textContent = String(error); }
  finally { appRefreshInFlight = false; }
}

let benchmarksShown = false;
let benchmarksOnline = false;

let benchmarksView = null;

function renderBenchmarks(collection) {
  benchmarksView = window.PhorminxBenchmarks.render(node('benchmarks-view'), collection, {
    loadReport: (id) => hostInvoke('benchmarks_report', {id, online: benchmarksOnline}),
    onRefresh: () => void refreshBenchmarks(),
    models: window.PhorminxModels && {
      entries: () => window.PhorminxModels.entries(),
      use: async (slot, name) => {
        const message = await hostInvoke('models_assign', {slot, name});
        void window.PhorminxModels.refresh();
        return message;
      },
      download: async (name) => {
        await installModel(name, null);
        return `Downloading ${name}. Progress shows on Models; the list updates when it is done.`;
      },
    },
  });
}

async function refreshBenchmarks() {
  try {
    const collection = await hostInvoke('benchmarks_load', {online: true});
    benchmarksOnline = collection.source === 'online';
    renderBenchmarks(collection);
  } catch (error) { node('status').textContent = String(error); }
}

// Shows the bundled results at once, then quietly checks for newer published ones.
async function showBenchmarks() {
  if (benchmarksShown || !hostInvoke || !window.PhorminxBenchmarks) return;
  benchmarksShown = true;
  try {
    renderBenchmarks(await hostInvoke('benchmarks_load', {online: false}));
    void refreshBenchmarks();
  } catch (error) { benchmarksShown = false; node('status').textContent = String(error); }
}

for (const button of document.querySelectorAll('[data-route]')) {
  button.addEventListener('click', () => showRoute(button.dataset.route));
}
for (const button of document.querySelectorAll('[data-settings-tab]')) button.addEventListener('click', () => showSettingsTab(button.dataset.settingsTab));
node('update-daily').addEventListener('change', (event) => void updateAction('daily-checks', event.target.checked));
node('update-automatic').addEventListener('change', (event) => void updateAction('automatic-install', event.target.checked));
node('setup-auto-updates').addEventListener('change', (event) => void updateAction('automatic-install', event.target.checked));
node('update-check').addEventListener('click', () => void updateAction('check'));
node('update-download').addEventListener('click', () => void updateAction('download'));
node('update-install').addEventListener('click', () => void updateAction('install-on-exit'));
node('update-restart').addEventListener('click', () => void updateAction('restart-now'));
node('history-query').addEventListener('input', () => {
  if (historySearchTimer) clearTimeout(historySearchTimer);
  historySearchTimer = setTimeout(() => { void startHistorySearch(); }, 250);
  if (appView) renderHistory(appView);
});
node('history-mode').addEventListener('change', () => { void startHistorySearch(); });
node('history-variant').addEventListener('change', () => { void loadHistoryText(); });
node('test-dictation').addEventListener('click', async () => {
  try { await hostInvoke('plugin:phorminx-host|host_action', {kind: 'test-dictation'}); }
  catch (error) { node('status').textContent = String(error); }
});
node('open-feedback').addEventListener('click', async () => {
  try { await hostInvoke('plugin:phorminx-host|host_open_feedback'); }
  catch (error) { node('status').textContent = String(error); }
});
node('new-action').addEventListener('click', () => openActionEditor());
node('new-lexicon').addEventListener('click', () => openLexiconEditor());
node('new-profile').addEventListener('click', () => openProfileEditor());
node('provider-form').addEventListener('input', () => { providerDirty = true; });
node('provider-form').addEventListener('change', () => { providerDirty = true; });
node('provider-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  node('provider-result').textContent = 'Saving…';
  try {
    const before = await hostInvoke('meeting_provider');
    await hostInvoke('meeting_save_provider', {provider: {
      kind: node('provider-kind').value,
      model: node('provider-model').value.trim(),
      fallbackModel: node('provider-fallback').value.trim(),
      endpoint: node('provider-endpoint').value.trim(),
      apiKey: node('provider-key').value,
      clearCredential: node('provider-clear-key').checked,
      preset: node('provider-preset').value,
    }});
    let saved = false;
    for (let attempt = 0; attempt < 25; attempt++) {
      await new Promise((resolve) => setTimeout(resolve, 120));
      const current = await hostInvoke('meeting_provider');
      if (current.revision > before.revision) { saved = true; break; }
    }
    if (!saved) {
      const meeting = await hostInvoke('meeting_snapshot', {since: null});
      throw new Error(meeting?.notice || 'Assistant settings were not confirmed.');
    }
    providerDirty = false;
    node('provider-result').textContent = 'Saved.';
    await refreshProvider();
  } catch (error) { node('provider-result').textContent = String(error); }
});
node('setup-consent').addEventListener('close', () => {
  const id = pendingSetupAction;
  pendingSetupAction = null;
  if (node('setup-consent').returnValue === 'confirm' && id) void startSetupAction(id, true);
});
node('bootstrap-start').addEventListener('click', () => node('bootstrap-consent').showModal());
node('repair-refresh').addEventListener('click', () => void refreshRepair());
node('bootstrap-cancel').addEventListener('click', async () => {
  try { renderBootstrap(await hostInvoke('host_cancel_bootstrap')); }
  catch (error) { node('bootstrap-message').textContent = String(error); }
});
node('bootstrap-consent').addEventListener('close', async () => {
  if (node('bootstrap-consent').returnValue !== 'confirm') return;
  try { renderBootstrap(await hostInvoke('host_start_bootstrap')); }
  catch (error) { node('bootstrap-message').textContent = String(error); }
});
node('setup-start').addEventListener('click', async () => {
  try { await hostInvoke('plugin:phorminx-host|host_action', {kind: 'start-after-setup'}); }
  catch (error) { node('setup-summary').textContent = String(error); }
});
for (const button of document.querySelectorAll('[data-whisper-model]')) {
  button.addEventListener('click', async () => {
    try {
      node('model-result').textContent = 'Starting verified download…';
      await hostInvoke('plugin:phorminx-host|host_select_whisper_model', {manifestId: button.dataset.whisperModel});
    } catch (error) { node('model-result').textContent = String(error); }
  });
}
node('install-vosk').addEventListener('click', async () => {
  try {
    node('model-result').textContent = 'Installing verified Vosk assets…';
    await hostInvoke('plugin:phorminx-host|host_install_vosk');
  } catch (error) { node('model-result').textContent = String(error); }
});
node('profile-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  node('profile-result').textContent = 'Saving…';
  try {
    await hostInvoke('plugin:phorminx-host|host_save_profile', {
      originalExecutable: node('profile-original').value || null,
      executable: node('profile-executable').value.trim(),
      formattingStyle: node('profile-style').value,
      customInstructions: node('profile-instructions').value.trim() || null,
      language: node('profile-language').value.trim() || null,
      insertionPreference: node('profile-insertion').value,
      deny: node('profile-deny').checked,
    });
    await refreshApp();
    showRoute('profiles');
  } catch (error) { node('profile-result').textContent = String(error); }
});
node('delete-profile').addEventListener('click', async () => {
  const executable = node('profile-original').value;
  if (!executable || !window.confirm('Delete this application profile?')) return;
  node('profile-result').textContent = 'Deleting…';
  try {
    await hostInvoke('plugin:phorminx-host|host_delete_profile', {executable});
    await refreshApp();
    showRoute('profiles');
  } catch (error) { node('profile-result').textContent = String(error); }
});
node('lexicon-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  node('lexicon-result').textContent = 'Saving…';
  try {
    await hostInvoke('plugin:phorminx-host|host_save_lexicon', {
      id: node('lexicon-id').value ? Number(node('lexicon-id').value) : null,
      canonical: node('lexicon-canonical').value.trim(),
      alias: node('lexicon-alias').value.trim(),
      language: node('lexicon-language').value.trim() || null,
      appExecutable: node('lexicon-app').value.trim() || null,
      casePolicy: node('lexicon-case').value,
      enabled: node('lexicon-enabled').checked,
    });
    await refreshApp();
    showRoute('lexicon');
  } catch (error) { node('lexicon-result').textContent = String(error); }
});
node('delete-lexicon').addEventListener('click', async () => {
  const id = Number(node('lexicon-id').value);
  if (!id || !window.confirm('Delete this lexicon entry?')) return;
  node('lexicon-result').textContent = 'Deleting…';
  try {
    await hostInvoke('plugin:phorminx-host|host_delete_lexicon', {id});
    await refreshApp();
    showRoute('lexicon');
  } catch (error) { node('lexicon-result').textContent = String(error); }
});
node('action-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  const id = node('action-id').value;
  const name = node('action-name').value.trim();
  const before = new Set(knownActions.map((item) => item.id));
  node('action-result').textContent = 'Saving…';
  try {
    const beforeMeeting = await hostInvoke('meeting_snapshot', {since: null});
    await hostInvoke('meeting_save_action', {action: {
      id, name,
      endpoint: node('action-endpoint').value.trim(),
      method: node('action-method').value,
      payloadTemplate: node('action-template').value,
      authorization: node('action-auth').value,
      headersJson: node('action-headers').value,
      launcherSlot: node('action-slot').value ? Number(node('action-slot').value) : null,
      payloadMode: node('action-payload-mode').value,
      payloadModel: node('action-model').value.trim(),
      payloadPrompt: node('action-prompt').value,
    }});
    if (await waitForAction((actions) => actions.some((item) => id ? item.id === id && item.name === name : !before.has(item.id) && item.name === name), beforeMeeting.actionRevision)) {
      await refreshActions();
      showRoute('actions');
    } else {
      const snapshot = await hostInvoke('meeting_snapshot', {since: null});
      node('action-result').textContent = snapshot?.notice || 'The action was not confirmed. Check its settings and try again.';
    }
  } catch (error) { node('action-result').textContent = String(error); }
});
node('delete-action').addEventListener('click', async () => {
  const id = node('action-id').value;
  if (!id || !window.confirm('Delete this saved action?')) return;
  node('action-result').textContent = 'Deleting…';
  try {
    await hostInvoke('meeting_delete_action', {id});
    if (await waitForAction((actions) => !actions.some((item) => item.id === id))) {
      await refreshActions();
      showRoute('actions');
    } else {
      const snapshot = await hostInvoke('meeting_snapshot', {since: null});
      node('action-result').textContent = snapshot?.notice || 'Deletion was not confirmed. Please retry.';
    }
  } catch (error) { node('action-result').textContent = String(error); }
});
node('preferences-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  if (capturingShortcut) await endShortcutCapture();
  node('preferences-result').textContent = 'Saving…';
  try {
    await hostInvoke('plugin:phorminx-host|host_save_preferences', {
      theme: node('app-theme').value,
      recordingMode: node('recording-mode').value,
      launcherShortcut: node('launcher-shortcut').value.trim(),
      showLiveText: node('show-live-text').checked,
    });
    preferencesDirty = false;
    node('preferences-result').textContent = 'Saved.';
    await refreshApp();
  } catch (error) { node('preferences-result').textContent = String(error); }
});
node('preferences-form').addEventListener('input', () => { preferencesDirty = true; });
node('preferences-form').addEventListener('change', () => { preferencesDirty = true; });
node('dictation-form').addEventListener('input', () => { dictationDirty = true; });
node('dictation-form').addEventListener('change', () => { dictationDirty = true; updateFormattingHint(); });
node('privacy-form').addEventListener('input', () => { privacyDirty = true; });
node('privacy-form').addEventListener('change', () => { privacyDirty = true; });
node('dictation-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  node('dictation-result').textContent = 'Saving…';
  try {
    await hostInvoke('plugin:phorminx-host|host_save_dictation_settings', {
      mode: node('recognition-mode').value,
      backend: node('accurate-backend').value,
      microphone: node('dictation-microphone').value || null,
      language: node('recognition-language').value.trim(),
      minimumRms: node('minimum-rms').value,
      strength: node('formatting-strength').value,
      model: node('formatting-model').value || null,
      lifecycle: node('model-residency').value,
      customInstructions: node('custom-instructions').value.trim() || null,
      passes: Number(node('formatting-passes').value),
    });
    dictationDirty = false;
    node('dictation-result').textContent = 'Saved.';
    await refreshApp();
  } catch (error) { node('dictation-result').textContent = String(error); }
});
node('privacy-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  node('privacy-result').textContent = 'Saving…';
  try {
    await hostInvoke('plugin:phorminx-host|host_save_privacy_settings', {
      retention: node('history-retention').value,
      embeddingModel: node('embedding-model').value.trim() || null,
      launchAtLogin: node('launch-at-login').checked,
      directShortcut: node('direct-shortcut').value.trim() || null,
    });
    privacyDirty = false;
    node('privacy-result').textContent = 'Saved.';
    await refreshApp();
  } catch (error) { node('privacy-result').textContent = String(error); }
});
node('capture-shortcut').addEventListener('click', async () => {
  if (capturingShortcut) { await endShortcutCapture(); return; }
  try {
    await hostInvoke('plugin:phorminx-host|host_action', {kind: 'begin-shortcut-capture'});
    capturingShortcut = true;
    preferencesDirty = true;
    node('capture-shortcut').textContent = 'Cancel capture';
    node('launcher-shortcut').readOnly = true;
    node('preferences-result').textContent = 'Press the new shortcut. Esc cancels.';
  } catch (error) { node('preferences-result').textContent = String(error); }
});
document.addEventListener('keydown', (event) => {
  if (!capturingShortcut) return;
  event.preventDefault();
  event.stopPropagation();
  if (event.key === 'Escape') {
    if (appView) node('launcher-shortcut').value = appView.settings.launcherShortcut;
    void endShortcutCapture();
    return;
  }
  const key = shortcutKey(event);
  if (!key) return;
  const parts = [];
  if (event.ctrlKey) parts.push('Ctrl');
  if (event.altKey) parts.push('Alt');
  if (event.shiftKey) parts.push('Shift');
  if (event.metaKey) parts.push('Win');
  parts.push(key);
  node('launcher-shortcut').value = parts.join('+');
  node('preferences-result').textContent = 'Shortcut captured. Save preferences to apply it.';
  void endShortcutCapture();
}, true);

if (hostInvoke) { void refreshApp(); setInterval(() => { void refreshApp(); }, 4000); }
else node('status').textContent = 'Open this page inside Phorminx.';
if (hostInvoke) setInterval(() => { if (currentRoute === 'setup') void refreshSetup(); }, 1500);
if (hostInvoke) setInterval(() => { if (currentRoute === 'settings' && !node('settings-updates').hidden) void refreshUpdates(); }, 1500);
if (hostInvoke) setInterval(() => { void refreshLibrary(); }, 450);

// Model fields pick from the shared registry, offering only the right kind.
const MODEL_FILTERS = [['all', 'All'], ['installed', 'Installed'], ['language', 'Language'], ['embedding', 'Search'], ['vision', 'Vision'], ['speech', 'Speech'], ['ocr', 'Screenshot text']];
const MODEL_TAG_LABELS = {language: 'Language', embedding: 'Search', vision: 'Vision', tools: 'Tools', speech: 'Speech', ocr: 'Screenshot text'};
const MODEL_FAMILIES = {ollama: 'Ollama', whisper: 'Whisper', vosk: 'Vosk', ocr: 'PaddleOCR'};
let modelFilter = 'all';

function renderRegistry(entries) {
  const query = node('model-search').value.trim().toLowerCase();
  const list = node('model-registry');
  const shown = entries.filter((entry) => {
    if (modelFilter === 'installed' && !entry.installed) return false;
    if (!['all', 'installed'].includes(modelFilter) && !entry.tags.includes(modelFilter)) return false;
    const haystack = [entry.name, MODEL_FAMILIES[entry.family], entry.note, ...entry.tags.map((tag) => MODEL_TAG_LABELS[tag] || tag), ...entry.usedBy].join(' ').toLowerCase();
    return !query || haystack.includes(query);
  });
  const rows = shown.map((entry) => {
    const row = makeElement('div', `model-entry${entry.installed ? ' installed' : ''}`);
    row.setAttribute('role', 'listitem');
    const head = makeElement('div', 'model-entry-head');
    const size = entry.sizeBytes ? ` · ${entry.sizeBytes >= 1073741824 ? `${(entry.sizeBytes / 1073741824).toFixed(1)} GB` : `${Math.round(entry.sizeBytes / 1048576)} MB`}` : '';
    head.append(makeElement('strong', '', entry.name), makeElement('small', '', `${MODEL_FAMILIES[entry.family] || entry.family}${size} · ${entry.installed ? 'Installed' : 'Not installed'}`));
    const tags = makeElement('div', 'model-tags');
    for (const tag of entry.tags) tags.append(makeElement('span', 'model-tag', MODEL_TAG_LABELS[tag] || tag));
    for (const feature of entry.usedBy) tags.append(makeElement('span', 'model-tag used', `Used by ${feature}`));
    row.append(head, tags);
    if (entry.note) row.append(makeElement('p', 'setting-hint', entry.note));
    if (!entry.installed && entry.family === 'ollama') {
      const install = makeElement('button', '', 'Install');
      install.type = 'button';
      install.addEventListener('click', () => void installModel(entry.name, entry.tags[0]));
      row.append(install);
    }
    return row;
  });
  if (!rows.length) rows.push(makeElement('p', 'setting-hint', 'No model matches. Install one from Ollama by its exact name below.'));
  list.replaceChildren(...rows);
}

function renderModelFilters() {
  const group = node('model-filters');
  group.replaceChildren(...MODEL_FILTERS.map(([id, label]) => {
    const chip = makeElement('button', 'chip', label);
    chip.type = 'button';
    chip.setAttribute('aria-pressed', String(modelFilter === id));
    chip.addEventListener('click', () => {
      modelFilter = id;
      renderModelFilters();
      renderRegistry(window.PhorminxModels?.entries() || []);
      group.querySelector(`[aria-pressed="true"]`)?.focus();
    });
    return chip;
  }));
}

let modelPullTimer = null;

async function installModel(name, expected) {
  const status = node('model-install-status');
  try {
    const view = await hostInvoke('models_pull', {name, expected: expected || null});
    status.textContent = view.message;
    clearInterval(modelPullTimer);
    modelPullTimer = setInterval(async () => {
      const pull = await hostInvoke('models_pull_status');
      status.textContent = pull.percent != null && pull.state === 'pulling' ? `${pull.message} (${pull.percent}%)` : pull.message;
      if (pull.state !== 'pulling') {
        clearInterval(modelPullTimer);
        void window.PhorminxModels?.refresh();
      }
    }, 1000);
  } catch (error) { status.textContent = String(error); }
}

if (window.PhorminxModels) {
  const external = () => node('provider-kind').value !== 'ollama';
  PhorminxModels.attach('formatting-model', {kind: 'language', empty: 'No model'});
  PhorminxModels.attach('embedding-model', {kind: 'embedding', empty: 'Off'});
  const answer = PhorminxModels.attach('provider-model', {kind: 'language', empty: 'Choose a model', free: external});
  PhorminxModels.attach('provider-fallback', {kind: 'language', empty: 'No fallback'});
  PhorminxModels.attach('translation-model', {kind: 'language', empty: 'Choose an installed model'});
  PhorminxModels.attach('action-model', {kind: 'language', empty: 'Choose a model'});
  node('provider-kind').addEventListener('change', () => answer?.render());
  document.addEventListener('phorminx:models', (event) => { renderRegistry(event.detail); benchmarksView?.redraw(); });
  node('model-search').addEventListener('input', () => renderRegistry(PhorminxModels.entries()));
  node('model-install-form').addEventListener('submit', (event) => {
    event.preventDefault();
    const name = node('model-install-name').value.trim();
    if (name) void installModel(name, null);
  });
  renderModelFilters();
  void PhorminxModels.refresh();
}

// Feedback: reports and comments sent from this computer, with their status.
const FEEDBACK_STATES = {open: 'Open', solved: '✓ Solved', shared: 'Shared · waiting for the team or shown', removed: 'Taken down'};

function renderFeedbackHistory(items) {
  const rows = items.map((item) => {
    const row = makeElement('div', `record-row feedback-row feedback-${item.status}`);
    row.setAttribute('role', 'listitem');
    const head = makeElement('div', 'feedback-head');
    const sent = new Date(item.sentAtMs).toLocaleString(undefined, {dateStyle: 'medium', timeStyle: 'short'});
    head.append(makeElement('small', '', `${item.kind === 'comment' ? 'COMMENT' : 'REPORT'} · ${sent}`), makeElement('span', 'feedback-state', FEEDBACK_STATES[item.status] || item.status));
    const text = makeElement('p', 'feedback-text', item.text);
    row.append(head, text);
    if (item.resolutionNote) row.append(makeElement('p', 'feedback-note', `From the team: ${item.resolutionNote}`));
    const actions = makeElement('div', 'feedback-actions');
    if (item.kind === 'comment' && item.status === 'shared') {
      const remove = makeElement('button', '', 'Take down from website');
      remove.type = 'button';
      remove.addEventListener('click', async () => {
        if (!window.confirm('Remove this comment from phorminx.net for good?')) return;
        try { await hostInvoke('feedback_remove_comment', {id: item.id}); void refreshFeedbackHistory(); }
        catch (error) { node('feedback-status').textContent = String(error); }
      });
      actions.append(remove);
    }
    const forget = makeElement('button', '', 'Remove from this list');
    forget.type = 'button';
    forget.addEventListener('click', async () => {
      try { await hostInvoke('feedback_forget', {id: item.id}); void refreshFeedbackHistory(); }
      catch (error) { node('feedback-status').textContent = String(error); }
    });
    actions.append(forget);
    row.append(actions);
    return row;
  });
  if (!rows.length) rows.push(makeElement('div', 'record-row', 'Nothing sent yet. Use “New report or comment” to tell the team what happened, or what you like.'));
  node('feedback-list').replaceChildren(...rows);
}

async function refreshFeedbackHistory() {
  if (!hostInvoke) return;
  try {
    const items = await hostInvoke('feedback_history');
    renderFeedbackHistory(items);
    const solved = items.filter((item) => item.status === 'solved').length;
    node('feedback-status').textContent = solved ? `${solved} solved` : '';
  } catch (error) { node('feedback-status').textContent = String(error); }
}

node('feedback-check').addEventListener('click', async () => {
  node('feedback-status').textContent = 'Checking with phorminx.net…';
  try {
    const solved = await hostInvoke('feedback_check_now');
    await refreshFeedbackHistory();
    node('feedback-status').textContent = solved ? `${solved} newly solved.` : 'Up to date.';
  } catch (error) { node('feedback-status').textContent = String(error); }
});
