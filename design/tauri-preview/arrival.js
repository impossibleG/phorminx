// The quiet "message arrived" panel: sender name and, if allowed, a short
// snippet. It never takes focus; clicking it opens the conversation.
(() => {
  const invoke = window.__TAURI__?.core?.invoke;
  const $ = (id) => document.getElementById(id);
  if (!invoke) return;

  void invoke('meeting_theme').then((theme) => { document.documentElement.dataset.theme = theme; }).catch(() => {});

  async function poll() {
    try {
      const view = await invoke('host_messaging_arrival');
      if (!view) return;
      $('arrival-title').textContent = view.title;
      $('arrival-detail').textContent = view.detail;
    } catch (_) { /* the host closes this window when it is done */ }
  }

  $('arrival').addEventListener('click', () => { void invoke('host_messaging_arrival_open'); });
  void poll();
  setInterval(() => { void poll(); }, 400);
})();
