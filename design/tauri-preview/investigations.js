// Investigations tab: chronology, diagram, assistant, capture and OCR models.
// The host owns all data; this file renders snapshots and sends intents.
(() => {
  const call = window.__TAURI__?.core?.invoke;
  const $ = (id) => document.getElementById(id);
  const el = (tag, className, text) => {
    const element = document.createElement(tag);
    if (className) element.className = className;
    if (text !== undefined) element.textContent = text;
    return element;
  };
  const SVG = 'http://www.w3.org/2000/svg';
  const svgEl = (tag, attributes = {}) => {
    const element = document.createElementNS(SVG, tag);
    for (const [key, value] of Object.entries(attributes)) element.setAttribute(key, value);
    return element;
  };
  const STORAGE_KEY = 'phorminx.investigations.open';

  const state = {
    route: 'home',
    overview: null,
    detail: null,
    id: null,
    view: 'chronology',
    filter: 'all',
    query: '',
    matches: null,
    images: new Map(),
    assistantOpen: true,
    diagram: {view: {x: 60, y: 60, k: 1}, viewLoadedFor: null, selected: null, drag: null, menuNode: null, pendingParent: null},
    refreshing: false,
    lastVoiceRevision: -1,
    lastVoiceNotice: '',
    searchTimer: null,
    layoutTimer: null,
  };

  const visible = () => state.route === 'investigations';
  const status = (text) => { $('inv-status').textContent = text || ''; };
  const fail = (error) => status(String(error?.message || error));

  async function invoke(command, args) {
    if (!call) throw new Error('Open this page inside Phorminx.');
    return call(command, args);
  }

  function rememberOpen(id) {
    try { localStorage.setItem(STORAGE_KEY, String(id)); } catch (_) { /* optional */ }
  }
  function recalledOpen() {
    try { return Number(localStorage.getItem(STORAGE_KEY)) || null; } catch (_) { return null; }
  }

  function timeLabel(ms) {
    return new Intl.DateTimeFormat(undefined, {hour: '2-digit', minute: '2-digit'}).format(new Date(ms));
  }
  function fullTimeLabel(ms) {
    return new Intl.DateTimeFormat(undefined, {dateStyle: 'medium', timeStyle: 'short'}).format(new Date(ms));
  }

  function imageUrl(sha) {
    if (!sha || !state.id) return Promise.resolve(null);
    const key = `${state.id}:${sha}`;
    if (!state.images.has(key)) {
      state.images.set(key, invoke('investigation_image', {id: state.id, sha}).catch(() => { state.images.delete(key); return null; }));
    }
    return state.images.get(key);
  }
  function setImage(img, sha) {
    img.decoding = 'async';
    void imageUrl(sha).then((url) => { if (url) img.src = url; else img.classList.add('missing'); });
  }

  // ---------- overview ----------

  async function refresh(force = false) {
    if (!call || state.refreshing) return;
    state.refreshing = true;
    try {
      const overview = await invoke('investigation_overview');
      state.overview = overview;
      renderOcrCards(overview);
      renderVoice(overview.voice);
      renderAssistantState(overview.assistant);
      if (!visible()) return;
      renderOverview(overview);
      if (state.id !== null) {
        const detail = await invoke('investigation_detail', {id: state.id, since: force || !state.detail ? null : state.detail.revision});
        if (detail) renderDetail(detail);
      }
    } catch (error) {
      if (visible()) fail(error);
    } finally {
      state.refreshing = false;
    }
  }

  function chooseInvestigation(overview) {
    const ids = overview.investigations.map((item) => item.id);
    if (state.id !== null && ids.includes(state.id)) return state.id;
    const recalled = recalledOpen();
    if (recalled && ids.includes(recalled)) return recalled;
    if (overview.activeId && ids.includes(overview.activeId)) return overview.activeId;
    return ids[0] ?? null;
  }

  function renderOverview(overview) {
    const chosen = chooseInvestigation(overview);
    if (chosen !== state.id) openInvestigation(chosen, false);
    const empty = !overview.investigations.length;
    $('inv-empty').hidden = !empty;
    $('inv-body').hidden = empty;
    for (const id of ['inv-picker', 'inv-active', 'inv-more']) $(id).hidden = empty;
    document.querySelector('#investigations .inv-tabs').hidden = empty;
    if (empty) $('inv-title').textContent = 'Investigations';
    const picker = $('inv-picker');
    const options = overview.investigations.map((item) => {
      const option = el('option', '', `${item.title}${item.id === overview.activeId ? '  ★' : ''} · ${item.itemCount}`);
      option.value = String(item.id);
      return option;
    });
    if (document.activeElement !== picker) {
      picker.replaceChildren(...options);
      if (state.id !== null) picker.value = String(state.id);
    }
    const active = state.id !== null && state.id === overview.activeId;
    $('inv-active').setAttribute('aria-pressed', String(active));
    $('inv-active').textContent = active ? 'Capturing here' : 'Capture here';
    const capture = overview.capture;
    $('inv-capture-hint').textContent = capture?.registered
      ? `${capture.shortcut} sends the clipboard to ${active ? 'this' : 'the ★'} investigation`
      : capture?.enabled ? 'Quick-capture shortcut unavailable · More → Quick-capture shortcut' : '';
  }

  function openInvestigation(id, refreshNow = true) {
    state.id = id;
    state.detail = null;
    state.matches = null;
    state.query = '';
    $('inv-search').value = '';
    state.diagram.selected = null;
    if (id !== null) rememberOpen(id);
    if (refreshNow) void refresh(true);
  }

  // ---------- OCR models (Models, Setup and Investigations) ----------

  function renderOcrCards(overview) {
    const ocr = overview.ocr;
    for (const card of document.querySelectorAll('[data-ocr-card]')) {
      card.dataset.state = ocr.state;
      card.querySelector('[data-ocr-message]').textContent = ocr.state === 'ready' && overview.pendingOcr
        ? `${ocr.message} Reading ${overview.pendingOcr} screenshot${overview.pendingOcr === 1 ? '' : 's'}…`
        : ocr.message;
      const progress = card.querySelector('.inv-ocr-progress');
      progress.hidden = ocr.state !== 'downloading';
      progress.firstElementChild.style.width = `${Math.round(100 * ocr.downloadedBytes / Math.max(1, ocr.totalBytes))}%`;
      const install = card.querySelector('[data-ocr-install]');
      install.hidden = ocr.state === 'ready';
      install.disabled = ocr.state === 'downloading';
      install.textContent = ocr.state === 'failed' ? 'Retry download' : ocr.state === 'downloading' ? 'Downloading…' : `Download verified models · ${invFormatBytes(ocr.totalBytes)}`;
      card.querySelector('[data-ocr-remove]').hidden = ocr.state !== 'ready';
    }
  }

  // ---------- detail ----------

  function renderDetail(detail) {
    const previous = state.detail;
    state.detail = detail;
    $('inv-title').textContent = detail.title;
    if (!previous || previous.id !== detail.id) {
      state.diagram.viewLoadedFor = null;
    }
    renderTimeline();
    renderTray();
    if (state.view === 'diagram') renderDiagram();
    renderMessages();
    const suggested = detail.edges.some((edge) => edge.suggested);
    $('inv-dismiss-suggestions').hidden = !suggested;
  }

  function itemById(id) {
    return state.detail?.items.find((item) => item.id === id) || null;
  }

  // ---------- chronology ----------

  function badgeFor(item) {
    if (item.kind === 'image') {
      if (item.ocrState === 'pending') {
        const models = state.overview?.ocr.state;
        if (models === 'ready') return ['Reading text…', 'busy'];
        if (models === 'downloading') return ['Downloading text models…', 'busy'];
        if (models === 'failed') return ['Text models need repair · Models', 'warn'];
        return ['Text models not installed · Models', 'muted'];
      }
      if (item.ocrState === 'failed') return ['Text not read', 'warn'];
      if (item.ocrState === 'done' && !item.body.trim()) return ['No text found', 'muted'];
    }
    if (item.kind === 'link') {
      if (item.fetchState === 'pending') return ['Fetching preview…', 'busy'];
      if (item.fetchState === 'failed') return ['Preview unavailable', 'warn'];
    }
    return null;
  }

  function linkPreview(item, compact = false) {
    const box = el('div', `inv-link${item.imageSha && !compact ? ' with-image' : ''}`);
    const text = el('div', 'inv-link-text');
    const site = el('div', 'inv-link-site');
    if (item.faviconSha) {
      const icon = el('img', 'inv-favicon');
      icon.alt = '';
      setImage(icon, item.faviconSha);
      site.append(icon);
    } else {
      site.append(el('span', 'inv-favicon placeholder', (item.linkSite || item.url || '?').replace(/^https?:\/\//, '').charAt(0).toUpperCase()));
    }
    site.append(el('span', '', item.linkSite || hostOf(item.url)));
    text.append(site, el('strong', 'inv-link-title', item.linkTitle || item.url));
    if (item.linkDescription) text.append(el('p', 'inv-link-description', item.linkDescription));
    text.append(el('span', 'inv-link-url', item.url));
    box.append(text);
    if (item.imageSha && !compact) {
      const image = el('img', 'inv-link-image');
      image.alt = '';
      setImage(image, item.imageSha);
      box.append(image);
    }
    if (item.fetchState === 'pending') box.classList.add('loading');
    return box;
  }

  function hostOf(url) {
    try { return new URL(url).host.replace(/^www\./, ''); } catch (_) { return url || ''; }
  }

  function cardContent(item) {
    const content = el('div', 'inv-card-content');
    if (item.title && item.kind !== 'link') content.append(el('strong', 'inv-card-title', item.title));
    if (item.kind === 'link') {
      content.append(linkPreview(item));
      if (item.title) content.prepend(el('strong', 'inv-card-title', item.title));
      if (item.body.trim()) content.append(el('p', 'inv-card-text clamp', item.body));
    } else if (item.kind === 'image') {
      const frame = el('div', 'inv-shot');
      const image = el('img');
      image.alt = item.title || 'Screenshot';
      image.width = item.imageWidth;
      image.height = item.imageHeight;
      setImage(image, item.imageSha);
      frame.append(image);
      content.append(frame);
      if (item.body.trim()) content.append(el('p', 'inv-card-text clamp ocr', item.body));
    } else if (item.kind === 'answer') {
      const answer = el('div', 'inv-card-text markdown clamp-tall');
      answer.append(renderWithCitations(item.body));
      content.append(answer);
    } else {
      content.append(el('p', `inv-card-text clamp${item.kind === 'voice' ? ' quote' : ''}`, item.body));
    }
    return content;
  }

  function renderTimeline() {
    const detail = state.detail;
    const timeline = $('inv-timeline');
    if (!detail) { timeline.replaceChildren(); return; }
    const items = invVisibleItems(detail.items, state.filter, state.matches);
    const fragment = document.createDocumentFragment();
    let day = null;
    for (const item of [...items].reverse()) {
      const label = invDayLabel(item.createdAtMs);
      if (label !== day) {
        day = label;
        fragment.append(el('div', 'inv-day', label));
      }
      const card = el('article', 'inv-card');
      card.dataset.kind = item.kind;
      card.dataset.itemId = String(item.id);
      card.setAttribute('role', 'listitem');
      card.tabIndex = 0;
      const box = el('div', 'inv-card-box');
      const header = el('header');
      header.append(el('span', 'inv-kind', invKindLabel(item.kind)), el('span', 'inv-card-id', `#${item.id}`), el('time', '', timeLabel(item.createdAtMs)));
      if (item.source === 'quick-capture') header.append(el('span', 'inv-source', 'Quick capture'));
      if (item.nodeId) header.append(el('span', 'inv-source placed', 'On diagram'));
      const badge = badgeFor(item);
      if (badge) {
        const chip = el('span', `inv-badge ${badge[1]}`, badge[0]);
        if (badge[1] === 'warn') {
          const retry = el('button', 'inv-inline', 'Retry');
          retry.type = 'button';
          retry.addEventListener('click', (event) => { event.stopPropagation(); void act(() => invoke('investigation_retry', {itemId: item.id})); });
          chip.append(retry);
        }
        header.append(chip);
      }
      box.append(header, cardContent(item));
      card.append(el('span', 'inv-dot', ''), box);
      card.addEventListener('click', (event) => { if (!event.target.closest('a,button')) openDetail(item.id); });
      card.addEventListener('keydown', (event) => { if (event.key === 'Enter') openDetail(item.id); });
      fragment.append(card);
    }
    if (!items.length) {
      const empty = el('div', 'inv-timeline-empty');
      if (detail.items.length) empty.append(el('strong', '', 'No cards match.'), el('p', '', 'Clear the search or choose All.'));
      else empty.append(el('strong', '', 'Nothing collected yet.'), el('p', '', 'Paste a screenshot (Ctrl+V), drop an image or a link, type a note, or record a voice note.'));
      fragment.append(empty);
    }
    const scroll = timeline.scrollTop;
    timeline.replaceChildren(fragment);
    timeline.scrollTop = scroll;
  }

  function flashCard(id) {
    const card = $('inv-timeline').querySelector(`[data-item-id="${id}"]`);
    if (!card) return;
    card.scrollIntoView({block: 'center', behavior: 'smooth'});
    card.classList.remove('flash');
    void card.offsetWidth;
    card.classList.add('flash');
    card.focus({preventScroll: true});
  }

  function jumpToCard(id) {
    if (!itemById(id)) { status(`Card #${id} is not in this investigation.`); return; }
    if (state.filter !== 'all' || state.matches !== null) {
      setFilter('all');
      state.matches = null;
      state.query = '';
      $('inv-search').value = '';
      renderTimeline();
    }
    setView('chronology');
    requestAnimationFrame(() => flashCard(id));
  }

  function setFilter(kind) {
    state.filter = kind;
    for (const chip of document.querySelectorAll('[data-inv-filter]')) {
      const on = chip.dataset.invFilter === kind;
      chip.classList.toggle('active', on);
      chip.setAttribute('aria-pressed', String(on));
    }
  }

  async function runSearch() {
    const query = $('inv-search').value.trim();
    state.query = query;
    if (!query) { state.matches = null; renderTimeline(); return; }
    try {
      const ids = await invoke('investigation_search', {id: state.id, query, kind: null});
      if (query === state.query) { state.matches = new Set(ids); renderTimeline(); }
    } catch (error) { fail(error); }
  }

  // ---------- detail dialog ----------

  let detailItemId = null;
  function openDetail(id) {
    const item = itemById(id);
    if (!item) return;
    detailItemId = id;
    $('inv-detail-kind').textContent = `${invKindLabel(item.kind).toUpperCase()} · #${item.id}`;
    $('inv-detail-title').value = item.title;
    $('inv-detail-body').value = item.body;
    const media = $('inv-detail-media');
    media.replaceChildren();
    $('inv-detail-body-label').textContent = item.kind === 'image' ? 'Extracted text (editable)' : item.kind === 'link' ? 'Your notes about this link' : 'Text';
    if (item.kind === 'image') {
      const image = el('img', 'inv-detail-image');
      image.alt = item.title || 'Screenshot';
      setImage(image, item.imageSha);
      media.append(image);
      const badge = badgeFor(item);
      if (badge) media.append(el('p', `inv-badge ${badge[1]}`, badge[0]));
      if (item.ocrState === 'done' || item.ocrState === 'failed') {
        const retry = el('button', '', item.ocrState === 'done' ? 'Read text again' : 'Retry reading text');
        retry.type = 'button';
        retry.addEventListener('click', () => void act(() => invoke('investigation_retry', {itemId: item.id})));
        media.append(retry);
      }
    } else if (item.kind === 'link') {
      media.append(linkPreview(item));
      const copy = el('button', '', 'Copy link');
      copy.type = 'button';
      copy.addEventListener('click', async () => {
        try { await navigator.clipboard.writeText(item.url); copy.textContent = 'Copied'; } catch (_) { copy.textContent = 'Copy failed'; }
      });
      media.append(copy);
      if (item.fetchState === 'failed') {
        const retry = el('button', '', 'Retry preview');
        retry.type = 'button';
        retry.addEventListener('click', () => void act(() => invoke('investigation_retry', {itemId: item.id})));
        media.append(retry);
      }
    }
    media.hidden = !media.childNodes.length;
    const source = {paste: 'pasted', drop: 'dropped', file: 'added from a file', typed: 'typed', voice: 'dictated (text only)', 'quick-capture': 'quick capture', assistant: 'pinned from the assistant'}[item.source] || item.source;
    $('inv-detail-meta').textContent = `Added ${fullTimeLabel(item.createdAtMs)} · ${source}${item.updatedAtMs > item.createdAtMs + 1000 ? ` · edited ${fullTimeLabel(item.updatedAtMs)}` : ''}`;
    $('inv-detail-place').textContent = item.nodeId ? 'Show on diagram' : 'Place on diagram';
    $('inv-detail').returnValue = '';
    $('inv-detail').showModal();
  }

  // ---------- capture ----------

  async function act(action, success) {
    try {
      const result = await action();
      status(success || '');
      await refresh(true);
      return result;
    } catch (error) {
      fail(error);
      return undefined;
    }
  }

  const readAsDataUrl = (file) => new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result);
    reader.onerror = () => reject(new Error('The image could not be read.'));
    reader.readAsDataURL(file);
  });

  async function addImages(files, source, parentNode = null) {
    const images = [...files].filter((file) => file.type.startsWith('image/'));
    if (!images.length) return false;
    if (state.id === null) { status('Start an investigation first.'); return true; }
    for (const file of images) {
      if (file.size > 32 * 1024 * 1024) { status(`${file.name || 'Image'} is larger than 32 MB.`); continue; }
      status(`Adding ${file.name || 'image'}…`);
      try {
        const data = await readAsDataUrl(file);
        await invoke('investigation_add_image', {id: state.id, data, source, parentNode});
        status(state.overview?.ocr.state === 'ready' ? 'Screenshot added. Reading its text locally…' : 'Screenshot added. Install the text models in Models to extract its text.');
      } catch (error) { fail(error); }
    }
    await refresh(true);
    return true;
  }

  async function addText(text, note, parentNode = null) {
    if (state.id === null) { status('Start an investigation first.'); return null; }
    return act(() => invoke('investigation_add_text', {id: state.id, text, note, parentNode}));
  }

  function isTextField(target) {
    return target instanceof HTMLElement && (target.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName));
  }

  document.addEventListener('paste', (event) => {
    if (!visible() || state.id === null || document.querySelector('dialog[open]')) return;
    const data = event.clipboardData;
    if (!data) return;
    const files = [...data.items].filter((item) => item.kind === 'file' && item.type.startsWith('image/')).map((item) => item.getAsFile()).filter(Boolean);
    if (files.length) {
      event.preventDefault();
      void addImages(files, 'paste');
      return;
    }
    if (isTextField(event.target)) return;
    const text = data.getData('text/plain');
    if (text.trim()) {
      event.preventDefault();
      void addText(text, false).then(() => status('Pasted text added.'));
    }
  });

  const section = $('investigations');
  let dragDepth = 0;
  section.addEventListener('dragenter', (event) => {
    if (!event.dataTransfer?.types.includes('Files') && !event.dataTransfer?.types.includes('text/uri-list')) return;
    dragDepth++;
    section.classList.add('inv-dropping');
  });
  section.addEventListener('dragleave', () => { dragDepth = Math.max(0, dragDepth - 1); if (!dragDepth) section.classList.remove('inv-dropping'); });
  section.addEventListener('dragover', (event) => { if (event.dataTransfer?.types.some((type) => ['Files', 'text/uri-list', 'text/plain', 'application/x-phorminx-item'].includes(type))) event.preventDefault(); });
  section.addEventListener('drop', (event) => {
    dragDepth = 0;
    section.classList.remove('inv-dropping');
    const data = event.dataTransfer;
    if (!data || data.types.includes('application/x-phorminx-item')) return;
    event.preventDefault();
    const parentNode = event.target.closest?.('[data-node-id]') ? Number(event.target.closest('[data-node-id]').dataset.nodeId) : null;
    if (data.files.length) { void addImages(data.files, 'drop', parentNode); return; }
    const text = data.getData('text/uri-list').split('\n').find((line) => line && !line.startsWith('#')) || data.getData('text/plain');
    if (text.trim()) void addText(text.trim(), false, parentNode);
  });

  $('inv-capture').addEventListener('submit', async (event) => {
    event.preventDefault();
    const text = $('inv-capture-text').value;
    if (!text.trim()) { $('inv-capture-text').focus(); return; }
    const id = await addText(text, true);
    if (id !== undefined && id !== null) { $('inv-capture-text').value = ''; status('Note added.'); }
  });
  $('inv-capture-text').addEventListener('keydown', (event) => {
    if (event.key === 'Enter' && (event.ctrlKey || event.metaKey)) { event.preventDefault(); $('inv-capture').requestSubmit(); }
  });
  $('inv-file').addEventListener('click', () => { state.diagram.pendingParent = null; $('inv-file-input').click(); });
  $('inv-file-input').addEventListener('change', async (event) => {
    const parent = state.diagram.pendingParent;
    state.diagram.pendingParent = null;
    await addImages(event.target.files, 'file', parent);
    event.target.value = '';
  });
  $('inv-paste').addEventListener('click', () => void act(async () => {
    const title = await invoke('investigation_capture_clipboard', {id: state.id});
    return title;
  }, 'Clipboard added.'));

  // ---------- voice ----------

  function renderVoice(voice) {
    if (!voice) return;
    const active = voice.state !== 'idle';
    const here = voice.investigationId === state.id;
    const button = $('inv-voice');
    button.classList.toggle('recording', active);
    button.setAttribute('aria-pressed', String(active));
    $('inv-voice-label').textContent = !active ? 'Voice note' : voice.state === 'finishing' ? 'Finishing…' : voice.state === 'starting' ? 'Starting…' : 'Stop';
    button.disabled = voice.state === 'finishing';
    $('inv-voice-live').hidden = !active || !here;
    $('inv-voice-text').textContent = voice.text || voice.notice || 'Listening…';
    if (voice.revision !== state.lastVoiceRevision) {
      state.lastVoiceRevision = voice.revision;
      if (!active && voice.notice && voice.notice !== state.lastVoiceNotice && state.lastVoiceRevision > 0) {
        state.lastVoiceNotice = voice.notice;
        if (visible()) status(voice.notice);
        if (visible()) void refresh(true);
      }
    }
    state.voiceActive = active;
  }

  async function toggleVoice(parentNode = null) {
    try {
      const kind = state.voiceActive ? 'stop' : 'start';
      const voice = await invoke('investigation_voice', {kind, id: state.id, parentNode});
      state.lastVoiceNotice = '';
      renderVoice(voice);
      if (kind === 'start') status('Listening. Only the recognized text is kept; no audio is stored.');
    } catch (error) { fail(error); }
  }
  $('inv-voice').addEventListener('click', () => void toggleVoice());

  // ---------- header actions ----------

  function prompt({kicker = 'INVESTIGATION', title, value = '', placeholder = '', confirm = 'Save', multiline = false}) {
    return new Promise((resolve) => {
      const dialog = $('inv-prompt');
      $('inv-prompt-kicker').textContent = kicker;
      $('inv-prompt-title').textContent = title;
      const input = $('inv-prompt-input');
      input.value = value;
      input.placeholder = placeholder;
      input.rows = multiline ? 6 : 1;
      input.classList.toggle('single', !multiline);
      $('inv-prompt-confirm').textContent = confirm;
      $('inv-prompt-error').textContent = '';
      dialog.returnValue = '';
      const onKey = (event) => { if (!multiline && event.key === 'Enter') { event.preventDefault(); dialog.close('confirm'); } };
      input.addEventListener('keydown', onKey);
      dialog.addEventListener('close', () => {
        input.removeEventListener('keydown', onKey);
        resolve(dialog.returnValue === 'confirm' ? input.value : null);
      }, {once: true});
      dialog.showModal();
      input.focus();
      input.select();
    });
  }

  $('inv-start').addEventListener('submit', async (event) => {
    event.preventDefault();
    const id = await act(() => invoke('investigation_create', {title: $('inv-start-title').value}));
    if (id) { $('inv-start-title').value = ''; openInvestigation(id); }
  });
  $('inv-new').addEventListener('click', async () => {
    const title = await prompt({title: 'New investigation', placeholder: 'What are you looking into?', confirm: 'Create'});
    if (title === null) return;
    const id = await act(() => invoke('investigation_create', {title}));
    if (id) openInvestigation(id);
  });
  $('inv-picker').addEventListener('change', (event) => openInvestigation(Number(event.target.value)));
  $('inv-active').addEventListener('click', () => {
    if (state.id === null) return;
    const active = state.overview?.activeId === state.id;
    void act(() => invoke('investigation_set_active', {id: active ? null : state.id}), active ? 'Quick capture now creates a “Quick captures” investigation when needed.' : 'Quick capture now sends the clipboard here.');
  });
  const moreMenu = $('inv-more-menu');
  $('inv-more').addEventListener('click', (event) => {
    event.stopPropagation();
    moreMenu.hidden = !moreMenu.hidden;
    $('inv-more').setAttribute('aria-expanded', String(!moreMenu.hidden));
  });
  document.addEventListener('click', (event) => {
    if (!event.target.closest('#inv-more-menu, #inv-more')) { moreMenu.hidden = true; $('inv-more').setAttribute('aria-expanded', 'false'); }
    if (!event.target.closest('#inv-node-menu, .inv-node-plus')) $('inv-node-menu').hidden = true;
  });
  moreMenu.addEventListener('click', async (event) => {
    const kind = event.target.closest('[data-inv-menu]')?.dataset.invMenu;
    if (!kind || state.id === null) return;
    moreMenu.hidden = true;
    if (kind === 'rename') {
      const title = await prompt({title: 'Rename investigation', value: state.detail?.title || ''});
      if (title !== null) await act(() => invoke('investigation_rename', {id: state.id, title}), 'Renamed.');
    } else if (kind === 'delete') {
      const count = state.detail?.items.length || 0;
      if (!window.confirm(`Delete “${state.detail?.title}” and its ${count} card${count === 1 ? '' : 's'}, screenshots and diagram? This cannot be undone.`)) return;
      const id = state.id;
      state.id = null;
      state.detail = null;
      await act(() => invoke('investigation_delete', {id}), 'Investigation deleted.');
    } else if (kind === 'export') {
      try {
        $('inv-export-text').value = await invoke('investigation_export', {id: state.id});
        $('inv-export-result').textContent = '';
        $('inv-export').showModal();
      } catch (error) { fail(error); }
    } else if (kind === 'capture-settings') {
      openShortcutDialog();
    }
  });
  $('inv-export-copy').addEventListener('click', async () => {
    try { await navigator.clipboard.writeText($('inv-export-text').value); $('inv-export-result').textContent = 'Copied to the clipboard.'; }
    catch (_) { $('inv-export-text').select(); $('inv-export-result').textContent = 'Press Ctrl+C to copy the selected text.'; }
  });
  $('inv-export-save').addEventListener('click', () => {
    const blob = new Blob([$('inv-export-text').value], {type: 'text/markdown'});
    const link = el('a');
    link.href = URL.createObjectURL(blob);
    link.download = `${(state.detail?.title || 'investigation').replace(/[\\/:*?"<>|]+/g, '-').slice(0, 80)}.md`;
    document.body.append(link);
    link.click();
    link.remove();
    setTimeout(() => URL.revokeObjectURL(link.href), 10_000);
    $('inv-export-result').textContent = 'Saved to your Downloads folder.';
  });

  // ---------- quick-capture shortcut ----------

  let shortcutDraft = '';
  function openShortcutDialog() {
    const capture = state.overview?.capture || {};
    shortcutDraft = capture.shortcut || '';
    $('inv-shortcut-input').value = shortcutDraft;
    $('inv-shortcut-status').textContent = capture.notice || '';
    $('inv-shortcut').returnValue = '';
    $('inv-shortcut').showModal();
  }
  $('inv-shortcut-input').addEventListener('keydown', (event) => {
    if (event.key === 'Tab' || event.key === 'Escape') return;
    event.preventDefault();
    const text = invShortcutFromEvent(event);
    if (text) { shortcutDraft = text; $('inv-shortcut-input').value = text; $('inv-shortcut-status').textContent = 'Save to apply this shortcut.'; }
  });
  async function saveShortcut(shortcut) {
    try {
      const view = await invoke('investigation_capture_shortcut', {shortcut});
      $('inv-shortcut').close();
      status(view.notice);
      await refresh();
    } catch (error) { $('inv-shortcut-status').textContent = String(error); }
  }
  $('inv-shortcut-form').addEventListener('submit', (event) => {
    if (event.submitter?.value !== 'save') return;
    event.preventDefault();
    void saveShortcut(shortcutDraft);
  });
  $('inv-shortcut-off').addEventListener('click', () => void saveShortcut(''));

  // ---------- detail dialog actions ----------

  $('inv-detail-form').addEventListener('submit', (event) => {
    if (event.submitter?.value !== 'save' || detailItemId === null) return;
    event.preventDefault();
    const itemId = detailItemId;
    void act(() => invoke('investigation_update_item', {itemId, title: $('inv-detail-title').value, body: $('inv-detail-body').value}), 'Card saved.')
      .then((result) => { if (result !== undefined) $('inv-detail').close(); });
  });
  $('inv-detail-delete').addEventListener('click', () => {
    if (detailItemId === null || !window.confirm('Delete this card? Its screenshot or preview images are removed too.')) return;
    const itemId = detailItemId;
    $('inv-detail').close();
    void act(() => invoke('investigation_delete_item', {itemId}), 'Card deleted.');
  });
  $('inv-detail-place').addEventListener('click', async () => {
    const item = itemById(detailItemId);
    if (!item) return;
    $('inv-detail').close();
    setView('diagram');
    if (!item.nodeId) {
      const center = canvasCenter();
      await act(() => invoke('investigation_diagram', {request: {kind: 'place', itemId: item.id, x: center.x - INV_NODE_WIDTH / 2, y: center.y - 48}}));
    }
    const node = state.detail?.nodes.find((candidate) => candidate.itemId === item.id);
    if (node) { state.diagram.selected = {type: 'node', id: node.id}; centerOn(node); }
  });

  // ---------- views ----------

  function setView(view) {
    state.view = view;
    for (const tab of document.querySelectorAll('[data-inv-view]')) {
      const on = tab.dataset.invView === view;
      tab.classList.toggle('active', on);
      tab.setAttribute('aria-selected', String(on));
    }
    $('inv-chronology').hidden = view !== 'chronology';
    $('inv-diagram').hidden = view !== 'diagram';
    if (view === 'diagram') renderDiagram();
  }
  for (const tab of document.querySelectorAll('[data-inv-view]')) tab.addEventListener('click', () => setView(tab.dataset.invView));
  for (const chip of document.querySelectorAll('[data-inv-filter]')) chip.addEventListener('click', () => { setFilter(chip.dataset.invFilter); renderTimeline(); });
  $('inv-search').addEventListener('input', () => {
    clearTimeout(state.searchTimer);
    state.searchTimer = setTimeout(() => void runSearch(), 220);
  });
  $('inv-assistant-toggle').addEventListener('click', () => {
    state.assistantOpen = !state.assistantOpen;
    $('inv-assistant').hidden = !state.assistantOpen;
    $('inv-body').classList.toggle('no-assistant', !state.assistantOpen);
    $('inv-assistant-toggle').setAttribute('aria-pressed', String(state.assistantOpen));
    if (state.view === 'diagram') renderDiagram();
  });

  // ---------- diagram ----------

  const canvas = $('inv-canvas');
  const viewport = $('inv-viewport');

  function nodeBox(node) {
    const item = node.itemId ? itemById(node.itemId) : null;
    return {x: node.x, y: node.y, w: INV_NODE_WIDTH, h: invNodeHeight(node, item)};
  }

  function applyView() {
    const {x, y, k} = state.diagram.view;
    viewport.setAttribute('transform', `translate(${x.toFixed(1)} ${y.toFixed(1)}) scale(${k.toFixed(4)})`);
    const grid = $('inv-grid');
    grid.setAttribute('patternTransform', `translate(${x % (24 * k)} ${y % (24 * k)}) scale(${k})`);
    $('inv-canvas-bg').setAttribute('fill', 'url(#inv-grid)');
  }

  function saveViewSoon() {
    clearTimeout(state.layoutTimer);
    const id = state.id;
    const view = {...state.diagram.view};
    state.layoutTimer = setTimeout(() => {
      if (id !== null) void invoke('investigation_save_layout', {id, layout: JSON.stringify({x: Math.round(view.x), y: Math.round(view.y), k: Number(view.k.toFixed(3))})}).catch(() => {});
    }, 600);
  }

  function canvasSize() {
    const rect = canvas.getBoundingClientRect();
    return {width: rect.width, height: rect.height, left: rect.left, top: rect.top};
  }
  function canvasCenter() {
    const size = canvasSize();
    return invScreenToWorld(state.diagram.view, size.width / 2, size.height / 2);
  }
  function centerOn(node) {
    const size = canvasSize();
    const box = nodeBox(node);
    const k = state.diagram.view.k;
    state.diagram.view = {k, x: size.width / 2 - (box.x + box.w / 2) * k, y: size.height / 2 - (box.y + box.h / 2) * k};
    applyView();
    saveViewSoon();
    renderDiagram();
  }
  function fit() {
    const size = canvasSize();
    state.diagram.view = invFitView((state.detail?.nodes || []).map(nodeBox), size.width, size.height);
    applyView();
    saveViewSoon();
  }

  function renderTray() {
    const detail = state.detail;
    if (!detail) return;
    const unplaced = detail.items.filter((item) => !item.nodeId);
    $('inv-tray-count').textContent = unplaced.length ? String(unplaced.length) : '';
    const fragment = document.createDocumentFragment();
    for (const item of [...unplaced].reverse()) {
      const row = el('div', 'inv-tray-item');
      row.dataset.kind = item.kind;
      row.draggable = true;
      row.addEventListener('dragstart', (event) => {
        event.dataTransfer.setData('application/x-phorminx-item', String(item.id));
        event.dataTransfer.effectAllowed = 'move';
      });
      const text = el('div', 'inv-tray-text');
      text.append(el('small', '', `${invKindLabel(item.kind)} · #${item.id}`), el('span', '', invCardLabel(item)));
      const place = el('button', '', 'Place');
      place.type = 'button';
      place.addEventListener('click', () => {
        const center = canvasCenter();
        const offset = (detail.nodes.length % 5) * 24;
        void act(() => invoke('investigation_diagram', {request: {kind: 'place', itemId: item.id, x: center.x - INV_NODE_WIDTH / 2 + offset, y: center.y - 48 + offset}}));
      });
      row.append(text, place);
      fragment.append(row);
    }
    if (!unplaced.length) fragment.append(el('p', 'inv-tray-empty', 'Every card is on the diagram.'));
    $('inv-tray-list').replaceChildren(fragment);
  }

  $('inv-canvas-wrap').addEventListener('dragover', (event) => {
    if (event.dataTransfer?.types.includes('application/x-phorminx-item')) { event.preventDefault(); event.dataTransfer.dropEffect = 'move'; }
  });
  $('inv-canvas-wrap').addEventListener('drop', (event) => {
    const id = Number(event.dataTransfer?.getData('application/x-phorminx-item'));
    if (!id) return;
    event.preventDefault();
    event.stopPropagation();
    const size = canvasSize();
    const point = invScreenToWorld(state.diagram.view, event.clientX - size.left, event.clientY - size.top);
    void act(() => invoke('investigation_diagram', {request: {kind: 'place', itemId: id, x: point.x - INV_NODE_WIDTH / 2, y: point.y - 30}}));
  });

  function nodeContent(node, item, box) {
    const card = el('div', `inv-node${item ? '' : ' concept'}`);
    card.dataset.kind = item ? item.kind : 'concept';
    if (state.diagram.selected?.type === 'node' && state.diagram.selected.id === node.id) card.classList.add('selected');
    if (item) {
      const head = el('div', 'inv-node-head');
      head.append(el('span', 'inv-kind', invKindLabel(item.kind)), el('span', 'inv-card-id', `#${item.id}`));
      card.append(head);
      if ((item.kind === 'image' || item.kind === 'link') && item.imageSha) {
        const image = el('img', 'inv-node-image');
        image.alt = '';
        image.draggable = false;
        setImage(image, item.imageSha);
        card.append(image);
      }
      card.append(el('div', 'inv-node-label', invCardLabel(item)));
    } else {
      card.append(el('div', 'inv-node-label', node.label));
    }
    card.style.height = `${box.h}px`;
    return card;
  }

  function renderDiagram() {
    const detail = state.detail;
    if (!detail || state.view !== 'diagram' || visible() === false) return;
    if (state.diagram.viewLoadedFor !== detail.id) {
      state.diagram.viewLoadedFor = detail.id;
      const saved = invParseLayout(detail.layout);
      if (saved) state.diagram.view = saved; else fit();
    }
    applyView();
    const boxes = new Map(detail.nodes.map((node) => [node.id, nodeBox(node)]));
    const edges = document.createDocumentFragment();
    for (const edge of detail.edges) {
      const a = boxes.get(edge.from), b = boxes.get(edge.to);
      if (!a || !b) continue;
      const selected = state.diagram.selected?.type === 'edge' && state.diagram.selected.id === edge.id;
      const group = svgEl('g', {class: `inv-edge-group${edge.suggested ? ' suggested' : ''}${selected ? ' selected' : ''}`, 'data-edge-id': edge.id});
      const d = invEdgePath(a, b);
      group.append(svgEl('path', {class: 'inv-edge-hit', d}), svgEl('path', {class: 'inv-edge', d}));
      if (edge.suggested || selected) {
        const mid = invMidpoint(a, b);
        const width = edge.suggested ? 156 : 132;
        const holder = svgEl('foreignObject', {x: mid.x - width / 2, y: mid.y - 15, width, height: 30});
        const bar = el('div', 'inv-edge-actions');
        if (edge.suggested) {
          bar.title = edge.rationale || 'Suggested by the assistant';
          const accept = el('button', 'accept', 'Accept');
          accept.type = 'button';
          accept.addEventListener('click', (event) => { event.stopPropagation(); void act(() => invoke('investigation_diagram', {request: {kind: 'accept', edgeId: edge.id}}), 'Connection accepted.'); });
          const dismiss = el('button', '', 'Dismiss');
          dismiss.type = 'button';
          dismiss.addEventListener('click', (event) => { event.stopPropagation(); void act(() => invoke('investigation_diagram', {request: {kind: 'disconnect', edgeId: edge.id}})); });
          bar.append(accept, dismiss);
        } else {
          const remove = el('button', '', 'Remove link');
          remove.type = 'button';
          remove.addEventListener('click', (event) => { event.stopPropagation(); state.diagram.selected = null; void act(() => invoke('investigation_diagram', {request: {kind: 'disconnect', edgeId: edge.id}})); });
          bar.append(remove);
        }
        holder.append(bar);
        group.append(holder);
      }
      if (edge.suggested && edge.rationale) {
        const title = svgEl('title');
        title.textContent = `Suggested: ${edge.rationale}`;
        group.append(title);
      }
      group.addEventListener('pointerdown', (event) => {
        if (event.target.closest('button')) return;
        event.stopPropagation();
        state.diagram.selected = {type: 'edge', id: edge.id};
        renderDiagram();
      });
      edges.append(group);
    }
    $('inv-edges').replaceChildren(edges);
    const nodes = document.createDocumentFragment();
    for (const node of detail.nodes) {
      const item = node.itemId ? itemById(node.itemId) : null;
      const box = boxes.get(node.id);
      const group = svgEl('g', {class: 'inv-node-group', 'data-node-id': node.id, transform: `translate(${box.x} ${box.y})`});
      const holder = svgEl('foreignObject', {x: 0, y: 0, width: box.w, height: box.h});
      holder.append(nodeContent(node, item, box));
      const plus = svgEl('g', {class: 'inv-node-plus', transform: `translate(${box.w} ${box.h / 2})`, role: 'button', tabindex: '0', 'aria-label': 'Add connected item'});
      plus.append(svgEl('circle', {r: 12}), svgEl('path', {d: 'M-5,0 H5 M0,-5 V5'}));
      const handle = svgEl('circle', {class: 'inv-node-handle', cx: box.w / 2, cy: box.h, r: 6});
      const handleTitle = svgEl('title');
      handleTitle.textContent = 'Drag to another node to connect';
      handle.append(handleTitle);
      group.append(holder, plus, handle);
      group.addEventListener('pointerdown', (event) => startNodeDrag(event, node, group));
      group.addEventListener('dblclick', (event) => {
        event.stopPropagation();
        if (item) openDetail(item.id); else void renameConcept(node);
      });
      plus.addEventListener('pointerdown', (event) => event.stopPropagation());
      plus.addEventListener('click', (event) => { event.stopPropagation(); openNodeMenu(node, plus); });
      plus.addEventListener('keydown', (event) => { if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); openNodeMenu(node, plus); } });
      handle.addEventListener('pointerdown', (event) => startConnect(event, node));
      nodes.append(group);
    }
    $('inv-nodes').replaceChildren(nodes);
    if (!detail.nodes.length) {
      const hint = svgEl('text', {class: 'inv-canvas-empty', x: 40 , y: 60});
      hint.textContent = 'Drag cards here from the Unplaced list, or add a concept.';
      $('inv-nodes').append(hint);
    }
  }

  function startNodeDrag(event, node, group) {
    if (event.button !== 0 || event.target.closest('.inv-node-plus, .inv-node-handle')) return;
    event.stopPropagation();
    const start = {x: event.clientX, y: event.clientY};
    const origin = {x: node.x, y: node.y};
    const k = state.diagram.view.k;
    let moved = false;
    group.setPointerCapture(event.pointerId);
    const onMove = (move) => {
      const dx = (move.clientX - start.x) / k, dy = (move.clientY - start.y) / k;
      if (!moved && Math.hypot(dx * k, dy * k) < 4) return;
      moved = true;
      node.x = origin.x + dx;
      node.y = origin.y + dy;
      group.setAttribute('transform', `translate(${node.x} ${node.y})`);
      redrawEdgesFor(node.id);
    };
    const onUp = () => {
      group.removeEventListener('pointermove', onMove);
      group.removeEventListener('pointerup', onUp);
      group.removeEventListener('pointercancel', onUp);
      if (moved) {
        void invoke('investigation_diagram', {request: {kind: 'move', nodeId: node.id, x: Math.round(node.x), y: Math.round(node.y)}}).catch(fail);
      } else {
        state.diagram.selected = {type: 'node', id: node.id};
        renderDiagram();
      }
    };
    group.addEventListener('pointermove', onMove);
    group.addEventListener('pointerup', onUp);
    group.addEventListener('pointercancel', onUp);
  }

  function redrawEdgesFor(nodeId) {
    const detail = state.detail;
    const boxes = new Map(detail.nodes.map((node) => [node.id, nodeBox(node)]));
    for (const edge of detail.edges) {
      if (edge.from !== nodeId && edge.to !== nodeId) continue;
      const group = $('inv-edges').querySelector(`[data-edge-id="${edge.id}"]`);
      if (!group) continue;
      const a = boxes.get(edge.from), b = boxes.get(edge.to);
      const d = invEdgePath(a, b);
      for (const path of group.querySelectorAll('path')) path.setAttribute('d', d);
      const holder = group.querySelector('foreignObject');
      if (holder) {
        const mid = invMidpoint(a, b);
        holder.setAttribute('x', mid.x - Number(holder.getAttribute('width')) / 2);
        holder.setAttribute('y', mid.y - 15);
      }
    }
  }

  function startConnect(event, node) {
    if (event.button !== 0) return;
    event.stopPropagation();
    event.preventDefault();
    const preview = $('inv-link-preview');
    const from = nodeBox(node);
    const size = canvasSize();
    canvas.setPointerCapture(event.pointerId);
    const onMove = (move) => {
      const point = invScreenToWorld(state.diagram.view, move.clientX - size.left, move.clientY - size.top);
      const sx = from.x + from.w / 2, sy = from.y + from.h;
      preview.setAttribute('d', `M${sx},${sy} C${sx},${sy + 60} ${point.x},${point.y - 60} ${point.x},${point.y}`);
    };
    const onUp = (up) => {
      canvas.removeEventListener('pointermove', onMove);
      canvas.removeEventListener('pointerup', onUp);
      canvas.removeEventListener('pointercancel', onUp);
      preview.setAttribute('d', '');
      canvas.releasePointerCapture(up.pointerId);
      const target = document.elementFromPoint(up.clientX, up.clientY)?.closest?.('[data-node-id]');
      const to = target ? Number(target.dataset.nodeId) : null;
      if (to && to !== node.id) void act(() => invoke('investigation_diagram', {request: {kind: 'connect', nodeId: node.id, otherNodeId: to}}), 'Connected.');
    };
    canvas.addEventListener('pointermove', onMove);
    canvas.addEventListener('pointerup', onUp);
    canvas.addEventListener('pointercancel', onUp);
  }

  // Panning on the background; wheel zooms around the pointer.
  canvas.addEventListener('pointerdown', (event) => {
    if (event.button !== 0 || event.target.closest('[data-node-id], [data-edge-id]')) return;
    const start = {x: event.clientX, y: event.clientY, view: {...state.diagram.view}};
    let moved = false;
    canvas.setPointerCapture(event.pointerId);
    canvas.classList.add('panning');
    const onMove = (move) => {
      moved = true;
      state.diagram.view = {...start.view, x: start.view.x + move.clientX - start.x, y: start.view.y + move.clientY - start.y};
      applyView();
    };
    const onUp = () => {
      canvas.classList.remove('panning');
      canvas.removeEventListener('pointermove', onMove);
      canvas.removeEventListener('pointerup', onUp);
      canvas.removeEventListener('pointercancel', onUp);
      if (moved) saveViewSoon();
      else if (state.diagram.selected) { state.diagram.selected = null; renderDiagram(); }
    };
    canvas.addEventListener('pointermove', onMove);
    canvas.addEventListener('pointerup', onUp);
    canvas.addEventListener('pointercancel', onUp);
  });
  canvas.addEventListener('wheel', (event) => {
    event.preventDefault();
    const size = canvasSize();
    state.diagram.view = invZoomAt(state.diagram.view, Math.exp(-event.deltaY * 0.0015), event.clientX - size.left, event.clientY - size.top);
    applyView();
    saveViewSoon();
  }, {passive: false});
  const zoomBy = (factor) => {
    const size = canvasSize();
    state.diagram.view = invZoomAt(state.diagram.view, factor, size.width / 2, size.height / 2);
    applyView();
    saveViewSoon();
  };
  $('inv-zoom-in').addEventListener('click', () => zoomBy(1.25));
  $('inv-zoom-out').addEventListener('click', () => zoomBy(0.8));
  $('inv-zoom-fit').addEventListener('click', fit);

  document.addEventListener('keydown', (event) => {
    if (!visible() || state.view !== 'diagram' || isTextField(event.target) || document.querySelector('dialog[open]')) return;
    if (event.key === 'Escape') { state.diagram.selected = null; $('inv-node-menu').hidden = true; renderDiagram(); return; }
    if (event.key !== 'Delete' && event.key !== 'Backspace') return;
    const selected = state.diagram.selected;
    if (!selected) return;
    event.preventDefault();
    state.diagram.selected = null;
    if (selected.type === 'edge') void act(() => invoke('investigation_diagram', {request: {kind: 'disconnect', edgeId: selected.id}}), 'Connection removed.');
    else {
      const node = state.detail?.nodes.find((candidate) => candidate.id === selected.id);
      const label = node?.itemId ? 'Card moved back to Unplaced. It is still in the chronology.' : 'Concept removed.';
      void act(() => invoke('investigation_diagram', {request: {kind: 'remove', nodeId: selected.id}}), label);
    }
  });

  async function renameConcept(node) {
    const label = await prompt({kicker: 'CONCEPT', title: 'Rename concept', value: node.label});
    if (label !== null) await act(() => invoke('investigation_diagram', {request: {kind: 'rename', nodeId: node.id, label}}));
  }

  $('inv-add-concept').addEventListener('click', async () => {
    const label = await prompt({kicker: 'CONCEPT', title: 'Add a concept', placeholder: 'A person, place, claim or theme', confirm: 'Add'});
    if (label === null) return;
    const center = canvasCenter();
    await act(() => invoke('investigation_diagram', {request: {kind: 'concept', investigationId: state.id, label, x: center.x - INV_NODE_WIDTH / 2, y: center.y - 29}}));
  });

  function openNodeMenu(node, anchor) {
    const menu = $('inv-node-menu');
    state.diagram.menuNode = node.id;
    const wrap = $('inv-canvas-wrap').getBoundingClientRect();
    const rect = anchor.getBoundingClientRect();
    menu.style.left = `${Math.min(rect.right - wrap.left + 6, wrap.width - 170)}px`;
    menu.style.top = `${Math.max(8, rect.top - wrap.top - 8)}px`;
    menu.hidden = false;
    menu.querySelector('button')?.focus();
  }
  $('inv-node-menu').addEventListener('click', async (event) => {
    const kind = event.target.closest('[data-node-add]')?.dataset.nodeAdd;
    const parentNode = state.diagram.menuNode;
    if (!kind || parentNode === null) return;
    $('inv-node-menu').hidden = true;
    const parent = state.detail?.nodes.find((node) => node.id === parentNode);
    if (kind === 'note') {
      const text = await prompt({kicker: 'CONNECTED NOTE', title: 'Add a note', multiline: true, confirm: 'Add note'});
      if (text !== null && text.trim()) await addText(text, true, parentNode);
    } else if (kind === 'link') {
      const url = await prompt({kicker: 'CONNECTED LINK', title: 'Add a link', placeholder: 'https://…', confirm: 'Add link'});
      if (url !== null && url.trim()) await addText(url.trim(), false, parentNode);
    } else if (kind === 'image') {
      state.diagram.pendingParent = parentNode;
      $('inv-file-input').click();
    } else if (kind === 'voice') {
      await toggleVoice(parentNode);
    } else if (kind === 'concept' && parent) {
      const label = await prompt({kicker: 'CONNECTED CONCEPT', title: 'Add a concept', confirm: 'Add'});
      if (label !== null) await act(() => invoke('investigation_diagram', {request: {kind: 'concept', investigationId: state.id, label, nodeId: parentNode, x: parent.x + 300, y: parent.y}}));
    }
  });

  $('inv-suggest').addEventListener('click', () => void assistantAction('suggest'));
  $('inv-dismiss-suggestions').addEventListener('click', () => void assistantAction('dismiss-suggestions'));

  // ---------- assistant ----------

  function renderWithCitations(text) {
    const fragment = renderMarkdown(text);
    const walker = document.createTreeWalker(fragment, NodeFilter.SHOW_TEXT);
    const targets = [];
    for (let current = walker.nextNode(); current; current = walker.nextNode()) if (/\[\s*#\d/.test(current.nodeValue)) targets.push(current);
    for (const textNode of targets) {
      const replacement = document.createDocumentFragment();
      for (const segment of invSplitCitations(textNode.nodeValue)) {
        if (segment.text !== undefined) { replacement.append(document.createTextNode(segment.text)); continue; }
        for (const id of segment.cite) {
          const item = itemById(id);
          if (!item) { replacement.append(document.createTextNode(`[#${id}]`)); continue; }
          const chip = el('button', 'inv-cite', `#${id}`);
          chip.type = 'button';
          chip.title = `${invKindLabel(item.kind)}: ${invCardLabel(item)}`;
          chip.addEventListener('click', (event) => { event.stopPropagation(); jumpToCard(id); });
          replacement.append(chip);
        }
      }
      textNode.replaceWith(replacement);
    }
    return fragment;
  }

  function renderMessages() {
    const detail = state.detail;
    if (!detail) return;
    const log = $('inv-messages');
    const atBottom = log.scrollHeight - log.scrollTop - log.clientHeight < 40;
    const fragment = document.createDocumentFragment();
    for (const message of detail.messages) {
      const row = el('div', `inv-message ${message.role} ${message.status}`);
      if (message.role === 'user') row.append(el('p', '', message.text));
      else {
        const body = el('div', 'markdown');
        body.append(renderWithCitations(message.text || (message.status === 'streaming' ? '' : '')));
        if (message.status === 'streaming') body.append(el('span', 'inv-typing', ''));
        row.append(body);
        const foot = el('div', 'inv-message-foot');
        if (message.status === 'complete') {
          const pin = el('button', '', 'Pin as card');
          pin.type = 'button';
          pin.addEventListener('click', () => void assistantAction('pin', {messageId: message.id}, 'Answer pinned to the chronology.'));
          foot.append(pin);
        } else if (message.status !== 'streaming') {
          foot.append(el('span', 'inv-message-state', message.status === 'cancelled' ? 'Stopped' : 'Did not finish'));
        }
        row.append(foot);
      }
      fragment.append(row);
    }
    if (!detail.messages.length) {
      const intro = el('div', 'inv-assistant-intro');
      intro.append(el('strong', '', 'Ask across everything you collected.'), el('p', '', 'Answers use this investigation’s cards and cite them, like [#12]. Select a citation to jump to its card.'));
      fragment.append(intro);
    }
    log.replaceChildren(fragment);
    if (atBottom) log.scrollTop = log.scrollHeight;
  }

  function renderAssistantState(view) {
    if (!view) return;
    $('inv-provider').textContent = view.provider || '';
    const busy = view.state !== 'idle';
    const here = view.investigationId === state.id;
    $('inv-stop-answer').hidden = !(busy && view.state === 'answering' && here);
    $('inv-ask').querySelector('button[type="submit"]').disabled = busy;
    $('inv-suggest').disabled = busy;
    $('inv-suggest').textContent = view.state === 'suggesting' ? 'Suggesting…' : 'Suggest connections';
    $('inv-assistant-notice').textContent = busy ? (view.state === 'answering' ? 'Answering from your cards…' : 'Looking for connections…') : view.notice || '';
    if (state.assistantBusy && !busy && visible()) void refresh(true);
    state.assistantBusy = busy;
  }

  async function assistantAction(kind, extra = {}, success = '') {
    if (state.id === null) return;
    try {
      const view = await invoke('investigation_assistant', {kind, id: state.id, text: extra.text ?? null, messageId: extra.messageId ?? null});
      renderAssistantState(view);
      status(success);
      await refresh(true);
    } catch (error) {
      $('inv-assistant-notice').textContent = String(error);
    }
  }

  $('inv-ask').addEventListener('submit', async (event) => {
    event.preventDefault();
    const text = $('inv-question').value.trim();
    if (!text) return;
    await assistantAction('ask', {text});
    if (state.assistantBusy) $('inv-question').value = '';
  });
  $('inv-question').addEventListener('keydown', (event) => {
    if (event.key === 'Enter' && !event.shiftKey) { event.preventDefault(); $('inv-ask').requestSubmit(); }
  });
  $('inv-stop-answer').addEventListener('click', () => void assistantAction('cancel'));
  $('inv-clear-chat').addEventListener('click', () => {
    if (state.detail?.messages.length && window.confirm('Clear this conversation? Pinned answers stay in the chronology.')) void assistantAction('clear');
  });

  // ---------- OCR model buttons ----------

  document.addEventListener('click', (event) => {
    const install = event.target.closest('[data-ocr-install]');
    const remove = event.target.closest('[data-ocr-remove]');
    if (install) void invoke('investigation_models', {kind: 'install'}).then(() => refresh()).catch((error) => { install.closest('[data-ocr-card]').querySelector('[data-ocr-message]').textContent = String(error); });
    if (remove && window.confirm('Remove the screenshot text models? Text already extracted is kept.')) void invoke('investigation_models', {kind: 'remove'}).then(() => refresh()).catch(fail);
  });

  // ---------- lifecycle ----------

  document.addEventListener('phorminx:route', (event) => {
    state.route = event.detail;
    if (visible()) {
      void refresh(true);
      if (state.view === 'diagram') renderDiagram();
    } else if (['models', 'setup'].includes(state.route)) void refresh();
  });
  window.addEventListener('resize', () => { if (visible() && state.view === 'diagram') applyView(); });

  if (call) {
    setInterval(() => {
      if (visible() || state.voiceActive || state.assistantBusy || ['models', 'setup'].includes(state.route)) void refresh();
    }, 700);
    void refresh();
  }
})();
