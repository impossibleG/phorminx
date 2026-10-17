const tauriInvoke = window.__TAURI__?.core?.invoke;
const invoke = tauriInvoke && ((command, args) => tauriInvoke(command.replace(/^plugin:phorminx-host\|/, ''), args));
const element = (id) => document.getElementById(id);
let polling = false;
let pendingEdit = false;
let lastServerText = '';
let editGeneration = 0;
let savePending = false;
let saving = false;
let savePromise = Promise.resolve();

function countBytes(text) { return new TextEncoder().encode(text).length; }
function updateCount() { element('count').textContent = `${countBytes(element('report').value).toLocaleString()} / 12,000 bytes`; }

function saveDraft() {
  if (!invoke) return Promise.resolve();
  savePending = true;
  if (saving) return savePromise;
  saving = true;
  savePromise = (async () => {
    // Keep writes ordered, but coalesce edits made while an IPC write is in
    // flight. An older response must never overwrite the newest local draft.
    while (savePending) {
      savePending = false;
      const generation = editGeneration;
      const text = element('report').value;
      await invoke('plugin:phorminx-host|feedback_update', {
        text,
        diagnostics: element('diagnostics').checked,
      });
      if (generation === editGeneration) {
        pendingEdit = false;
        lastServerText = text;
      }
    }
  })().finally(() => {
    saving = false;
    if (savePending) void saveDraft();
  });
  return savePromise;
}

async function refresh() {
  if (!invoke || polling) return;
  polling = true;
  try {
    const view = await invoke('plugin:phorminx-host|feedback_snapshot');
    if (!pendingEdit && view.text !== lastServerText) {
      element('report').value = view.text;
      lastServerText = view.text;
      updateCount();
    }
    element('report').disabled = view.recording || view.sending;
    element('diagnostics').disabled = view.recording || view.sending;
    if (!pendingEdit) element('diagnostics').checked = view.diagnostics;
    element('record').hidden = view.recording;
    element('record').disabled = view.sending;
    element('stop').hidden = !view.recording;
    element('stop').disabled = view.stopping;
    element('send').disabled = view.recording || view.sending || !element('report').value.trim()
      || countBytes(element('report').value) > 12000;
    element('provisional').textContent = view.provisionalText;
    element('notice').textContent = view.notice;
  } catch (error) { element('notice').textContent = String(error); }
  finally { polling = false; }
}

async function action(kind) {
  if (!invoke) return;
  try {
    if (kind === 'send') await saveDraft();
    await invoke('plugin:phorminx-host|feedback_action', {kind});
    await refresh();
  } catch (error) { element('notice').textContent = String(error); }
}

// Persist every edit to the in-process draft before a blur or window-close can
// discard the renderer. Nothing is transmitted to the feedback service here.
function draftChanged() {
  pendingEdit = true;
  editGeneration++;
  updateCount();
  void saveDraft().catch((error) => { element('notice').textContent = String(error); });
}
element('report').addEventListener('input', draftChanged);
element('diagnostics').addEventListener('change', draftChanged);
element('report').addEventListener('blur', () => { if (pendingEdit) void saveDraft().catch((error) => { element('notice').textContent = String(error); }); });
element('diagnostics').addEventListener('blur', () => { if (pendingEdit) void saveDraft().catch((error) => { element('notice').textContent = String(error); }); });
element('record').addEventListener('click', () => { void action('record'); });
element('stop').addEventListener('click', () => { void action('stop'); });
element('send').addEventListener('click', () => { void action('send'); });
if (invoke) {
  void invoke('meeting_theme').then((theme) => { document.documentElement.dataset.theme = theme; });
  void refresh();
  setInterval(() => { void refresh(); }, 300);
}
else element('notice').textContent = 'Open this window inside Phorminx.';
