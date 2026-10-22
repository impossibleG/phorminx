// Review window for a dictated message: Enter stops the recording, then Enter
// sends the (editable) text; Esc discards at any point.
(() => {
  const invoke = window.__TAURI__?.core?.invoke;
  const $ = (id) => document.getElementById(id);
  let draft = null;
  let edited = false;

  const labels = {
    starting: ['MESSAGE · STARTING', 'Starting the microphone…', '', 'Stop'],
    recording: ['MESSAGE · LISTENING', 'Speak your message.', 'Enter stops · Esc discards', 'Stop'],
    review: ['MESSAGE · REVIEW', '', 'Enter sends · Shift+Enter new line · Esc discards', 'Send'],
    empty: ['MESSAGE', 'No speech was captured. Nothing was sent.', 'Esc closes', 'Close'],
    failed: ['MESSAGE', '', 'Esc closes', 'Close'],
  };

  function render() {
    if (!draft) return;
    const [phase, status, hint, primary] = labels[draft.phase] || labels.failed;
    $('review-phase').textContent = phase;
    $('review-to').textContent = draft.contactName ? `To ${draft.contactName}` : 'Message';
    $('review-status').textContent = draft.phase === 'failed' ? draft.message : status;
    $('review-hint').textContent = hint;
    $('review-primary').textContent = primary;
    const reviewing = draft.phase === 'review';
    $('review-text').hidden = !reviewing;
    if (reviewing && !edited) {
      $('review-text').value = draft.text;
      $('review-text').focus();
      $('review-text').setSelectionRange(draft.text.length, draft.text.length);
    }
  }

  async function poll() {
    if (!invoke) return;
    try {
      const next = await invoke('host_messaging_draft');
      if (next.phase !== draft?.phase) edited = false;
      draft = next;
      render();
    } catch (error) { $('review-status').textContent = String(error); }
  }

  async function action(kind) {
    try {
      await invoke('host_messaging_draft_action', {kind, textBody: kind === 'send' ? $('review-text').value : null});
      if (kind === 'stop') { $('review-status').textContent = 'Transcribing…'; }
    } catch (error) { $('review-status').textContent = String(error); }
  }

  async function primary() {
    const phase = draft?.phase;
    if (phase === 'starting' || phase === 'recording') return action('stop');
    if (phase === 'review') {
      if (!$('review-text').value.trim()) { $('review-status').textContent = 'The message is empty.'; return; }
      return action('send');
    }
    return action('discard');
  }

  $('review-text').addEventListener('input', () => { edited = true; });
  $('review-primary').addEventListener('click', () => { void primary(); });
  $('review-discard').addEventListener('click', () => { void action('discard'); });
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') { event.preventDefault(); void action('discard'); return; }
    if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) { event.preventDefault(); void primary(); }
  });

  if (invoke) {
    void invoke('meeting_theme').then((theme) => { document.documentElement.dataset.theme = theme; }).catch(() => {});
    void poll();
    setInterval(() => { void poll(); }, 300);
  }
  else $('review-status').textContent = 'Open this window inside Phorminx.';
})();
