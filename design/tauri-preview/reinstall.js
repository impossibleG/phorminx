// "Reinstall Phorminx": the confirmation, the progress view and, after a
// reinstall, the banner that restores the previous settings. Shared by the
// main window and the minimal Repair Phorminx window (reinstall.html).
(() => {
  const invoke = window.__TAURI__?.core?.invoke;
  const standalone = document.body.dataset.shell === 'reinstall';

  const RESET = [
    'Your settings: shortcut, microphone, formatting, meeting and assistant settings (a backup is kept so you can restore them)',
    'Setup files, setup downloads and the speech runtime (set up again automatically)',
    'Window positions and the app’s internal web cache',
  ];
  const KEEP = [
    'Dictation history, meetings, messages, contacts, investigations and prompts',
    'Your messaging identity',
    'Downloaded speech models and Ollama models',
    'Recordings that were not saved yet',
    'Update preferences and earlier backups',
  ];

  function el(tag, className, text) {
    const element = document.createElement(tag);
    if (className) element.className = className;
    if (text !== undefined) element.textContent = text;
    return element;
  }

  function list(items) {
    const ul = el('ul', 'reinstall-list');
    for (const item of items) ul.append(el('li', '', item));
    return ul;
  }

  function formatBytes(bytes) {
    if (!bytes) return '';
    return bytes >= 1e6 ? `${(bytes / 1e6).toFixed(0)} MB` : `${Math.ceil(bytes / 1e3)} KB`;
  }

  let dialog = null;
  let pollTimer = null;
  let finishing = false;
  const parts = {};

  function build() {
    if (dialog) return dialog;
    dialog = el('dialog', 'reinstall-dialog');
    dialog.setAttribute('aria-labelledby', 'reinstall-title');
    const confirm = el('div', 'reinstall-step');
    confirm.append(
      el('small', '', 'FIX EVERYTHING'),
      Object.assign(el('h2', '', 'Reinstall Phorminx'), { id: 'reinstall-title' }),
      el('p', '', 'Use this when something is broken and nothing else helped. Phorminx downloads a fresh, verified copy of itself, closes, reinstalls, and opens again to set itself up.'),
      el('h3', '', 'Reset to defaults'),
      list(RESET),
      el('h3', '', 'Kept'),
      list(KEEP),
      el('p', 'reinstall-note', 'Needs an internet connection. It takes a few minutes, and setup afterwards may download a few hundred MB. Nothing is deleted: what is reset is moved to a backup folder.'),
    );
    parts.blocked = el('p', 'reinstall-error');
    parts.blocked.setAttribute('role', 'alert');
    parts.qa = el('p', 'reinstall-note', 'QA build: this installs the local test installer instead of downloading the release.');
    parts.qa.hidden = true;
    const confirmActions = el('div', 'dialog-actions');
    parts.cancel = el('button', '', 'Cancel');
    parts.cancel.type = 'button';
    parts.confirm = el('button', 'primary', 'Reinstall Phorminx');
    parts.confirm.type = 'button';
    confirmActions.append(parts.cancel, parts.confirm);
    confirm.append(parts.qa, parts.blocked, confirmActions);

    const progress = el('div', 'reinstall-step');
    parts.progressTitle = el('h2', '', 'Reinstalling Phorminx');
    parts.message = el('p', '', '');
    parts.message.setAttribute('role', 'status');
    parts.bar = el('div', 'reinstall-bar');
    parts.fill = el('span');
    parts.bar.append(parts.fill);
    parts.amount = el('p', 'reinstall-note', '');
    parts.error = el('p', 'reinstall-error');
    parts.error.setAttribute('role', 'alert');
    const progressActions = el('div', 'dialog-actions');
    parts.close = el('button', '', 'Close');
    parts.close.type = 'button';
    parts.retry = el('button', 'primary', 'Try again');
    parts.retry.type = 'button';
    progressActions.append(parts.close, parts.retry);
    progress.append(el('small', '', 'FIX EVERYTHING'), parts.progressTitle, parts.message, parts.bar, parts.amount, parts.error, progressActions);

    parts.confirmStep = confirm;
    parts.progressStep = progress;
    dialog.append(confirm, progress);
    document.body.append(dialog);

    parts.cancel.addEventListener('click', close);
    parts.close.addEventListener('click', close);
    parts.confirm.addEventListener('click', () => void begin());
    parts.retry.addEventListener('click', () => void begin());
    dialog.addEventListener('cancel', (event) => {
      // Escape never interrupts a running reinstall, and never leaves the
      // Repair window empty.
      event.preventDefault();
      if (parts.progressStep.hidden || !parts.close.hidden) close();
    });
    return dialog;
  }

  function close() {
    clearInterval(pollTimer);
    pollTimer = null;
    if (standalone) { void invoke?.('reinstall_close'); return; }
    dialog?.close();
  }

  function showStep(step) {
    parts.confirmStep.hidden = step !== 'confirm';
    parts.progressStep.hidden = step !== 'progress';
  }

  async function open() {
    build();
    showStep('confirm');
    parts.blocked.textContent = '';
    parts.confirm.disabled = !invoke;
    if (!dialog.open) dialog.showModal();
    if (!invoke) { parts.blocked.textContent = 'Reinstalling is only available in the Phorminx app.'; return; }
    try {
      const view = await invoke('reinstall_snapshot');
      parts.qa.hidden = !view.flow.qa;
      if (['checking', 'downloading', 'ready', 'handing-off'].includes(view.flow.state)) {
        showStep('progress');
        render(view.flow);
        startPolling();
        return;
      }
      if (view.blocked) {
        parts.blocked.textContent = view.blocked;
        parts.confirm.disabled = true;
      }
    } catch (error) {
      parts.blocked.textContent = String(error);
    }
  }

  async function begin() {
    showStep('progress');
    parts.error.textContent = '';
    try {
      render(await invoke('reinstall_start'));
      startPolling();
    } catch (error) {
      render({ state: 'failed', message: String(error), received: 0, total: 0 });
    }
  }

  function startPolling() {
    clearInterval(pollTimer);
    pollTimer = setInterval(() => void poll(), 500);
  }

  async function poll() {
    try {
      const view = await invoke('reinstall_snapshot');
      render(view.flow);
      if (view.flow.state === 'ready' && !finishing) await finish();
      if (['failed', 'idle'].includes(view.flow.state)) { clearInterval(pollTimer); pollTimer = null; }
    } catch (error) {
      render({ state: 'failed', message: String(error), received: 0, total: 0 });
      clearInterval(pollTimer);
      pollTimer = null;
    }
  }

  async function finish() {
    finishing = true;
    try {
      render(await invoke('reinstall_finish'));
    } catch (error) {
      finishing = false;
      clearInterval(pollTimer);
      pollTimer = null;
      render({ state: 'failed', message: String(error), received: 0, total: 0 });
    }
  }

  function render(flow) {
    const failed = flow.state === 'failed';
    const busy = !failed && flow.state !== 'idle';
    parts.progressTitle.textContent = failed ? 'Phorminx was not reinstalled' : 'Reinstalling Phorminx';
    parts.message.textContent = failed ? '' : flow.message;
    parts.error.textContent = failed ? flow.message : '';
    const percent = flow.total ? Math.min(100, Math.round((flow.received / flow.total) * 100)) : 0;
    parts.bar.hidden = failed || !['downloading', 'ready', 'handing-off'].includes(flow.state);
    parts.fill.style.width = `${flow.state === 'downloading' ? percent : 100}%`;
    parts.amount.textContent = flow.state === 'downloading' && flow.total
      ? `${formatBytes(flow.received)} of ${formatBytes(flow.total)}`
      : flow.state === 'handing-off' ? 'You can leave this window. Phorminx closes now and reopens by itself; setup then runs automatically.' : '';
    parts.close.hidden = busy;
    parts.retry.hidden = !failed;
  }

  // The banner on Setup & repair after a reinstall.
  const banner = document.getElementById('reinstall-banner');
  let restoreTimer = null;

  function date(seconds) {
    try { return new Date(seconds * 1000).toLocaleString(); } catch (_) { return ''; }
  }

  function renderBanner(view) {
    if (!banner) return;
    banner.replaceChildren();
    const latest = view.backups[0];
    const restoring = ['restoring', 'models', 'restarting'].includes(view.restore.state);
    const show = latest && !latest.dismissed && (restoring || view.restore.state === 'failed'
      || latest.status !== 'installed' || (latest.hasSettings && !latest.restored));
    banner.hidden = !show;
    if (!show) return;
    banner.append(el('small', '', 'AFTER REINSTALL'));
    if (latest.status !== 'installed') {
      banner.append(el('h2', '', 'The reinstall did not finish'), el('p', '', latest.message));
    } else {
      banner.append(el('h2', '', 'Settings were reset to defaults'),
        el('p', '', 'Phorminx was reinstalled and is setting itself up again. Your history, meetings, messages and models were kept. If you want your own settings back, restore them here.'));
    }
    const status = el('p', 'reinstall-note');
    status.setAttribute('role', 'status');
    if (view.restore.state && view.restore.state !== 'idle') {
      status.textContent = view.restore.message + (view.restore.percent != null && view.restore.state === 'models' ? ` ${view.restore.percent}%` : '');
    }
    const restorable = view.backups.filter((backup) => backup.hasSettings);
    const actions = el('div', 'bootstrap-control');
    if (latest.status === 'installed' && latest.hasSettings && !latest.restored) {
      const restore = el('button', 'primary', 'Restore my previous settings');
      restore.type = 'button';
      restore.disabled = restoring;
      restore.addEventListener('click', () => void restoreBackup(latest.id, status));
      actions.append(restore);
    }
    const dismiss = el('button', '', latest.status === 'installed' ? 'Keep the defaults' : 'Dismiss');
    dismiss.type = 'button';
    dismiss.disabled = restoring;
    dismiss.addEventListener('click', async () => {
      try { await invoke('reinstall_dismiss', { id: latest.id }); banner.hidden = true; }
      catch (error) { status.textContent = String(error); }
    });
    actions.append(dismiss);
    banner.append(actions, status);
    if (restorable.length > 1 || (restorable.length === 1 && restorable[0].id !== latest.id)) {
      const details = el('details', 'reinstall-backups');
      details.append(el('summary', '', 'Other saved settings'));
      for (const backup of restorable) {
        const row = el('div', 'reinstall-backup-row');
        row.append(el('span', '', `${date(backup.createdAt)} · version ${backup.fromVersion}${backup.restored ? ' · restored' : ''}`));
        const button = el('button', '', 'Restore these');
        button.type = 'button';
        button.disabled = restoring;
        button.addEventListener('click', () => void restoreBackup(backup.id, status));
        row.append(button);
        details.append(row);
      }
      banner.append(details);
    }
  }

  async function restoreBackup(id, status) {
    status.textContent = 'Restoring your previous settings…';
    try {
      await invoke('reinstall_restore', { id });
      clearInterval(restoreTimer);
      restoreTimer = setInterval(() => void refreshBanner(), 700);
    } catch (error) {
      status.textContent = String(error);
    }
  }

  async function refreshBanner() {
    if (!banner || !invoke) return;
    try {
      const view = await invoke('reinstall_snapshot');
      renderBanner(view);
      if (!['restoring', 'models', 'restarting'].includes(view.restore.state)) { clearInterval(restoreTimer); restoreTimer = null; }
    } catch (_) { /* the page keeps working without the banner */ }
  }

  async function afterReinstall() {
    if (!invoke || standalone) return;
    await refreshBanner();
    let start = false;
    try { start = await invoke('reinstall_take_auto_setup'); } catch (_) { return; }
    if (!start) return;
    document.querySelector('.side [data-route="setup"]')?.click();
    try { await invoke('host_start_bootstrap'); } catch (_) { /* the Setup page shows the state */ }
    // product.js owns the Setup page; its renderer is a page-level function.
    if (typeof refreshSetup === 'function') void refreshSetup();
  }

  document.getElementById('reinstall-open')?.addEventListener('click', () => void open());
  document.querySelector('.side [data-route="setup"]')?.addEventListener('click', () => void refreshBanner());
  window.PhorminxReinstall = { open, refreshBanner };
  if (standalone) void open();
  else void afterReinstall();
})();
