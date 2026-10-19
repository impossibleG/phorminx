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
// A public comment is shorter than a report.
function limit() { return element('comment').checked ? 2000 : 12000; }
function updateCount() { element('count').textContent = `${countBytes(element('report').value).toLocaleString()} / ${limit().toLocaleString()} bytes`; }
function updateMode() {
  const comment = element('comment').checked;
  element('comment-options').hidden = !comment;
  element('diagnostics-row').hidden = comment;
  element('send').textContent = comment ? 'Share comment' : 'Send report';
  updateCount();
}

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
        comment: element('comment').checked,
        consent: element('consent').checked,
        displayName: element('display-name').value,
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
    const busy = view.recording || view.sending || view.reformatting;
    element('report').disabled = busy;
    for (const id of ['diagnostics', 'comment', 'consent', 'display-name', 'strength', 'instruction']) element(id).disabled = busy;
    element('reformat').disabled = busy || !element('report').value.trim();
    element('reformat').textContent = view.reformatting ? 'Reformatting…' : 'Reformat';
    if (!pendingEdit) {
      element('diagnostics').checked = view.diagnostics;
      element('comment').checked = view.comment;
      element('consent').checked = view.consent;
      if (document.activeElement !== element('display-name')) element('display-name').value = view.displayName;
      updateMode();
    }
    element('record').hidden = view.recording;
    element('record').disabled = view.sending;
    element('stop').hidden = !view.recording;
    element('stop').disabled = view.stopping;
    element('send').disabled = busy || !element('report').value.trim()
      || countBytes(element('report').value) > limit()
      || (element('comment').checked && !element('consent').checked);
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
element('comment').addEventListener('change', () => { updateMode(); draftChanged(); });
element('consent').addEventListener('change', draftChanged);
element('display-name').addEventListener('input', draftChanged);
element('reformat').addEventListener('click', async () => {
  if (!invoke) return;
  try {
    await saveDraft();
    await invoke('plugin:phorminx-host|feedback_reformat', {strength: element('strength').value, instruction: element('instruction').value.trim() || null});
    await refresh();
  } catch (error) { element('notice').textContent = String(error); }
});
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
