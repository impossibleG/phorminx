// Messages page: peer-to-peer contacts and conversations over Tor.
// Contacts are shown by name only; addresses and keys never reach the page.
(() => {
  const invoke = window.__TAURI__?.core?.invoke;
  const $ = (id) => document.getElementById(id);
  const el = (tag, className, text) => {
    const element = document.createElement(tag);
    if (className) element.className = className;
    if (text !== undefined) element.textContent = text;
    return element;
  };
  let snapshot = null;
  let revision = 0;
  let selectedId = null;
  let visible = false;
  let conversationKey = '';

  const networkText = {
    disabled: 'Off',
    starting: 'Connecting to Tor…',
    online: 'Online',
    offline: 'Offline · retrying',
  };
  const stateText = {
    sending: 'Sending…',
    waiting: 'Waiting for contact',
    delivered: 'Delivered',
    received: '',
  };

  function result(text) { $('messaging-result').textContent = text || ''; }

  function selected() {
    return snapshot?.contacts.find((contact) => contact.id === selectedId) || null;
  }

  function keyOptions(select, keys, current, noneLabel) {
    const options = [el('option', '', noneLabel)];
    options[0].value = '';
    for (const key of keys) {
      const option = el('option', '', String(key));
      option.value = String(key);
      options.push(option);
    }
    select.replaceChildren(...options);
    select.value = current ? String(current) : '';
  }

  function render() {
    const view = snapshot;
    if (!view) return;
    const status = view.status;
    const enabled = status.enabled;
    $('messaging-network').textContent = enabled ? networkText[status.network] || status.network : '';
    $('messaging-network').dataset.state = status.network;
    $('messaging-off').hidden = enabled;
    $('messaging-on').hidden = !enabled;
    $('messaging-invite').hidden = !enabled;
    $('messaging-add').hidden = !enabled;
    const online = status.network === 'online';
    $('messaging-invite').disabled = !online;
    $('messaging-add').disabled = !online;
    if (!enabled && status.displayName && !$('messaging-name').value) $('messaging-name').value = status.displayName;

    const unread = view.contacts.reduce((sum, contact) => sum + contact.unread, 0);
    $('messages-badge').hidden = unread === 0;
    $('messages-badge').textContent = unread > 99 ? '99+' : String(unread);

    const list = $('messaging-contacts');
    const rows = view.contacts.map((contact) => {
      const row = el('button', 'messaging-contact');
      row.type = 'button';
      row.classList.toggle('active', contact.id === selectedId);
      row.classList.toggle('blocked', contact.blocked);
      const name = el('span', 'messaging-contact-name', contact.name);
      const meta = el('small', '', contact.blocked ? 'Blocked' : contact.connected ? 'Connected' : contact.contactsSlot ? `Key ${contact.contactsSlot}` : '');
      row.append(name, meta);
      if (contact.unread) row.append(el('span', 'nav-badge', String(contact.unread)));
      row.addEventListener('click', () => select(contact.id));
      return row;
    });
    if (!rows.length) rows.push(el('p', 'messaging-none', 'No contacts yet.'));
    list.replaceChildren(...rows);

    const contact = selected();
    if (selectedId !== null && !contact) selectedId = null;
    $('messaging-empty').hidden = Boolean(contact);
    $('messaging-thread').hidden = !contact;
    if (contact) {
      $('messaging-thread-name').textContent = contact.name;
      $('messaging-thread-state').textContent = contact.nickname && contact.nickname !== contact.displayName ? `Calls themself ${contact.displayName}` : '';
      $('messaging-block').textContent = contact.blocked ? 'Unblock' : 'Block';
      $('messaging-text').disabled = contact.blocked;
      $('messaging-dictate').disabled = contact.blocked;
    }
  }

  async function loadConversation(force = false) {
    const contact = selected();
    if (!contact) return;
    const key = `${contact.id}:${revision}`;
    if (!force && key === conversationKey) return;
    conversationKey = key;
    try {
      const messages = await invoke('host_messaging_conversation', {contactId: contact.id});
      if (selectedId !== contact.id) return;
      const log = $('messaging-log');
      const atBottom = log.scrollHeight - log.scrollTop - log.clientHeight < 40;
      log.replaceChildren(...messages.map((message) => {
        const bubble = el('div', `messaging-message ${message.outgoing ? 'outgoing' : 'incoming'}`);
        bubble.append(el('p', '', message.text));
        const time = new Date(message.sentAtMs).toLocaleString([], {dateStyle: 'short', timeStyle: 'short'});
        const state = message.outgoing ? stateText[message.state] : '';
        const meta = el('small', `messaging-state ${message.state}`, state ? `${time} · ${state}` : time);
        bubble.append(meta);
        return bubble;
      }));
      if (atBottom || force) log.scrollTop = log.scrollHeight;
      await focus();
    } catch (error) { result(String(error)); }
  }

  async function focus() {
    const id = visible && document.visibilityState === 'visible' ? selectedId : null;
    try { await invoke('host_messaging_focus', {contactId: id}); } catch (_) { /* best effort */ }
  }

  async function refresh() {
    if (!invoke) return;
    try {
      const next = await invoke('host_messaging_snapshot');
      const changed = !snapshot || next.revision !== revision;
      snapshot = next;
      revision = next.revision;
      if (changed) render();
      if (visible && changed) await loadConversation();
    } catch (error) { result(String(error)); }
  }

  function select(id) {
    selectedId = id;
    conversationKey = '';
    render();
    void loadConversation(true);
    $('messaging-text').focus();
  }

  async function act(kind, extra = {}) {
    const contact = selected();
    if (!contact) return;
    try {
      await invoke('host_messaging_contact', {contactId: contact.id, kind, nickname: null, slot: null, ...extra});
      await refresh();
      await loadConversation(true);
    } catch (error) { result(String(error)); }
  }

  async function send() {
    const contact = selected();
    const text = $('messaging-text').value.trim();
    if (!contact || !text) return;
    try {
      await invoke('host_messaging_send', {contactId: contact.id, textBody: text});
      $('messaging-text').value = '';
      result('');
      await refresh();
      await loadConversation(true);
    } catch (error) { result(String(error)); }
  }

  $('messaging-enable-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    const name = $('messaging-name').value.trim();
    if (!name) return;
    result('Turning on messaging and connecting to Tor…');
    try { await invoke('host_messaging_enable', {name}); result(''); await refresh(); }
    catch (error) { result(String(error)); }
  });

  $('messaging-invite').addEventListener('click', async () => {
    try {
      const invite = await invoke('host_messaging_invite');
      try { await navigator.clipboard.writeText(invite); result('Invite copied. Send it to one person; it works once and expires in 24 hours.'); }
      catch (_) { window.prompt('Copy this invite and send it to one person. It works once and expires in 24 hours.', invite); }
    } catch (error) { result(String(error)); }
  });

  $('messaging-add').addEventListener('click', () => {
    $('messaging-invite-text').value = '';
    $('messaging-add-result').textContent = '';
    $('messaging-add-dialog').showModal();
  });

  $('messaging-add-confirm').addEventListener('click', async (event) => {
    event.preventDefault();
    const invite = $('messaging-invite-text').value.trim();
    if (!invite) return;
    const button = $('messaging-add-confirm');
    button.disabled = true;
    $('messaging-add-result').textContent = 'Connecting over Tor… this can take up to a minute.';
    try {
      const contact = await invoke('host_messaging_redeem', {invite});
      $('messaging-add-dialog').close();
      result(`${contact.name} was added.`);
      await refresh();
      select(contact.id);
    } catch (error) { $('messaging-add-result').textContent = String(error); }
    finally { button.disabled = false; }
  });

  $('messaging-compose').addEventListener('submit', (event) => { event.preventDefault(); void send(); });
  $('messaging-text').addEventListener('keydown', (event) => {
    if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) { event.preventDefault(); void send(); }
  });
  $('messaging-dictate').addEventListener('click', async () => {
    const contact = selected();
    if (!contact) return;
    try { await invoke('host_messaging_dictate', {contactId: contact.id}); }
    catch (error) { result(String(error)); }
  });

  $('messaging-rename').addEventListener('click', () => {
    const contact = selected();
    if (!contact) return;
    const name = window.prompt(`Name for ${contact.displayName} (leave empty to use theirs)`, contact.nickname || '');
    if (name === null) return;
    void act('rename', {nickname: name.trim() || null});
  });
  $('messaging-block').addEventListener('click', () => {
    const contact = selected();
    if (!contact) return;
    if (!contact.blocked && !window.confirm(`Block ${contact.name}? They will not be able to reach you until you unblock them.`)) return;
    void act(contact.blocked ? 'unblock' : 'block');
  });
  $('messaging-clear').addEventListener('click', () => {
    const contact = selected();
    if (contact && window.confirm(`Delete your whole history with ${contact.name} from this computer? Unsent messages are dropped too.`)) void act('delete-history');
  });
  $('messaging-remove').addEventListener('click', async () => {
    const contact = selected();
    if (!contact || !window.confirm(`Remove ${contact.name} and your history with them? You will need a new invite to talk again.`)) return;
    await act('remove');
    selectedId = null;
    render();
  });

  $('messaging-keys').addEventListener('click', () => {
    const contact = selected();
    if (!contact) return;
    $('messaging-keys-title').textContent = contact.name;
    keyOptions($('messaging-contacts-key'), [1, 2, 3, 4, 5, 6, 7, 8, 9], contact.contactsSlot, 'None');
    const mainKeys = snapshot.freeMainKeys.filter((key) => key !== snapshot.launcherSlot);
    keyOptions($('messaging-main-key'), mainKeys, contact.mainSlot, 'Not on the main launcher');
    $('messaging-keys-result').textContent = '';
    $('messaging-keys-dialog').showModal();
  });
  $('messaging-keys-save').addEventListener('click', async (event) => {
    event.preventDefault();
    const contact = selected();
    if (!contact) return;
    const value = (id) => ($(id).value ? Number($(id).value) : null);
    try {
      await invoke('host_messaging_contact', {contactId: contact.id, kind: 'contacts-key', nickname: null, slot: value('messaging-contacts-key')});
      await invoke('host_messaging_contact', {contactId: contact.id, kind: 'promote', nickname: null, slot: value('messaging-main-key')});
      $('messaging-keys-dialog').close();
      await refresh();
    } catch (error) { $('messaging-keys-result').textContent = String(error); }
  });

  $('messaging-settings-open').addEventListener('click', () => {
    $('messaging-self-name').value = snapshot?.status.displayName || '';
    keyOptions($('messaging-launcher-key'), snapshot?.freeMainKeys || [], snapshot?.launcherSlot, 'None');
    $('messaging-settings-result').textContent = '';
    $('messaging-settings-dialog').showModal();
  });
  $('messaging-settings-save').addEventListener('click', async (event) => {
    event.preventDefault();
    try {
      const name = $('messaging-self-name').value.trim();
      if (name && name !== snapshot?.status.displayName) await invoke('host_messaging_set_name', {name});
      const key = $('messaging-launcher-key').value ? Number($('messaging-launcher-key').value) : null;
      await invoke('host_messaging_launcher_key', {slot: key});
      $('messaging-settings-dialog').close();
      await refresh();
    } catch (error) { $('messaging-settings-result').textContent = String(error); }
  });
  $('messaging-turn-off').addEventListener('click', async () => {
    if (!window.confirm('Turn off messaging? Phorminx disconnects from Tor. Contacts and history stay on this computer.')) return;
    try { await invoke('host_messaging_disable'); $('messaging-settings-dialog').close(); await refresh(); }
    catch (error) { $('messaging-settings-result').textContent = String(error); }
  });

  document.addEventListener('visibilitychange', () => { void focus(); });

  window.phorminxMessages = {
    routeChanged(route) {
      visible = route === 'messages';
      if (visible) { void refresh().then(() => loadConversation(true)); }
      else void focus();
    },
  };

  if (invoke) { void refresh(); setInterval(() => { void refresh(); }, 1500); }
})();
