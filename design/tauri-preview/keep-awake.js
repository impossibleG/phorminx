// Keep awake: one switch that stops Windows from sleeping while it is on.
// The host holds the request; this page shows the state, a live timer and a
// short line each time it is turned on or off. Totals stay in this browser
// profile only and are a convenience, so storage failures are ignored.
(() => {
  const tauriInvoke = window.__TAURI__?.core?.invoke;
  const node = (id) => document.getElementById(id);
  const totalsKey = 'phorminx.keepAwake.totals';
  const onLines = [
    'Your PC will stay awake. Let the AI keep working.',
    'Staying awake. Long jobs can run without interruption.',
    'Keeping the lights on while you are away.',
  ];
  let view = {active: false, keepScreenOn: true, startedAtMs: null, elapsedMs: 0};
  let receivedAt = Date.now();
  let busy = false;
  let timer = null;

  function formatDuration(ms) {
    const totalSeconds = Math.max(0, Math.floor(ms / 1000));
    const hours = Math.floor(totalSeconds / 3600);
    const minutes = Math.floor((totalSeconds % 3600) / 60);
    const seconds = totalSeconds % 60;
    if (hours) return `${hours} h ${minutes} min`;
    if (minutes) return `${minutes} min ${seconds} s`;
    return `${seconds} s`;
  }

  function readTotals() {
    try {
      const totals = JSON.parse(localStorage.getItem(totalsKey) || '{}');
      return {ms: Number(totals.ms) || 0, sessions: Number(totals.sessions) || 0, longestMs: Number(totals.longestMs) || 0};
    } catch { return {ms: 0, sessions: 0, longestMs: 0}; }
  }

  function addToTotals(ms) {
    const totals = readTotals();
    totals.ms += ms;
    totals.sessions += 1;
    totals.longestMs = Math.max(totals.longestMs, ms);
    try { localStorage.setItem(totalsKey, JSON.stringify(totals)); } catch { /* totals are optional */ }
  }

  function elapsed() {
    return view.active ? view.elapsedMs + (Date.now() - receivedAt) : 0;
  }

  function renderTimer() {
    node('keep-awake-timer').textContent = view.active ? `Awake for ${formatDuration(elapsed())}` : '';
  }

  function renderTotals() {
    const totals = readTotals();
    node('keep-awake-totals').textContent = totals.sessions
      ? `So far: ${formatDuration(totals.ms)} kept awake over ${totals.sessions} ${totals.sessions === 1 ? 'time' : 'times'}. Longest: ${formatDuration(totals.longestMs)}.`
      : '';
  }

  function render() {
    const toggle = node('keep-awake-toggle');
    toggle.setAttribute('aria-pressed', String(view.active));
    toggle.textContent = view.active ? 'Turn off' : 'Turn on';
    toggle.disabled = busy || !tauriInvoke;
    node('keep-awake-panel').dataset.active = String(view.active);
    node('keep-awake-state').textContent = view.active ? 'Keeping your PC awake' : 'Off';
    node('keep-awake-screen').checked = view.active ? view.keepScreenOn : node('keep-awake-screen').checked;
    node('keep-awake-badge').hidden = !view.active;
    renderTimer();
    renderTotals();
    clearInterval(timer);
    timer = view.active ? setInterval(renderTimer, 1000) : null;
  }

  function accept(next) {
    view = next;
    receivedAt = Date.now();
    render();
  }

  async function refresh() {
    if (!tauriInvoke) { node('keep-awake-message').textContent = 'Keep awake works in the Phorminx app.'; render(); return; }
    try { accept(await tauriInvoke('keep_awake_snapshot')); }
    catch (error) { node('keep-awake-message').textContent = String(error); }
  }

  async function set(active) {
    if (busy || !tauriInvoke) return;
    busy = true;
    render();
    try {
      const next = await tauriInvoke('keep_awake_set', {active, keepScreenOn: node('keep-awake-screen').checked});
      const wasActive = view.active;
      busy = false;
      accept(next);
      if (active && !wasActive) {
        node('keep-awake-message').textContent = onLines[Math.floor(Math.random() * onLines.length)];
      } else if (!active && next.endedAfterMs != null) {
        addToTotals(next.endedAfterMs);
        renderTotals();
        node('keep-awake-message').textContent = `Done. Your PC stayed awake for ${formatDuration(next.endedAfterMs)}. Normal sleep is back.`;
      }
    } catch (error) {
      busy = false;
      node('keep-awake-message').textContent = String(error);
      await refresh();
    }
  }

  node('keep-awake-toggle').addEventListener('click', () => void set(!view.active));
  node('keep-awake-screen').addEventListener('change', () => { if (view.active) void set(true); });
  document.addEventListener('phorminx:route', (event) => { if (event.detail === 'keep-awake') void refresh(); });
  void refresh();
})();
