// One way to pick a model everywhere. Each model field (formatting, search,
// the meeting assistant and its fallback, live translation, actions) becomes
// a picker that lists only installed models of the kind that slot needs,
// read from the shared model registry. The original field stays in the page,
// hidden, so the code that saves settings keeps reading its value.
(function (global) {
  'use strict';

  const invoke = global.__TAURI__?.core?.invoke;
  const KIND_LABELS = { language: 'language', embedding: 'search (embedding)', vision: 'vision', tools: 'tools', speech: 'speech', ocr: 'screenshot text' };
  const MORE = '__install_more__';
  const pickers = [];
  let entries = [];

  function sameModel(left, right) {
    const normalize = (name) => {
      const value = String(name || '').trim().toLowerCase();
      return value.includes(':') ? value : `${value}:latest`;
    };
    return normalize(left) === normalize(right);
  }

  /** Installed models of `kind`, plus a note for a current value that does not fit. */
  function options(list, kind, current) {
    const fitting = list
      .filter((entry) => entry.family === 'ollama' && entry.installed && entry.tags.includes(kind))
      .map((entry) => ({ value: entry.name, label: entry.usedBy.length ? `${entry.name} · used by ${entry.usedBy.join(', ')}` : entry.name }));
    if (current && !fitting.some((option) => sameModel(option.value, current))) {
      const known = list.find((entry) => entry.family === 'ollama' && sameModel(entry.name, current));
      const why = !known || !known.installed ? 'not installed' : `not a ${KIND_LABELS[kind] || kind} model`;
      fitting.push({ value: current, label: `${current} · ${why}`, warning: true });
    }
    return fitting;
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
    const select = document.createElement('select');
    select.className = 'model-picker';
    if (field.getAttribute('aria-label')) select.setAttribute('aria-label', field.getAttribute('aria-label'));
    const label = field.id && document.querySelector(`label[for="${field.id}"]`);
    if (label) { select.id = `${field.id}-picker`; label.htmlFor = select.id; }
    field.after(select);
    const hint = document.createElement('small');
    hint.className = 'model-picker-hint';
    hint.hidden = true;
    select.after(hint);

    let wasExternal = false;
    const render = () => {
      const external = Boolean(free && free());
      // Switching back from an external provider keeps what was typed there.
      if (wasExternal && !external) current = native.get.call(field) || '';
      wasExternal = external;
      field.hidden = !external;
      select.hidden = external;
      hint.hidden = true;
      if (external) return;
      const choices = options(entries, kind, current);
      const nodes = [new Option(empty, '')];
      for (const choice of choices) nodes.push(new Option(choice.label, choice.value));
      nodes.push(new Option('Install another model…', MORE));
      select.replaceChildren(...nodes);
      select.value = choices.find((choice) => sameModel(choice.value, current))?.value || '';
      const warning = choices.find((choice) => choice.warning);
      if (warning) {
        const why = warning.label.split(' · ')[1];
        hint.textContent = `${warning.value} is ${why}. Choose a ${KIND_LABELS[kind] || kind} model above, or install one from Models.`;
        hint.hidden = false;
      }
    };
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
    select.addEventListener('change', () => {
      if (select.value === MORE) {
        render();
        global.showRoute?.('models');
        return;
      }
      current = select.value;
      native.set.call(field, current);
      field.dispatchEvent(new Event('input', { bubbles: true }));
      field.dispatchEvent(new Event('change', { bubbles: true }));
      render();
    });
    const picker = { render };
    pickers.push(picker);
    render();
    return picker;
  }

  async function refresh() {
    if (!invoke) return entries;
    try { entries = await invoke('models_registry'); } catch { /* Keep the last list; pickers still show the saved value. */ }
    for (const picker of pickers) picker.render();
    document.dispatchEvent(new CustomEvent('phorminx:models', { detail: entries }));
    return entries;
  }

  const api = { attach, refresh, options, sameModel, entries: () => entries };
  global.PhorminxModels = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})(typeof window !== 'undefined' ? window : globalThis);
