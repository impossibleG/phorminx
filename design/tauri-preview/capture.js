// Small always-on-top capture window opened from the launcher's investigations
// panel: Note, Link or Image into the active investigation, or a name for a
// new one. Adding closes the window; Esc closes it without adding.
(() => {
  const invoke = window.__TAURI__?.core?.invoke;
  const $ = (id) => document.getElementById(id);
  const SNIP_TIMEOUT_MS = 90_000;
  const state = {request: -1, tab: 'note', investigationId: null, image: null, source: null, snip: null, busy: false};
  const submitLabels = {note: 'Add note', link: 'Add link', image: 'Add image', new: 'Start capturing'};

  const status = (text) => { $('capture-status').textContent = text || ''; };

  function setTab(tab) {
    state.tab = tab;
    $('capture-tabs').hidden = tab === 'new';
    for (const button of document.querySelectorAll('[data-tab]')) button.setAttribute('aria-selected', String(button.dataset.tab === tab));
    for (const panel of document.querySelectorAll('[data-panel]')) panel.hidden = panel.dataset.panel !== tab;
    $('capture-submit').textContent = submitLabels[tab];
    status('');
    requestAnimationFrame(() => ({new: $('capture-name'), note: $('capture-note'), link: $('capture-url'), image: $('capture-drop')})[tab]?.focus());
  }

  function render(view) {
    state.investigationId = view.investigationId;
    if (view.tab === 'new') {
      $('capture-kicker').textContent = 'NEW INVESTIGATION';
      $('capture-title').textContent = 'Name it, then capture into it';
    } else {
      $('capture-kicker').textContent = 'ADD TO';
      $('capture-title').textContent = view.title || 'Investigation';
    }
    setTab(view.tab || 'note');
  }

  async function poll() {
    try {
      const view = await invoke('investigation_capture_window');
      if (view.request !== state.request) { state.request = view.request; render(view); }
    } catch (error) { status(String(error)); }
  }

  // ---------- image ----------

  function showImage(data, source) {
    state.image = data;
    state.source = source;
    $('capture-preview').src = data;
    $('capture-preview').hidden = false;
    $('capture-drop-empty').hidden = true;
    status(source === 'screenshot' ? 'Screenshot ready.' : '');
  }

  const readFile = (file) => new Promise((resolve, reject) => {
    if (file.size > 32 * 1024 * 1024) { reject(new Error('Images up to 32 MB can be added.')); return; }
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result);
    reader.onerror = () => reject(new Error('The image could not be read.'));
    reader.readAsDataURL(file);
  });

  async function takeFile(file, source) {
    if (!file?.type.startsWith('image/')) { status('That is not an image.'); return; }
    try { showImage(await readFile(file), source); } catch (error) { status(error.message); }
  }

  document.addEventListener('paste', (event) => {
    const file = [...(event.clipboardData?.items || [])].find((item) => item.kind === 'file' && item.type.startsWith('image/'))?.getAsFile();
    if (!file) return;
    event.preventDefault();
    if (state.tab !== 'image' && state.tab !== 'new') setTab('image');
    void takeFile(file, 'paste');
  });
  const drop = $('capture-drop');
  drop.addEventListener('dragover', (event) => { event.preventDefault(); drop.classList.add('over'); });
  drop.addEventListener('dragleave', () => drop.classList.remove('over'));
  drop.addEventListener('drop', (event) => {
    event.preventDefault();
    drop.classList.remove('over');
    void takeFile(event.dataTransfer?.files[0], 'drop');
  });
  $('capture-choose').addEventListener('click', () => $('capture-file').click());
  $('capture-file').addEventListener('change', (event) => { void takeFile(event.target.files[0], 'file'); event.target.value = ''; });

  function endSnip(message) {
    if (!state.snip) return;
    clearInterval(state.snip.timer);
    state.snip = null;
    $('capture-snip').textContent = 'Take a screenshot';
    void invoke('investigation_snip', {kind: 'cancel', since: null}).catch(() => {});
    if (message !== undefined) status(message);
  }

  async function startSnip() {
    if (state.snip) { endSnip(''); return; }
    try {
      const view = await invoke('investigation_snip', {kind: 'start', since: null});
      const started = Date.now();
      state.snip = {since: view.sequence, timer: null};
      $('capture-snip').textContent = 'Cancel screenshot';
      status('Select an area of the screen.');
      state.snip.timer = setInterval(async () => {
        if (!state.snip) return;
        if (Date.now() - started > SNIP_TIMEOUT_MS) { endSnip('No screenshot arrived. Try again.'); return; }
        try {
          const result = await invoke('investigation_snip', {kind: 'poll', since: state.snip.since});
          if (result.image && state.snip) { endSnip(); showImage(result.image, 'screenshot'); }
        } catch (error) { endSnip(String(error)); }
      }, 400);
    } catch (error) { status(String(error)); }
  }
  $('capture-snip').addEventListener('click', () => void startSnip());

  // ---------- submit ----------

  function fields() {
    if (state.tab === 'new') return {text: $('capture-name').value};
    if (state.tab === 'note') return {text: $('capture-note').value};
    if (state.tab === 'link') return {url: $('capture-url').value, text: $('capture-link-note').value};
    return {data: state.image};
  }

  async function submit() {
    if (state.busy) return;
    const values = fields();
    const problem = invCaptureProblem(state.tab, values);
    if (problem) { status(problem); return; }
    state.busy = true;
    $('capture-submit').disabled = true;
    try {
      await invoke('investigation_capture_submit', {request: {
        kind: state.tab,
        investigationId: state.investigationId,
        text: values.text ?? null,
        url: values.url ?? null,
        data: values.data ?? null,
        source: state.tab === 'image' ? state.source : null,
      }});
    } catch (error) {
      status(String(error));
    } finally {
      state.busy = false;
      $('capture-submit').disabled = false;
    }
  }

  $('capture-form').addEventListener('submit', (event) => { event.preventDefault(); void submit(); });
  for (const button of document.querySelectorAll('[data-tab]')) button.addEventListener('click', () => setTab(button.dataset.tab));
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      event.preventDefault();
      if (state.snip) { endSnip(''); return; }
      void invoke('investigation_capture_close').catch(() => {});
      return;
    }
    if (event.key !== 'Enter' || event.isComposing) return;
    const inTextarea = event.target instanceof HTMLTextAreaElement;
    if (inTextarea && !(event.ctrlKey || event.metaKey)) return;
    event.preventDefault();
    void submit();
  });

  if (invoke) {
    void invoke('meeting_theme').then((theme) => { document.documentElement.dataset.theme = theme; }).catch(() => {});
    void poll();
    setInterval(() => { void poll(); }, 400);
  } else {
    status('Open this window inside Phorminx.');
  }
})();
