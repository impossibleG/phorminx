// One way to pick a model everywhere. Each model field (formatting, search,
// the meeting and investigation assistant and its fallback, live translation,
// actions) becomes a button that opens a searchable list from the shared
// model registry, like Models → Find a model: what is installed can be picked
// with a click, and what fits but is missing can be installed right there.
// The original field stays in the page, hidden, so the code that saves
// settings keeps reading its value.
(function (global) {
  'use strict';

  const invoke = global.__TAURI__?.core?.invoke;
  const KIND_LABELS = { language: 'language', embedding: 'search (embedding)', vision: 'vision', tools: 'tools', speech: 'speech', ocr: 'screenshot text' };
  const TAG_LABELS = { language: 'Language', embedding: 'Search', vision: 'Vision', tools: 'Tools', speech: 'Speech', ocr: 'Screenshot text' };
  const pickers = [];
  let entries = [];
  // Until the registry has answered once, nothing is called "not installed".
  let loaded = false;
  let pull = null;
  let pullTimer = null;

  function sameModel(left, right) {
    const normalize = (name) => {
      const value = String(name || '').trim().toLowerCase();
      return value.includes(':') ? value : `${value}:latest`;
    };
    return normalize(left) === normalize(right);
  }

  function formatSize(bytes) {
    if (!bytes) return '';
    return bytes >= 1073741824 ? `${(bytes / 1073741824).toFixed(1)} GB` : `${Math.round(bytes / 1048576)} MB`;
  }

  /** Installed models of `kind`, plus a note for a current value that does not fit. */
  function options(list, kind, current) {
    const fitting = list
      .filter((entry) => entry.family === 'ollama' && entry.installed && entry.tags.includes(kind))
      .map((entry) => ({ value: entry.name, label: entry.usedBy.length ? `${entry.name} · used by ${entry.usedBy.join(', ')}` : entry.name }));
    if (current && !fitting.some((option) => sameModel(option.value, current))) {
      const known = list.find((entry) => entry.family === 'ollama' && entry.installed && sameModel(entry.name, current))
        || list.find((entry) => entry.family === 'ollama' && sameModel(entry.name, current));
      const why = !known || !known.installed ? 'not installed' : `not a ${KIND_LABELS[kind] || kind} model`;
      fitting.push({ value: current, label: `${current} · ${why}`, warning: true });
    }
    return fitting;
  }

  /**
   * What the picker list shows for a slot of `kind`: Ollama models matching
   * `query`, split into installed and available, each marked whether it fits.
   * `scope` is 'fits' (only `kind`) or 'all' (every kind, misfits disabled).
   */
  function catalog(list, kind, query = '', scope = 'fits') {
    const needle = query.trim().toLowerCase();
    const shown = list.filter((entry) => {
      if (entry.family !== 'ollama') return false;
      if (scope === 'fits' && !entry.tags.includes(kind)) return false;
      const haystack = [entry.name, entry.note, ...entry.tags.map((tag) => TAG_LABELS[tag] || tag), ...entry.usedBy].join(' ').toLowerCase();
      return !needle || haystack.includes(needle);
    }).map((entry) => ({ entry, fits: entry.tags.includes(kind) }));
    return {
      installed: shown.filter((item) => item.entry.installed),
      available: shown.filter((item) => !item.entry.installed),
    };
  }

  function element(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text != null) node.textContent = text;
    return node;
  }

  function pullText(view) {
    if (!view) return '';
    return view.percent != null && view.state === 'pulling' ? `${view.message} (${view.percent}%)` : view.message;
  }

  /** Downloads `name` through Ollama; every open picker shows the progress. */
  async function install(name, kind) {
    if (!invoke) return;
    try {
      pull = await invoke('models_pull', { name, expected: kind || null });
    } catch (error) {
      pull = { name, state: 'failed', percent: null, message: String(error) };
    }
    for (const picker of pickers) picker.render();
    clearInterval(pullTimer);
    if (pull.state !== 'pulling') { void refresh(); return; }
    pullTimer = setInterval(async () => {
      try { pull = await invoke('models_pull_status'); } catch (error) { pull = { ...pull, state: 'failed', message: String(error) }; }
      if (pull.state !== 'pulling') {
        clearInterval(pullTimer);
        await refresh();
        for (const picker of pickers) picker.installed(pull);
      }
      for (const picker of pickers) picker.render();
    }, 1000);
  }

  /**
   * Turns the field with `id` into a picker for `kind` models.
   * `empty` labels the no-model choice; `free()` returning true shows the
   * original free-text field instead (external providers name their own models).
   */
  function attach(id, { kind, empty = 'Choose a model', free = null } = {}) {
    const field = document.getElementById(id);
    if (!field || field.dataset.modelPicker) return;
    field.dataset.modelPicker = kind;
    const native = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(field), 'value');
    let current = native.get.call(field) || '';
    let query = '';
    let scope = 'fits';
    let waitingFor = null;

    const root = element('div', 'model-picker');
    const button = element('button', 'model-picker-button');
    button.type = 'button';
    button.setAttribute('aria-haspopup', 'dialog');
    button.setAttribute('aria-expanded', 'false');
    if (field.getAttribute('aria-label')) button.setAttribute('aria-label', field.getAttribute('aria-label'));
    const label = field.id && document.querySelector(`label[for="${field.id}"]`);
    if (label) { button.id = `${field.id}-picker`; label.htmlFor = button.id; }
    const panel = element('div', 'model-picker-panel');
    panel.setAttribute('role', 'dialog');
    panel.hidden = true;
    const search = element('input', 'model-picker-search');
    search.type = 'search';
    search.placeholder = 'Search models, kinds or features';
    search.setAttribute('aria-label', 'Search models');
    const filters = element('div', 'model-filters model-picker-filters');
    filters.setAttribute('role', 'group');
    filters.setAttribute('aria-label', 'Show');
    const list = element('div', 'model-picker-list');
    list.setAttribute('role', 'listbox');
    const status = element('p', 'model-picker-status');
    status.setAttribute('role', 'status');
    panel.append(search, filters, list, status);
    const hint = element('small', 'model-picker-hint');
    hint.hidden = true;
    root.append(button, panel);
    field.after(root);
    root.after(hint);

    const choose = (value) => {
      current = value;
      native.set.call(field, current);
      field.dispatchEvent(new Event('input', { bubbles: true }));
      field.dispatchEvent(new Event('change', { bubbles: true }));
      close();
      render();
    };
    const open = () => {
      panel.hidden = false;
      button.setAttribute('aria-expanded', 'true');
      root.classList.add('open');
      renderPanel();
      search.focus();
      if (!loaded || !entries.length) void refresh();
    };
    function close() {
      if (panel.hidden) return;
      panel.hidden = true;
      button.setAttribute('aria-expanded', 'false');
      root.classList.remove('open');
    }

    const row = ({ entry, fits }) => {
      const selected = sameModel(entry.name, current);
      const item = element('div', `model-entry model-picker-option${entry.installed ? ' installed' : ''}${selected ? ' selected' : ''}${fits ? '' : ' misfit'}`);
      item.setAttribute('role', 'option');
      item.setAttribute('aria-selected', String(selected));
      const head = element('div', 'model-entry-head');
      const size = formatSize(entry.sizeBytes);
      head.append(element('strong', '', `${selected ? '✓ ' : ''}${entry.name}`), element('small', '', [size, entry.installed ? 'Installed' : 'Not installed', fits ? '' : `not a ${KIND_LABELS[kind] || kind} model`].filter(Boolean).join(' · ')));
      const tags = element('div', 'model-tags');
      for (const tag of entry.tags) tags.append(element('span', 'model-tag', TAG_LABELS[tag] || tag));
      for (const feature of entry.usedBy) tags.append(element('span', 'model-tag used', `Used by ${feature}`));
      item.append(head, tags);
      if (entry.note && !entry.installed) item.append(element('p', 'setting-hint', entry.note));
      if (entry.installed && fits) {
        item.tabIndex = 0;
        item.addEventListener('click', () => choose(entry.name));
        item.addEventListener('keydown', (event) => { if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); choose(entry.name); } });
      } else if (!entry.installed && fits) {
        const busy = pull?.state === 'pulling';
        const mine = busy && sameModel(pull.name, entry.name);
        const action = element('button', '', mine ? `Installing… ${pull.percent ?? 0}%` : 'Install');
        action.type = 'button';
        action.disabled = busy;
        action.addEventListener('click', (event) => {
          event.stopPropagation();
          waitingFor = entry.name;
          void install(entry.name, kind);
        });
        item.append(action);
        if (mine) {
          const bar = element('progress', 'model-picker-progress');
          bar.max = 100;
          bar.value = pull.percent ?? 0;
          item.append(bar);
        }
      }
      return item;
    };

    function renderPanel() {
      if (panel.hidden) return;
      filters.replaceChildren(...[['fits', `${TAG_LABELS[kind] || kind} models`], ['all', 'All kinds']].map(([value, text]) => {
        const chip = element('button', 'chip', text);
        chip.type = 'button';
        chip.setAttribute('aria-pressed', String(scope === value));
        chip.addEventListener('click', () => { scope = value; renderPanel(); });
        return chip;
      }));
      const { installed, available } = catalog(entries, kind, query, scope);
      const rows = [];
      const none = element('div', `model-entry model-picker-option${current ? '' : ' selected'}`, `${current ? '' : '✓ '}${empty}`);
      none.setAttribute('role', 'option');
      none.tabIndex = 0;
      none.addEventListener('click', () => choose(''));
      none.addEventListener('keydown', (event) => { if (event.key === 'Enter') choose(''); });
      rows.push(none);
      rows.push(element('small', 'model-picker-section', installed.length ? 'INSTALLED' : 'INSTALLED · none match'));
      rows.push(...installed.map(row));
      if (available.length) {
        rows.push(element('small', 'model-picker-section', 'AVAILABLE TO INSTALL'));
        rows.push(...available.map(row));
      }
      if (!loaded) rows.push(element('p', 'setting-hint', 'Checking which models are installed…'));
      else if (!installed.length && !available.length) rows.push(element('p', 'setting-hint', 'No model matches. Models → Find a model installs any Ollama model by name.'));
      list.replaceChildren(...rows);
      status.textContent = pull && (pull.state === 'pulling' || waitingFor) ? pullText(pull) : '';
    }

    let wasExternal = false;
    function render() {
      const external = Boolean(free && free());
      // Switching back from an external provider keeps what was typed there.
      if (wasExternal && !external) current = native.get.call(field) || '';
      wasExternal = external;
      field.hidden = !external;
      root.hidden = external;
      hint.hidden = true;
      if (external) { close(); return; }
      const entry = entries.find((item) => item.family === 'ollama' && item.installed && sameModel(item.name, current));
      button.textContent = '';
      button.append(element('span', 'model-picker-value', current || empty));
      if (entry?.usedBy.length) button.append(element('small', '', `used by ${entry.usedBy.join(', ')}`));
      button.append(element('span', 'model-picker-caret', '▾'));
      const warning = loaded ? options(entries, kind, current).find((choice) => choice.warning) : null;
      if (warning) {
        const why = warning.label.split(' · ')[1];
        hint.textContent = why === 'not installed'
          ? `${warning.value} is not installed. Open the list to install it, or choose another ${KIND_LABELS[kind] || kind} model.`
          : `${warning.value} is ${why}. Open the list and choose a ${KIND_LABELS[kind] || kind} model.`;
        hint.hidden = false;
      }
      renderPanel();
    }

    button.addEventListener('click', () => (panel.hidden ? open() : close()));
    search.addEventListener('input', () => { query = search.value; renderPanel(); });
    panel.addEventListener('keydown', (event) => { if (event.key === 'Escape') { event.stopPropagation(); close(); button.focus(); } });
    document.addEventListener('mousedown', (event) => { if (!root.contains(event.target)) close(); });

    // Code elsewhere reads and writes field.value as before; the picker follows.
    Object.defineProperty(field, 'value', {
      configurable: true,
      get: () => (free && free() ? native.get.call(field) : current),
      set: (value) => {
        current = value == null ? '' : String(value);
        native.set.call(field, current);
        render();
      },
    });
    const picker = {
      render,
      // A model installed from this picker is picked once it is ready.
      installed(view) {
        if (waitingFor && view.state === 'done' && sameModel(view.name, waitingFor)) {
          const fits = entries.find((item) => item.installed && item.tags.includes(kind) && sameModel(item.name, waitingFor));
          if (fits) choose(fits.name);
        }
        if (view.state !== 'pulling') waitingFor = null;
      },
    };
    pickers.push(picker);
    render();
    return picker;
  }

  async function refresh() {
    if (!invoke) return entries;
    try { entries = await invoke('models_registry'); loaded = true; } catch { /* Keep the last list; pickers still show the saved value. */ }
    for (const picker of pickers) picker.render();
    document.dispatchEvent(new CustomEvent('phorminx:models', { detail: entries }));
    return entries;
  }

  const api = { attach, refresh, options, catalog, sameModel, install, entries: () => entries };
  global.PhorminxModels = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})(typeof window !== 'undefined' ? window : globalThis);
