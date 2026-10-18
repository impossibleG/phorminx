const tauriInvoke = window.__TAURI__?.core?.invoke;
const invoke = tauriInvoke && ((command, args) => tauriInvoke(command, args));
const element = (id) => document.getElementById(id);
let revision = null;
let busy = false;
let polling = false;

function render(view) {
  // A new dictation, or a finished reformat, replaces the text. Edits made
  // while nothing new arrived are never overwritten by polling.
  if (view.revision !== revision) {
    revision = view.revision;
    element('text').value = view.text;
    element('strength').value = view.strength;
    element('replace').hidden = !view.canReplace;
  }
  busy = view.busy;
  element('text').disabled = busy;
  element('reformat').disabled = busy || !element('text').value.trim();
  element('reformat').textContent = busy ? 'Reformatting…' : 'Reformat';
  element('copy').disabled = busy || !element('text').value.trim();
  element('replace').disabled = busy || !element('text').value.trim();
  element('formatting-note').textContent = view.formattingNote || '';
  element('formatting-note').hidden = !view.formattingNote;
  if (view.notice) element('notice').textContent = view.notice;
}

async function refresh() {
  if (!invoke || polling) return;
  polling = true;
  try {
    render(await invoke('review_snapshot'));
  } catch (error) { element('notice').textContent = String(error); }
  finally { polling = false; }
}

async function run(command, args, pending) {
  if (!invoke) return;
  element('notice').textContent = pending;
  try {
    const message = await invoke(command, args);
    if (message) element('notice').textContent = message;
    await refresh();
  } catch (error) { element('notice').textContent = String(error); }
}

const copy = () => run('review_copy', {text: element('text').value}, 'Copying…');
element('copy').addEventListener('click', () => { void copy(); });
element('replace').addEventListener('click', () => {
  void run('review_replace', {text: element('text').value}, 'Replacing…');
});
element('reformat').addEventListener('click', () => {
  void run('review_reformat', {
    text: element('text').value,
    strength: element('strength').value,
    instruction: element('instruction').value.trim() || null,
  }, 'Reformatting with the local model…');
});
element('close').addEventListener('click', () => { void invoke?.('review_close'); });
element('text').addEventListener('input', () => {
  element('copy').disabled = busy || !element('text').value.trim();
  element('replace').disabled = element('copy').disabled;
  element('reformat').disabled = element('copy').disabled;
});
document.addEventListener('keydown', (event) => {
  // Copy is the default action; Replace never runs from the keyboard alone.
  if (event.key === 'Enter' && (event.ctrlKey || event.metaKey) && !element('copy').disabled) {
    event.preventDefault();
    void copy();
  } else if (event.key === 'Escape') {
    void invoke?.('review_close');
  }
});
if (invoke) {
  void invoke('meeting_theme').then((theme) => { document.documentElement.dataset.theme = theme; });
  void refresh().then(() => element('text').focus());
  setInterval(() => { void refresh(); }, 300);
} else element('notice').textContent = 'Open this window inside Phorminx.';
