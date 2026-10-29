// Meetings › Upcoming: scheduled meetings with instructions, important items,
// material, a briefing and a prep chat. Material lives in a linked
// investigation, so OCR, voice notes and the assistant are the same ones.
(() => {
  const invoke = window.__TAURI__?.core?.invoke;
  const $ = (id) => document.getElementById(id);
  if (!$('upcoming')) return;

  const KIND_LABELS = {text: 'Text', note: 'Note', image: 'Image', link: 'Link', voice: 'Voice note', answer: 'Pinned answer'};
  const MAX_FILE_BYTES = 25 * 1024 * 1024;
  let plans = [];
  let currentId = null;
  let investigationId = null;
  let items = [];
  let voiceTimer = null;
  let chatTimer = null;
  let deleteArmed = null;

  function status(text, error = false) {
    $('plan-status').textContent = text || '';
    $('plan-status').classList.toggle('error', error);
  }
  const fail = (error) => status(String(error), true);

  async function busy(button, label, work) {
    const original = button.textContent;
    button.disabled = true;
    button.textContent = label;
    button.classList.add('busy');
    try { return await work(); }
    catch (error) { fail(error); return undefined; }
    finally { button.disabled = false; button.textContent = original; button.classList.remove('busy'); }
  }

  // ---------- list ----------

  async function loadPlans() {
    if (!invoke) return;
    try { plans = await invoke('meeting_plans'); }
    catch (error) { status(String(error), true); return; }
    renderList();
    renderNext();
  }

  function renderList() {
    const list = document.createDocumentFragment();
    for (const plan of plans) {
      const button = document.createElement('button');
      button.type = 'button';
      button.classList.toggle('active', plan.id === currentId);
      button.textContent = plan.title || 'Untitled meeting';
      const detail = document.createElement('small');
      const extras = [planWhen(plan.scheduledAtMs)];
      if (plan.items.length) extras.push(`${plan.items.length} item${plan.items.length === 1 ? '' : 's'}`);
      if (plan.briefing) extras.push('briefing');
      detail.textContent = extras.join(' · ');
      button.append(detail);
      button.addEventListener('click', () => { void openPlan(plan.id); });
      list.append(button);
    }
    $('plan-list').replaceChildren(list);
    if (!plans.length) {
      const empty = document.createElement('p');
      empty.className = 'plan-list-empty';
      empty.textContent = 'No upcoming meetings. Schedule one to prepare it.';
      $('plan-list').append(empty);
    }
  }

  // Live page: offer the meeting that starts within the hour.
  function renderNext() {
    const banner = $('live-next');
    if (!banner) return;
    const plan = nextPlan(plans);
    const idle = (window.phorminxMeeting?.capture ?? 'idle') === 'idle';
    banner.hidden = !plan || !idle;
    if (!plan) return;
    banner.dataset.planId = String(plan.id);
    $('live-next-title').textContent = plan.title;
    $('live-next-when').textContent = planWhen(plan.scheduledAtMs);
  }

  // ---------- editor ----------

  function nextHalfHour() {
    const date = new Date(Date.now() + 30 * 60000);
    date.setMinutes(date.getMinutes() < 30 ? 30 : 60, 0, 0);
    return date.getTime();
  }

  function fill(plan) {
    currentId = plan?.id ?? null;
    investigationId = plan?.investigationId ?? null;
    $('plan-title').value = plan?.title ?? '';
    $('plan-when').value = toLocalInput(plan ? plan.scheduledAtMs : nextHalfHour());
    $('plan-reminder').value = plan ? (plan.reminderMinutes === null ? '' : String(plan.reminderMinutes)) : '10';
    if (![...$('plan-reminder').options].some((option) => option.value === $('plan-reminder').value)) {
      $('plan-reminder').append(new Option(`${plan.reminderMinutes} minutes before`, String(plan.reminderMinutes)));
      $('plan-reminder').value = String(plan.reminderMinutes);
    }
    $('plan-instructions').value = plan?.instructions ?? '';
    $('plan-briefing').value = plan?.briefing ?? '';
    items = (plan?.items ?? []).map((item) => ({text: item.text, done: item.done}));
    $('plan-suggestions').replaceChildren();
    renderItems();
    $('plan-empty').hidden = true;
    $('plan-form').hidden = false;
    $('plan-delete').hidden = currentId === null;
    disarmDelete();
    status('');
    renderList();
    void loadMaterials();
  }

  async function openPlan(id) {
    try { fill(await invoke('meeting_plan', {id})); }
    catch (error) { fail(error); void loadPlans(); }
  }

  function renderItems() {
    const list = document.createDocumentFragment();
    items.forEach((item, index) => {
      const row = document.createElement('li');
      row.classList.toggle('done', item.done);
      const check = document.createElement('input');
      check.type = 'checkbox';
      check.checked = item.done;
      check.setAttribute('aria-label', `Covered: ${item.text}`);
      check.addEventListener('change', () => { items[index].done = check.checked; renderItems(); });
      const text = document.createElement('input');
      text.value = item.text;
      text.maxLength = 500;
      text.setAttribute('aria-label', 'Important item');
      text.addEventListener('change', () => { items[index].text = text.value.trim(); if (!items[index].text) items.splice(index, 1); renderItems(); });
      const remove = document.createElement('button');
      remove.type = 'button';
      remove.textContent = '×';
      remove.setAttribute('aria-label', `Remove ${item.text}`);
      remove.addEventListener('click', () => { items.splice(index, 1); renderItems(); });
      row.append(check, text, remove);
      list.append(row);
    });
    $('plan-items').replaceChildren(list);
  }

  function addItem(text) {
    const value = text.trim();
    if (!value || hasItem(items, value)) return false;
    if (items.length >= 20) { status('A meeting can keep up to 20 important items.', true); return false; }
    items.push({text: value, done: false});
    renderItems();
    return true;
  }

  function draft() {
    const reminder = $('plan-reminder').value;
    return {
      title: $('plan-title').value.trim(),
      scheduledAtMs: fromLocalInput($('plan-when').value),
      reminderMinutes: reminder === '' ? null : Number(reminder),
      instructions: $('plan-instructions').value,
      briefing: $('plan-briefing').value,
      items,
    };
  }

  async function save() {
    const plan = draft();
    if (!plan.title) { $('plan-title').focus(); throw new Error('Give the meeting a title first.'); }
    if (plan.reminderMinutes !== null && plan.scheduledAtMs === null) throw new Error('Choose when the meeting starts, or turn the reminder off.');
    currentId = await invoke('meeting_plan_save', {id: currentId, plan});
    $('plan-delete').hidden = false;
    await loadPlans();
    return currentId;
  }

  async function ensureMaterials() {
    await save();
    investigationId = await invoke('meeting_plan_materials', {id: currentId});
    return investigationId;
  }

  // ---------- material and prep chat ----------

  function messageRow(message) {
    const row = document.createElement('div');
    row.className = `plan-message ${message.role === 'user' ? 'from-you' : 'from-assistant'}`;
    const who = document.createElement('small');
    who.textContent = message.role === 'user' ? 'YOU' : 'ASSISTANT';
    const body = document.createElement('div');
    if (message.role === 'user') body.textContent = message.text;
    else {
      body.className = 'markdown';
      body.append(renderMarkdown(message.text || (message.status === 'streaming' ? 'Thinking…' : '')));
    }
    row.append(who, body);
    if (message.status === 'failed') {
      const note = document.createElement('em');
      note.textContent = 'Failed';
      row.append(note);
    }
    return row;
  }

  async function loadMaterials() {
    const materials = $('plan-materials');
    const chat = $('plan-chat');
    if (!investigationId || !invoke) {
      materials.replaceChildren(Object.assign(document.createElement('p'), {className: 'plan-hint', textContent: 'Nothing added yet.'}));
      chat.replaceChildren();
      return null;
    }
    let detail;
    try { detail = await invoke('investigation_detail', {id: investigationId, since: null}); }
    catch (error) { fail(error); return null; }
    if (!detail) { investigationId = null; return loadMaterials(); }
    const cards = document.createDocumentFragment();
    for (const item of detail.items) {
      const card = document.createElement('div');
      card.className = 'plan-material';
      const head = document.createElement('div');
      const label = document.createElement('small');
      label.textContent = KIND_LABELS[item.kind] || item.kind;
      const title = document.createElement('strong');
      title.textContent = item.title || item.linkTitle || item.url || '';
      const remove = document.createElement('button');
      remove.type = 'button';
      remove.textContent = 'Remove';
      remove.addEventListener('click', async () => {
        try { await invoke('investigation_delete_item', {itemId: item.id}); await loadMaterials(); }
        catch (error) { fail(error); }
      });
      head.append(label, title, remove);
      const body = document.createElement('p');
      body.textContent = item.ocrState === 'pending' ? 'Reading the text in this image…'
        : item.ocrState === 'failed' ? 'No text could be read from this image.'
        : (item.body || '').slice(0, 220) + ((item.body || '').length > 220 ? '…' : '');
      card.append(head, body);
      cards.append(card);
    }
    materials.replaceChildren(cards);
    if (!detail.items.length) materials.append(Object.assign(document.createElement('p'), {className: 'plan-hint', textContent: 'Nothing added yet.'}));
    const follow = chat.scrollHeight - chat.scrollTop - chat.clientHeight < 60;
    chat.replaceChildren(...detail.messages.map(messageRow));
    if (follow) chat.scrollTop = chat.scrollHeight;
    // OCR runs in the background; look again until every image has text.
    if (detail.items.some((item) => item.ocrState === 'pending')) setTimeout(() => { if (!$('plan-form').hidden) void loadMaterials(); }, 2000);
    return detail;
  }

  function readFile(file) {
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve(reader.result);
      reader.onerror = () => reject(new Error(`${file.name} could not be read.`));
      reader.readAsDataURL(file);
    });
  }

  async function addFiles(files) {
    for (const file of files) {
      if (file.size > MAX_FILE_BYTES) { status(`${file.name} is larger than 25 MB.`, true); continue; }
      status(`Adding ${file.name}…`);
      try {
        await save();
        const data = await readFile(file);
        const parts = await invoke('meeting_plan_add_document', {id: currentId, name: file.name, data});
        investigationId = (await invoke('meeting_plan', {id: currentId})).investigationId;
        status(parts > 1 ? `Added ${file.name} in ${parts} parts.` : `Added ${file.name}.`);
      } catch (error) { fail(`${file.name}: ${error}`); }
      await loadMaterials();
    }
  }

  async function toggleVoice() {
    const button = $('plan-voice');
    try {
      if (voiceTimer) {
        await invoke('investigation_voice', {kind: 'stop', id: investigationId, parentNode: null});
        button.textContent = 'Saving…';
        return;
      }
      const id = await ensureMaterials();
      const voice = await invoke('investigation_voice', {kind: 'start', id, parentNode: null});
      button.textContent = 'Stop voice note';
      button.classList.add('recording');
      status(voice.notice || 'Listening. Only the recognized text is kept; no audio is stored.');
      voiceTimer = setInterval(async () => {
        let view;
        try { view = await invoke('investigation_voice', {kind: 'status', id: null, parentNode: null}); }
        catch (_) { return; }
        if (view.state !== 'idle') { if (view.text) status(`Hearing: ${view.text.slice(-160)}`); return; }
        clearInterval(voiceTimer);
        voiceTimer = null;
        button.textContent = 'Record a voice note';
        button.classList.remove('recording');
        status(view.notice || 'Voice note added.');
        await loadMaterials();
      }, 700);
    } catch (error) { fail(error); }
  }

  async function ask() {
    const input = $('plan-question');
    const text = input.value.trim();
    if (!text) return;
    await busy($('plan-ask'), 'Sending…', async () => {
      const id = await ensureMaterials();
      await invoke('investigation_assistant', {kind: 'ask', id, text, messageId: null});
      input.value = '';
      status('');
      await followChat();
    });
  }

  // Polls the prep chat while an answer streams in.
  async function followChat() {
    clearTimeout(chatTimer);
    const detail = await loadMaterials();
    if (detail?.messages.some((message) => message.status === 'streaming') && !$('plan-form').hidden) {
      chatTimer = setTimeout(() => { void followChat(); }, 600);
    }
  }

  // ---------- actions ----------

  function disarmDelete() {
    clearTimeout(deleteArmed);
    deleteArmed = null;
    $('plan-delete').textContent = 'Delete';
  }

  $('plan-new').addEventListener('click', () => { fill(null); $('plan-title').focus(); });
  $('plan-form').addEventListener('submit', (event) => {
    event.preventDefault();
    void busy($('plan-save'), 'Saving…', async () => { await save(); status('Saved.'); });
  });
  $('plan-item-add').addEventListener('click', () => {
    if (addItem($('plan-item-text').value)) $('plan-item-text').value = '';
  });
  $('plan-item-text').addEventListener('keydown', (event) => {
    if (event.key !== 'Enter') return;
    event.preventDefault();
    if (addItem($('plan-item-text').value)) $('plan-item-text').value = '';
  });
  $('plan-suggest').addEventListener('click', () => {
    void busy($('plan-suggest'), 'Thinking…', async () => {
      await save();
      const suggestions = await invoke('meeting_plan_suggest_items', {id: currentId});
      const chips = suggestions.map((text) => {
        const chip = document.createElement('button');
        chip.type = 'button';
        chip.className = 'pin-suggestion';
        chip.textContent = `+ ${text}`;
        chip.addEventListener('click', () => { if (addItem(text)) chip.remove(); });
        return chip;
      });
      $('plan-suggestions').replaceChildren(...chips);
      status('Click a suggestion to add it, then Save.');
    });
  });
  $('plan-briefing-write').addEventListener('click', () => {
    void busy($('plan-briefing-write'), 'Writing…', async () => {
      await save();
      $('plan-briefing').value = await invoke('meeting_plan_briefing', {id: currentId});
      status('Briefing written and saved. Edit it freely.');
      await loadPlans();
    });
  });
  $('plan-material-add').addEventListener('click', () => {
    const text = $('plan-material-text').value.trim();
    if (!text) { status('Paste or type some text first.', true); return; }
    void busy($('plan-material-add'), 'Adding…', async () => {
      const id = await ensureMaterials();
      await invoke('investigation_add_text', {id, text, note: true, parentNode: null});
      $('plan-material-text').value = '';
      status('Added.');
      await loadMaterials();
    });
  });
  $('plan-files').addEventListener('change', (event) => {
    const files = [...event.target.files];
    event.target.value = '';
    void addFiles(files);
  });
  $('plan-voice').addEventListener('click', () => { void toggleVoice(); });
  $('plan-ask').addEventListener('click', () => { void ask(); });
  $('plan-question').addEventListener('keydown', (event) => { if (event.key === 'Enter') { event.preventDefault(); void ask(); } });
  $('plan-start').addEventListener('click', () => {
    void busy($('plan-start'), 'Starting…', async () => {
      await save();
      await invoke('meeting_plan_start', {id: currentId});
      if (typeof showRoute === 'function') showRoute('live');
    });
  });
  $('plan-delete').addEventListener('click', () => {
    if (currentId === null) return;
    if (!deleteArmed) {
      $('plan-delete').textContent = 'Click again to delete';
      deleteArmed = setTimeout(disarmDelete, 4000);
      return;
    }
    disarmDelete();
    void busy($('plan-delete'), 'Deleting…', async () => {
      await invoke('meeting_plan_delete', {id: currentId});
      currentId = null;
      investigationId = null;
      $('plan-form').hidden = true;
      $('plan-empty').hidden = false;
      await loadPlans();
    });
  });

  $('live-next-start')?.addEventListener('click', async () => {
    const id = Number($('live-next').dataset.planId);
    try { await invoke('meeting_plan_start', {id}); $('live-next').hidden = true; }
    catch (error) { const status = $('status'); if (status) status.textContent = String(error); }
  });
  $('live-next-open')?.addEventListener('click', () => {
    const id = Number($('live-next').dataset.planId);
    if (typeof showRoute === 'function') showRoute('upcoming');
    void openPlan(id);
  });

  document.addEventListener('phorminx:route', (event) => {
    if (event.detail === 'upcoming' || event.detail === 'live') void loadPlans();
  });
  if (invoke) {
    void loadPlans();
    setInterval(() => { renderNext(); }, 5000);
    setInterval(() => { void loadPlans(); }, 60000);
  }
})();
