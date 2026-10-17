// Instruction field: a plain <textarea> where typing "@" suggests prompts
// from the library. Picking one inserts a reference such as @[Short email]
// as ordinary text, so undo, copy and paste, selection, IME and screen
// readers behave exactly as in any text box. A row of pills under the box
// lists the prompts the text refers to; a pill opens a preview, and a pill
// for a prompt that no longer exists is marked missing.
(function () {
  'use strict';

  const REFERENCE = /@\[([^\[\]\r\n]{1,80})\]|@\{prompt:(\d+)\}/g;

  // Pure text helpers, shared with instruction-field.test.js.
  const InstructionFieldText = {
    /** Distinct references in order: {name} or {id} for old @{prompt:ID} text. */
    references(text) {
      const found = [];
      for (const match of String(text || '').matchAll(REFERENCE)) {
        const reference = match[1] !== undefined ? {name: match[1].trim()} : {id: Number(match[2])};
        if (reference.name === '' || reference.id === 0) continue;
        const key = this.key(reference);
        if (!found.some((known) => this.key(known) === key)) found.push(reference);
      }
      return found;
    },
    key(reference) { return reference.name !== undefined ? `n:${reference.name.toLocaleLowerCase()}` : `i:${reference.id}`; },
    find(prompts, reference) {
      return prompts.find((prompt) => reference.name !== undefined
        ? prompt.name.toLocaleLowerCase() === reference.name.toLocaleLowerCase()
        : prompt.id === reference.id);
    },
    token(name) { return `@[${name.trim()}]`; },
    /** The "@query" being typed just before the caret, or null. */
    mention(textBeforeCaret) {
      const match = /(^|\s)@([^\s@\[\]]{0,40})$/.exec(textBeforeCaret);
      return match ? {query: match[2], start: textBeforeCaret.length - match[2].length - 1} : null;
    },
    filter(prompts, query) {
      const needle = String(query || '').toLocaleLowerCase();
      return prompts
        .filter((prompt) => !needle
          || prompt.name.toLocaleLowerCase().includes(needle)
          || prompt.tags.some((tag) => tag.includes(needle)))
        .slice(0, 8);
    },
  };
  window.InstructionFieldText = InstructionFieldText;
  if (typeof document === 'undefined' || !document.querySelectorAll) return;

  const invoke = (command, args) => window.__TAURI__?.core?.invoke(command, args);
  const fields = [];
  let prompts = [];
  let lastRefresh = 0;
  let sequence = 0;
  const valueProperty = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value');

  async function refreshPrompts() {
    lastRefresh = Date.now();
    try { prompts = (await invoke('prompts_list')) || []; } catch { return; }
    for (const field of fields) field.renderPills();
  }
  window.addEventListener('prompts:changed', () => { void refreshPrompts(); });

  function element(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  }

  /** Viewport position of the caret, measured with a mirror of the textarea. */
  function caretPoint(textarea) {
    const style = getComputedStyle(textarea);
    const mirror = element('div');
    for (const name of ['boxSizing', 'width', 'paddingTop', 'paddingRight', 'paddingBottom', 'paddingLeft', 'borderTopWidth', 'borderRightWidth', 'borderBottomWidth', 'borderLeftWidth', 'fontFamily', 'fontSize', 'fontWeight', 'fontStyle', 'letterSpacing', 'lineHeight', 'textTransform', 'wordSpacing', 'tabSize']) {
      mirror.style[name] = style[name];
    }
    Object.assign(mirror.style, {position: 'fixed', visibility: 'hidden', top: '0', left: '-9999px', whiteSpace: 'pre-wrap', overflowWrap: 'break-word', borderStyle: 'solid'});
    mirror.textContent = textarea.value.slice(0, textarea.selectionStart);
    const marker = element('span', '', '​');
    mirror.append(marker);
    document.body.append(mirror);
    const lineHeight = parseFloat(style.lineHeight) || parseFloat(style.fontSize) * 1.4;
    const box = textarea.getBoundingClientRect();
    const point = {
      left: box.left + marker.offsetLeft - textarea.scrollLeft,
      top: box.top + marker.offsetTop - textarea.scrollTop + lineHeight,
    };
    mirror.remove();
    return point;
  }

  class InstructionField {
    constructor(textarea) {
      this.textarea = textarea;
      this.id = `instruction-field-${++sequence}`;
      this.matches = [];
      this.highlight = 0;
      this.mention = null;
      this.preview = null;

      this.menu = element('div', 'instruction-menu');
      this.menu.id = `${this.id}-menu`;
      this.menu.hidden = true;
      this.menu.setAttribute('role', 'listbox');
      this.menu.setAttribute('aria-label', 'Prompts');
      document.body.append(this.menu);

      this.pills = element('div', 'instruction-pills');
      this.pills.setAttribute('aria-label', 'Prompts used');
      this.pills.hidden = true;
      this.popover = element('div', 'instruction-popover');
      this.popover.hidden = true;
      this.popover.setAttribute('role', 'region');
      this.popover.setAttribute('aria-label', 'Prompt preview');
      textarea.after(this.pills, this.popover);

      textarea.setAttribute('aria-autocomplete', 'list');
      textarea.setAttribute('aria-controls', this.menu.id);
      textarea.setAttribute('aria-expanded', 'false');

      // Forms fill the box by assigning .value, which fires no event.
      const field = this;
      Object.defineProperty(textarea, 'value', {
        configurable: true,
        get() { return valueProperty.get.call(textarea); },
        set(value) { valueProperty.set.call(textarea, value); field.renderPills(); },
      });
      textarea.addEventListener('input', () => { this.renderPills(); this.updateMenu(); });
      textarea.addEventListener('keydown', (event) => this.keydown(event));
      textarea.addEventListener('click', () => this.updateMenu());
      textarea.addEventListener('focus', () => { if (Date.now() - lastRefresh > 5000) void refreshPrompts(); });
      textarea.addEventListener('blur', () => setTimeout(() => this.closeMenu(), 120));
      textarea.addEventListener('scroll', () => this.closeMenu());
      window.addEventListener('resize', () => this.closeMenu());
      this.renderPills();
    }

    renderPills() {
      const references = InstructionFieldText.references(valueProperty.get.call(this.textarea));
      const pills = references.map((reference) => {
        const prompt = InstructionFieldText.find(prompts, reference);
        const label = prompt ? prompt.name : reference.name ?? `#${reference.id}`;
        const pill = element('button', prompt ? 'instruction-pill' : 'instruction-pill missing', prompt ? label : `Missing: ${label}`);
        pill.type = 'button';
        pill.title = prompt ? 'Show this prompt' : 'No prompt has this name';
        pill.setAttribute('aria-expanded', String(this.preview === InstructionFieldText.key(reference)));
        pill.addEventListener('click', () => this.togglePreview(reference, pill));
        return pill;
      });
      this.pills.replaceChildren(...(pills.length ? [element('span', 'instruction-pills-label', 'Uses'), ...pills] : []));
      this.pills.hidden = !pills.length;
      if (this.preview && !references.some((reference) => InstructionFieldText.key(reference) === this.preview)) this.closePreview();
    }

    updateMenu() {
      const textarea = this.textarea;
      if (document.activeElement !== textarea || textarea.selectionStart !== textarea.selectionEnd) { this.closeMenu(); return; }
      this.mention = InstructionFieldText.mention(textarea.value.slice(0, textarea.selectionStart));
      if (!this.mention) { this.closeMenu(); return; }
      this.matches = InstructionFieldText.filter(prompts, this.mention.query);
      this.highlight = 0;
      const items = this.matches.map((prompt, index) => {
        const item = element('div', 'instruction-option');
        item.id = `${this.id}-option-${index}`;
        item.setAttribute('role', 'option');
        item.append(element('strong', '', prompt.name), element('span', '', prompt.body.slice(0, 90)));
        // mousedown keeps focus (and the caret) in the textarea.
        item.addEventListener('mousedown', (event) => { event.preventDefault(); this.insert(prompt); });
        return item;
      });
      if (!items.length) items.push(element('p', 'instruction-menu-empty', prompts.length ? 'No prompt matches.' : 'No prompts yet. Create them under Prompts.'));
      this.menu.replaceChildren(...items);
      this.markHighlight();
      const point = caretPoint(textarea);
      this.menu.hidden = false;
      const width = Math.min(320, window.innerWidth - 16);
      const below = point.top + 4;
      const height = this.menu.offsetHeight;
      this.menu.style.width = `${width}px`;
      this.menu.style.left = `${Math.max(8, Math.min(point.left, window.innerWidth - width - 8))}px`;
      this.menu.style.top = `${below + height > window.innerHeight - 8 ? Math.max(8, point.top - height - 24) : below}px`;
      textarea.setAttribute('aria-expanded', 'true');
    }

    markHighlight() {
      [...this.menu.querySelectorAll('[role="option"]')].forEach((item, index) => {
        const active = index === this.highlight;
        item.classList.toggle('active', active);
        item.setAttribute('aria-selected', String(active));
        if (active) item.scrollIntoView({block: 'nearest'});
      });
      if (this.matches.length) this.textarea.setAttribute('aria-activedescendant', `${this.id}-option-${this.highlight}`);
      else this.textarea.removeAttribute('aria-activedescendant');
    }

    closeMenu() {
      if (this.menu.hidden) return;
      this.menu.hidden = true;
      this.textarea.setAttribute('aria-expanded', 'false');
      this.textarea.removeAttribute('aria-activedescendant');
    }

    keydown(event) {
      if (this.menu.hidden || event.isComposing) return;
      if ((event.key === 'ArrowDown' || event.key === 'ArrowUp') && this.matches.length) {
        event.preventDefault();
        const step = event.key === 'ArrowDown' ? 1 : -1;
        this.highlight = (this.highlight + step + this.matches.length) % this.matches.length;
        this.markHighlight();
      } else if ((event.key === 'Enter' || event.key === 'Tab') && this.matches.length) {
        event.preventDefault();
        this.insert(this.matches[this.highlight]);
      } else if (event.key === 'Escape') {
        event.preventDefault();
        event.stopPropagation();
        this.closeMenu();
      }
    }

    /** Replaces the typed "@query" with the reference, through the browser's
     *  own editing so the change can be undone with Ctrl+Z. */
    insert(prompt) {
      const textarea = this.textarea;
      const mention = this.mention;
      if (!mention) return;
      textarea.focus();
      textarea.setSelectionRange(mention.start, textarea.selectionStart);
      const text = `${InstructionFieldText.token(prompt.name)} `;
      if (!document.execCommand('insertText', false, text)) {
        textarea.setRangeText(text, mention.start, textarea.selectionEnd, 'end');
        textarea.dispatchEvent(new Event('input', {bubbles: true}));
      }
      this.closeMenu();
    }

    togglePreview(reference, pill) {
      const key = InstructionFieldText.key(reference);
      if (this.preview === key) { this.closePreview(); pill.focus(); return; }
      this.preview = key;
      const prompt = InstructionFieldText.find(prompts, reference);
      const children = [];
      if (prompt) {
        children.push(element('small', '', 'PROMPT'), element('strong', '', prompt.name));
        if (prompt.tags.length) children.push(element('span', 'instruction-popover-tags', prompt.tags.map((tag) => `#${tag}`).join(' ')));
        children.push(element('p', 'instruction-popover-body', prompt.body));
        const edit = element('button', '', 'Edit in Prompts');
        edit.type = 'button';
        edit.addEventListener('click', () => {
          this.closePreview();
          window.dispatchEvent(new CustomEvent('prompts:open', {detail: {id: prompt.id}}));
          void invoke('prompt_open', {id: prompt.id});
        });
        children.push(edit);
      } else {
        const label = reference.name ?? `#${reference.id}`;
        children.push(element('small', '', 'MISSING PROMPT'), element('p', 'instruction-popover-body', `No prompt is named “${label}”, so this reference adds nothing. Create a prompt with that name, or replace the reference by typing @.`));
      }
      const close = element('button', '', 'Close');
      close.type = 'button';
      close.addEventListener('click', () => { this.closePreview(); this.textarea.focus(); });
      children.push(close);
      this.popover.replaceChildren(...children);
      this.popover.hidden = false;
      for (const other of this.pills.querySelectorAll('.instruction-pill')) other.setAttribute('aria-expanded', String(other === pill));
    }

    closePreview() {
      this.preview = null;
      this.popover.hidden = true;
      for (const pill of this.pills.querySelectorAll('.instruction-pill')) pill.setAttribute('aria-expanded', 'false');
    }
  }

  function upgrade(root = document) {
    for (const textarea of root.querySelectorAll('textarea[data-instruction-field]')) {
      if (!textarea.instructionField) {
        textarea.instructionField = new InstructionField(textarea);
        fields.push(textarea.instructionField);
      }
    }
  }
  window.InstructionField = {upgrade, refreshPrompts};
  upgrade();
  void refreshPrompts();
})();
