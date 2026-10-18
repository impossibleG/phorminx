// Instruction field: a plain <textarea> where typing "@" suggests prompts
// from the library. Picking one inserts a reference such as @[Short email]
// as ordinary text. The textarea stays the only editable element and the
// source of truth, so undo, paste, select-all, IME and screen readers behave
// as in any text box.
//
// A mirror layer drawn over the textarea repeats its text with the same font,
// padding, wrapping and scroll position, and draws each reference as a chip.
// The textarea's own glyphs are transparent; its caret and selection show
// through the mirror. References behave as single units: the caret never
// lands inside one, a selection that touches one covers all of it, and
// Backspace or Delete next to one removes it whole. Clicking a chip opens the
// prompt's preview. A row of pills under the box gives the same previews to
// the keyboard and screen readers.
(function () {
  'use strict';

  const REFERENCE = /@\[([^\[\]\r\n]{1,80})\]|@\{prompt:(\d+)\}/g;

  // Pure text helpers, shared with instruction-field.test.js.
  const InstructionFieldText = {
    /** Every reference with its range in the text, in order. */
    tokens(text) {
      const found = [];
      for (const match of String(text || '').matchAll(REFERENCE)) {
        const reference = match[1] !== undefined ? {name: match[1].trim()} : {id: Number(match[2])};
        if (reference.name === '' || reference.id === 0) continue;
        found.push({...reference, start: match.index, end: match.index + match[0].length});
      }
      return found;
    },
    /** Distinct references in order: {name} or {id} for old @{prompt:ID} text. */
    references(text) {
      const found = [];
      for (const {start, end, ...reference} of this.tokens(text)) {
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
    /** Rewrites old @{prompt:ID} references to @[Name] for prompts that exist. */
    upgradeLegacy(text, prompts) {
      return String(text || '').replace(/@\{prompt:(\d+)\}/g, (whole, id) => {
        const prompt = prompts.find((entry) => entry.id === Number(id));
        return prompt ? this.token(prompt.name) : whole;
      });
    },
    /** A caret position moved out of any reference: toward `direction`
     *  (-1 start, 1 end) or, without one, to the nearer edge. */
    snap(position, tokens, direction = 0) {
      const inside = tokens.find((token) => position > token.start && position < token.end);
      if (!inside) return position;
      if (direction < 0) return inside.start;
      if (direction > 0) return inside.end;
      return position - inside.start <= inside.end - position ? inside.start : inside.end;
    },
    /** A selection widened so it never covers part of a reference. */
    expand(start, end, tokens) {
      for (const token of tokens) {
        if (token.start < end && token.end > start) {
          start = Math.min(start, token.start);
          end = Math.max(end, token.end);
        }
      }
      return [start, end];
    },
    /** The reference a Backspace or Delete at `position` removes whole, if any. */
    deletion(position, key, tokens) {
      return tokens.find((token) => key === 'Backspace'
        ? position > token.start && position <= token.end
        : position >= token.start && position < token.end) || null;
    },
    /** "@query" being typed just before the caret, or null. */
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
  const MIRRORED = ['boxSizing', 'paddingTop', 'paddingRight', 'paddingBottom', 'paddingLeft', 'borderTopWidth', 'borderRightWidth', 'borderBottomWidth', 'borderLeftWidth', 'fontFamily', 'fontSize', 'fontWeight', 'fontStyle', 'fontVariant', 'letterSpacing', 'lineHeight', 'textTransform', 'textIndent', 'wordSpacing', 'tabSize', 'textAlign', 'direction'];

  async function refreshPrompts() {
    lastRefresh = Date.now();
    try { prompts = (await invoke('prompts_list')) || []; } catch { return; }
    for (const field of fields) field.refresh();
  }
  window.addEventListener('prompts:changed', () => { void refreshPrompts(); });

  function element(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  }

  /** Viewport position just below the caret, measured with a hidden mirror. */
  function caretPoint(textarea) {
    const style = getComputedStyle(textarea);
    const mirror = element('div');
    for (const name of [...MIRRORED, 'width']) mirror.style[name] = style[name];
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
      this.direction = 0;
      this.composing = false;

      this.wrap = element('div', 'instruction-wrap');
      textarea.before(this.wrap);
      this.mirror = element('div', 'instruction-mirror');
      this.mirror.setAttribute('aria-hidden', 'true');
      this.wrap.append(textarea, this.mirror);
      textarea.classList.add('instruction-source');

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
      this.wrap.after(this.pills, this.popover);

      textarea.setAttribute('aria-autocomplete', 'list');
      textarea.setAttribute('aria-controls', this.menu.id);
      textarea.setAttribute('aria-expanded', 'false');

      // Forms fill the box by assigning .value, which fires no event.
      const field = this;
      Object.defineProperty(textarea, 'value', {
        configurable: true,
        get() { return valueProperty.get.call(textarea); },
        set(value) { valueProperty.set.call(textarea, value); field.refresh(); },
      });
      textarea.addEventListener('input', () => { this.renderMirror(); this.renderPills(); this.updateMenu(); });
      textarea.addEventListener('keydown', (event) => this.keydown(event));
      textarea.addEventListener('compositionstart', () => { this.composing = true; });
      textarea.addEventListener('compositionend', () => { this.composing = false; });
      textarea.addEventListener('mousedown', () => { this.direction = 0; });
      textarea.addEventListener('mouseup', (event) => this.mouseup(event));
      textarea.addEventListener('focus', () => { if (Date.now() - lastRefresh > 5000) void refreshPrompts(); });
      textarea.addEventListener('blur', () => setTimeout(() => this.closeMenu(), 120));
      textarea.addEventListener('scroll', () => { this.syncScroll(); this.closeMenu(); });
      document.addEventListener('selectionchange', () => {
        if (document.activeElement === textarea) this.normalizeSelection();
      });
      window.addEventListener('resize', () => this.closeMenu());
      if (window.ResizeObserver) new ResizeObserver(() => this.renderMirror()).observe(textarea);
      this.refresh();
    }

    refresh() {
      this.upgradeLegacy();
      this.renderMirror();
      this.renderPills();
    }

    tokens() { return InstructionFieldText.tokens(valueProperty.get.call(this.textarea)); }

    /** Old @{prompt:ID} text becomes @[Name] once the library is known. It
     *  is saved the next time the form is. */
    upgradeLegacy() {
      if (!prompts.length || document.activeElement === this.textarea) return;
      const value = valueProperty.get.call(this.textarea);
      const upgraded = InstructionFieldText.upgradeLegacy(value, prompts);
      if (upgraded !== value) valueProperty.set.call(this.textarea, upgraded);
    }

    renderMirror() {
      const textarea = this.textarea;
      const style = getComputedStyle(textarea);
      for (const name of MIRRORED) this.mirror.style[name] = style[name];
      // The mirror has no scrollbar; keep its text as wide as the textarea's.
      const borders = parseFloat(style.borderLeftWidth) + parseFloat(style.borderRightWidth);
      const scrollbar = Math.max(0, textarea.offsetWidth - textarea.clientWidth - borders);
      this.mirror.style.paddingRight = `${parseFloat(style.paddingRight) + scrollbar}px`;
      this.mirror.style.left = `${textarea.offsetLeft}px`;
      this.mirror.style.top = `${textarea.offsetTop}px`;
      this.mirror.style.width = `${textarea.offsetWidth}px`;
      this.mirror.style.height = `${textarea.offsetHeight}px`;
      const value = valueProperty.get.call(textarea);
      const parts = [];
      let last = 0;
      for (const token of InstructionFieldText.tokens(value)) {
        if (token.start > last) parts.push(document.createTextNode(value.slice(last, token.start)));
        parts.push(this.chip(token, value.slice(token.start, token.end)));
        last = token.end;
      }
      // A trailing newline needs a character after it to take up a line.
      parts.push(document.createTextNode(`${value.slice(last)}​`));
      this.mirror.replaceChildren(...parts);
      this.syncScroll();
    }

    /** The chip covers exactly the reference's characters, so wrapping and
     *  caret positions match the textarea; the brackets are drawn invisible. */
    chip(token, text) {
      const prompt = InstructionFieldText.find(prompts, token);
      const chip = element('span', prompt ? 'instruction-chip' : 'instruction-chip missing');
      chip.dataset.start = String(token.start);
      if (token.name !== undefined) {
        chip.append(element('span', 'instruction-chip-at', '@'), element('span', 'instruction-chip-hidden', '['), document.createTextNode(text.slice(2, -1)), element('span', 'instruction-chip-hidden', ']'));
      } else {
        chip.textContent = text;
      }
      return chip;
    }

    syncScroll() {
      this.mirror.scrollTop = this.textarea.scrollTop;
      this.mirror.scrollLeft = this.textarea.scrollLeft;
    }

    renderPills() {
      const references = InstructionFieldText.references(valueProperty.get.call(this.textarea));
      // An old @{prompt:ID} and a new @[Name] can name the same prompt.
      const shown = new Set();
      const pills = references.flatMap((reference) => {
        const prompt = InstructionFieldText.find(prompts, reference);
        if (prompt && shown.has(prompt.id)) return [];
        if (prompt) shown.add(prompt.id);
        const label = prompt ? prompt.name : reference.name ?? `#${reference.id}`;
        const pill = element('button', prompt ? 'instruction-pill' : 'instruction-pill missing', prompt ? label : `Missing: ${label}`);
        pill.type = 'button';
        pill.dataset.key = InstructionFieldText.key(reference);
        pill.title = prompt ? 'Show this prompt' : 'No prompt has this name';
        pill.setAttribute('aria-expanded', String(this.preview === pill.dataset.key));
        pill.addEventListener('click', () => this.togglePreview(reference, pill));
        return [pill];
      });
      this.pills.replaceChildren(...(pills.length ? [element('span', 'instruction-pills-label', 'Uses'), ...pills] : []));
      this.pills.hidden = !pills.length;
      if (this.preview && !references.some((reference) => InstructionFieldText.key(reference) === this.preview)) this.closePreview();
    }

    /** Keeps the caret out of references and selections around whole ones. */
    normalizeSelection() {
      if (this.composing) return;
      const textarea = this.textarea;
      const tokens = this.tokens();
      if (!tokens.length) return;
      const {selectionStart: start, selectionEnd: end, selectionDirection} = textarea;
      if (start === end) {
        const snapped = InstructionFieldText.snap(start, tokens, this.direction);
        if (snapped !== start) textarea.setSelectionRange(snapped, snapped);
        return;
      }
      const [from, to] = InstructionFieldText.expand(start, end, tokens);
      if (from !== start || to !== end) textarea.setSelectionRange(from, to, selectionDirection);
    }

    mouseup(event) {
      this.direction = 0;
      this.normalizeSelection();
      const textarea = this.textarea;
      if (textarea.selectionStart !== textarea.selectionEnd) return;
      for (const chip of this.mirror.querySelectorAll('.instruction-chip')) {
        const hit = [...chip.getClientRects()].some((box) => event.clientX >= box.left && event.clientX <= box.right && event.clientY >= box.top && event.clientY <= box.bottom);
        if (!hit) continue;
        const token = this.tokens().find((entry) => entry.start === Number(chip.dataset.start));
        if (!token) return;
        textarea.setSelectionRange(token.end, token.end);
        const reference = token.name !== undefined ? {name: token.name} : {id: token.id};
        const key = InstructionFieldText.key(reference);
        const pill = [...this.pills.querySelectorAll('.instruction-pill')].find((entry) => entry.dataset.key === key);
        this.togglePreview(reference, pill, true);
        return;
      }
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
      if (event.isComposing || this.composing) return;
      if (!this.menu.hidden) {
        if ((event.key === 'ArrowDown' || event.key === 'ArrowUp') && this.matches.length) {
          event.preventDefault();
          const step = event.key === 'ArrowDown' ? 1 : -1;
          this.highlight = (this.highlight + step + this.matches.length) % this.matches.length;
          this.markHighlight();
          return;
        }
        if ((event.key === 'Enter' || event.key === 'Tab') && this.matches.length) {
          event.preventDefault();
          this.insert(this.matches[this.highlight]);
          return;
        }
        if (event.key === 'Escape') {
          event.preventDefault();
          event.stopPropagation();
          this.closeMenu();
          return;
        }
      }
      this.direction = ['ArrowLeft', 'ArrowUp', 'Home', 'PageUp'].includes(event.key) ? -1
        : ['ArrowRight', 'ArrowDown', 'End', 'PageDown'].includes(event.key) ? 1 : 0;
      const textarea = this.textarea;
      if ((event.key === 'Backspace' || event.key === 'Delete') && textarea.selectionStart === textarea.selectionEnd && !event.ctrlKey) {
        const token = InstructionFieldText.deletion(textarea.selectionStart, event.key, this.tokens());
        if (token) {
          event.preventDefault();
          // Through the browser's editing, so Ctrl+Z restores the reference.
          textarea.setSelectionRange(token.start, token.end);
          if (!document.execCommand('delete')) {
            textarea.setRangeText('', token.start, token.end, 'end');
            textarea.dispatchEvent(new Event('input', {bubbles: true}));
          }
        }
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

    togglePreview(reference, pill, keepOpen = false) {
      const key = InstructionFieldText.key(reference);
      if (this.preview === key && !keepOpen) { this.closePreview(); pill?.focus(); return; }
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
      for (const other of this.pills.querySelectorAll('.instruction-pill')) other.setAttribute('aria-expanded', String(other.dataset.key === key));
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
