// Prompts page: a library of named instructions with a version per save.
(function () {
  'use strict';
  const invoke = (command, args) => window.__TAURI__?.core?.invoke(command, args);
  const byId = (id) => document.getElementById(id);
  let library = [];
  let selectedId = null;
  let activeTag = null;
  let pendingDelete = false;

  function el(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  }
  const when = (ms) => new Date(ms).toLocaleString([], {dateStyle: 'medium', timeStyle: 'short'});
  const notice = (text) => { byId('prompt-result').textContent = text; };

  async function load() {
    try { library = (await invoke('prompts_list')) || []; } catch (error) { notice(String(error)); return; }
    renderList();
    if (selectedId !== null && !library.some((prompt) => prompt.id === selectedId)) select(null);
  }

  function renderList() {
    const query = byId('prompt-query').value.trim().toLocaleLowerCase();
    const tags = [...new Set(library.flatMap((prompt) => prompt.tags))].sort();
    if (activeTag && !tags.includes(activeTag)) activeTag = null;
    byId('prompt-tags').replaceChildren(...tags.map((tag) => {
      const button = el('button', tag === activeTag ? 'active' : '', `#${tag}`);
      button.type = 'button';
      button.setAttribute('aria-pressed', String(tag === activeTag));
      button.addEventListener('click', () => { activeTag = tag === activeTag ? null : tag; renderList(); });
      return button;
    }));
    const shown = library.filter((prompt) => (!activeTag || prompt.tags.includes(activeTag))
      && (!query || prompt.name.toLocaleLowerCase().includes(query) || prompt.body.toLocaleLowerCase().includes(query)));
    const rows = shown.map((prompt) => {
      const button = el('button', prompt.id === selectedId ? 'active' : '');
      button.type = 'button';
      button.append(el('small', '', `v${prompt.version} · ${when(prompt.updatedAtMs)}${prompt.tags.length ? ' · ' + prompt.tags.map((tag) => '#' + tag).join(' ') : ''}`), el('strong', '', prompt.name), el('span', '', prompt.body));
      button.addEventListener('click', () => select(prompt.id));
      return button;
    });
    if (!rows.length) rows.push(el('div', 'record-row', library.length ? 'No prompts match.' : 'No prompts yet. Create one to reuse it with @ in any instruction field.'));
    byId('prompt-list').replaceChildren(...rows);
  }

  async function select(id) {
    selectedId = id;
    pendingDelete = false;
    const prompt = library.find((entry) => entry.id === id);
    byId('prompt-id').value = prompt ? String(prompt.id) : '';
    byId('prompt-name').value = prompt?.name || '';
    byId('prompt-tags-input').value = prompt?.tags.join(', ') || '';
    byId('prompt-body').value = prompt?.body || '';
    byId('delete-prompt').hidden = !prompt;
    byId('delete-prompt').textContent = 'Delete prompt';
    notice('');
    renderList();
    await renderVersions();
  }

  async function renderVersions() {
    const container = byId('prompt-versions');
    if (selectedId === null) { container.replaceChildren(el('p', 'empty-state', 'Each save keeps a version you can restore.')); return; }
    let versions = [];
    try { versions = (await invoke('prompt_versions', {id: selectedId})) || []; } catch (error) { notice(String(error)); }
    container.replaceChildren(...versions.map((version, index) => {
      const row = el('div', 'prompt-version');
      const head = el('div', 'prompt-version-head');
      head.append(el('strong', '', `Version ${version.version}${index === 0 ? ' · current' : ''}`), el('small', '', when(version.createdAtMs)));
      const body = el('pre', 'prompt-version-body', version.body);
      body.hidden = true;
      const view = el('button', '', 'View');
      view.type = 'button';
      view.addEventListener('click', () => { body.hidden = !body.hidden; view.textContent = body.hidden ? 'View' : 'Hide'; });
      head.append(view);
      if (index > 0) {
        const restore = el('button', '', 'Restore');
        restore.type = 'button';
        restore.addEventListener('click', async () => {
          try {
            await invoke('prompt_restore', {id: selectedId, version: version.version});
            notice(`Version ${version.version} restored as the newest version.`);
            await changed(selectedId);
          } catch (error) { notice(String(error)); }
        });
        head.append(restore);
      }
      row.append(head, body);
      return row;
    }));
  }

  async function changed(id) {
    window.dispatchEvent(new Event('prompts:changed'));
    await load();
    await select(id);
  }

  byId('prompt-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    const id = byId('prompt-id').value ? Number(byId('prompt-id').value) : null;
    notice('Saving…');
    try {
      const saved = await invoke('prompt_save', {
        id,
        name: byId('prompt-name').value,
        tags: byId('prompt-tags-input').value.split(',').map((tag) => tag.trim()).filter(Boolean),
        body: byId('prompt-body').value,
      });
      await changed(saved);
      notice(id === null ? 'Prompt created.' : 'Saved as a new version.');
    } catch (error) { notice(String(error)); }
  });

  byId('delete-prompt').addEventListener('click', async () => {
    if (selectedId === null) return;
    if (!pendingDelete) {
      let usages = [];
      try { usages = (await invoke('prompt_usages', {id: selectedId})) || []; } catch (error) { notice(String(error)); return; }
      pendingDelete = true;
      byId('delete-prompt').textContent = 'Delete anyway';
      notice(usages.length
        ? `Used in: ${usages.join('; ')}. Those fields will show a missing-prompt chip and add nothing until you replace it. Press again to delete.`
        : 'Not used anywhere. Press again to delete this prompt and its history.');
      return;
    }
    try {
      await invoke('prompt_delete', {id: selectedId});
      window.dispatchEvent(new Event('prompts:changed'));
      await load();
      await select(null);
      notice('Prompt deleted.');
    } catch (error) { notice(String(error)); }
  });

  byId('new-prompt').addEventListener('click', () => { void select(null); byId('prompt-name').focus(); });
  byId('prompt-query').addEventListener('input', renderList);
  window.addEventListener('prompts:show', () => { void load(); });
  window.addEventListener('prompts:open', (event) => {
    if (typeof showRoute === 'function') showRoute('prompts');
    void load().then(() => select(event.detail.id));
  });
  // Another window (the review panel) asked to edit a prompt here.
  setInterval(async () => {
    try {
      const id = await invoke('prompt_take_focus');
      if (id !== null && id !== undefined) window.dispatchEvent(new CustomEvent('prompts:open', {detail: {id}}));
    } catch { /* not inside Phorminx */ }
  }, 1000);
  void load().then(() => select(null));
})();
