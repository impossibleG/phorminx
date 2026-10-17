// Instruction field: a text box where "@" inserts a prompt from the library
// as a chip. The underlying <textarea> keeps the stored text, with each chip
// written as @{prompt:ID}, so existing forms keep reading and writing
// `.value` unchanged. Chips resolve to the prompt's latest text when used.
(function () {
  'use strict';

  const TOKEN = /@\{prompt:(\d+)\}/g;

  // Pure text helpers, shared with instruction-field.test.js.
  const InstructionFieldText = {
    parse(text) {
      const parts = [];
      let last = 0;
      const value = String(text || '');
      for (const match of value.matchAll(TOKEN)) {
        if (match.index > last) parts.push({type: 'text', value: value.slice(last, match.index)});
        parts.push({type: 'prompt', id: Number(match[1])});
        last = match.index + match[0].length;
      }
      if (last < value.length) parts.push({type: 'text', value: value.slice(last)});
      return parts;
    },
    serialize(parts) {
      return parts.map((part) => part.type === 'prompt' ? `@{prompt:${part.id}}` : part.value).join('');
    },
    // "@que" immediately before the caret, or null.
    mentionQuery(textBeforeCaret) {
      const match = /(^|\s)@([^\s@]{0,40})$/.exec(textBeforeCaret);
      return match ? match[2] : null;
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
  const valueProperty = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value');

  async function refreshPrompts() {
    try { prompts = (await invoke('prompts_list')) || []; } catch { return; }
    for (const field of fields) field.render();
  }
  window.addEventListener('prompts:changed', () => { void refreshPrompts(); });

  function element(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  }

  class InstructionField {
    constructor(textarea) {
      this.textarea = textarea;
      this.editor = element('div', 'instruction-field');
      this.editor.contentEditable = 'true';
      this.editor.spellcheck = true;
      this.editor.setAttribute('role', 'textbox');
      this.editor.setAttribute('aria-multiline', String(textarea.tagName === 'TEXTAREA' && textarea.rows !== 1));
      this.editor.dataset.placeholder = textarea.placeholder || 'Type instructions, or @ to insert a prompt';
      if (textarea.rows) this.editor.style.minHeight = `${Math.max(1, textarea.rows) * 1.6 + 1}em`;
      const label = textarea.closest('label');
      if (label) this.editor.setAttribute('aria-label', label.firstChild?.textContent?.trim() || 'Instructions');
      textarea.classList.add('instruction-field-source');
      textarea.after(this.editor);
      this.menu = element('div', 'instruction-menu');
      this.menu.hidden = true;
      this.menu.setAttribute('role', 'listbox');
      this.editor.after(this.menu);
      this.popover = element('div', 'instruction-popover');
      this.popover.hidden = true;
      this.menu.after(this.popover);
      this.matches = [];
      this.highlight = 0;

      const field = this;
      Object.defineProperty(textarea, 'value', {
        configurable: true,
        get() { return valueProperty.get.call(textarea); },
        set(value) { valueProperty.set.call(textarea, value); field.render(); },
      });
      // Labels focus their textarea; move that focus into the visible editor.
      textarea.addEventListener('focus', () => this.editor.focus());
      this.editor.addEventListener('input', () => { this.sync(); this.updateMenu(); });
      this.editor.addEventListener('keydown', (event) => this.keydown(event));
      this.editor.addEventListener('paste', (event) => {
        event.preventDefault();
        document.execCommand('insertText', false, event.clipboardData.getData('text/plain'));
      });
      this.editor.addEventListener('click', (event) => {
        const chip = event.target.closest('.prompt-chip');
        if (chip) this.showPopover(chip); else this.popover.hidden = true;
      });
      this.editor.addEventListener('blur', () => setTimeout(() => {
        if (!this.menu.contains(document.activeElement)) this.menu.hidden = true;
      }, 150));
      document.addEventListener('mousedown', (event) => {
        if (!this.popover.contains(event.target) && !event.target.closest?.('.prompt-chip')) this.popover.hidden = true;
      });
      this.render();
    }

    render() {
      if (document.activeElement === this.editor && this.rendered === this.textarea.value) return;
      const fragment = document.createDocumentFragment();
      for (const part of InstructionFieldText.parse(this.textarea.value)) {
        fragment.append(part.type === 'prompt' ? this.chip(part.id) : document.createTextNode(part.value));
      }
      this.editor.replaceChildren(fragment);
      this.rendered = this.textarea.value;
      this.editor.classList.toggle('is-empty', !this.textarea.value);
    }

    chip(id) {
      const prompt = prompts.find((entry) => entry.id === id);
      const chip = element('span', prompt ? 'prompt-chip' : 'prompt-chip missing', prompt ? prompt.name : 'Missing prompt');
      chip.contentEditable = 'false';
      chip.dataset.promptId = String(id);
      chip.tabIndex = 0;
      chip.setAttribute('role', 'button');
      chip.title = prompt ? 'Prompt from your library. Click to read it.' : 'This prompt was deleted.';
      chip.addEventListener('keydown', (event) => {
        if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); this.showPopover(chip); }
      });
      return chip;
    }

    // DOM back to stored text: chips become references, line breaks stay.
    sync() {
      const parts = [];
      const walk = (node) => {
        for (const child of node.childNodes) {
          if (child.nodeType === Node.TEXT_NODE) parts.push({type: 'text', value: child.nodeValue});
          else if (child.dataset?.promptId) parts.push({type: 'prompt', id: Number(child.dataset.promptId)});
          else if (child.nodeName === 'BR') parts.push({type: 'text', value: '\n'});
          else {
            if (child.nodeName === 'DIV' && parts.length) parts.push({type: 'text', value: '\n'});
            walk(child);
          }
        }
      };
      walk(this.editor);
      const value = InstructionFieldText.serialize(parts);
      valueProperty.set.call(this.textarea, value);
      this.rendered = value;
      this.editor.classList.toggle('is-empty', !value);
      this.textarea.dispatchEvent(new Event('input', {bubbles: true}));
    }

    caretText() {
      const selection = window.getSelection();
      if (!selection.rangeCount || !this.editor.contains(selection.anchorNode)) return null;
      const node = selection.anchorNode;
      if (node.nodeType !== Node.TEXT_NODE) return null;
      return {node, offset: selection.anchorOffset, before: node.nodeValue.slice(0, selection.anchorOffset)};
    }

    updateMenu() {
      const caret = this.caretText();
      const query = caret ? InstructionFieldText.mentionQuery(caret.before) : null;
      if (query === null) { this.menu.hidden = true; return; }
      this.matches = InstructionFieldText.filter(prompts, query);
      this.highlight = 0;
      const items = this.matches.map((prompt, index) => {
        const item = element('button', index === 0 ? 'active' : '');
        item.type = 'button';
        item.setAttribute('role', 'option');
        item.append(element('strong', '', prompt.name), element('span', '', prompt.body.slice(0, 90)));
        item.addEventListener('mousedown', (event) => { event.preventDefault(); this.insert(prompt); });
        return item;
      });
      if (!items.length) items.push(element('p', 'instruction-menu-empty', prompts.length ? 'No prompt matches.' : 'No prompts yet. Create them under Prompts.'));
      this.menu.replaceChildren(...items);
      this.menu.hidden = false;
    }

    keydown(event) {
      if (!this.menu.hidden && this.matches.length) {
        if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
          event.preventDefault();
          const step = event.key === 'ArrowDown' ? 1 : -1;
          this.highlight = (this.highlight + step + this.matches.length) % this.matches.length;
          [...this.menu.children].forEach((item, index) => item.classList.toggle('active', index === this.highlight));
          return;
        }
        if (event.key === 'Enter' || event.key === 'Tab') {
          event.preventDefault();
          this.insert(this.matches[this.highlight]);
          return;
        }
      }
      if (event.key === 'Escape' && !this.menu.hidden) { event.preventDefault(); this.menu.hidden = true; return; }
      if (event.key === 'Enter' && this.textarea.rows !== 1) {
        event.preventDefault();
        document.execCommand('insertText', false, '\n');
      } else if (event.key === 'Enter') {
        event.preventDefault();
      }
    }

    insert(prompt) {
      const caret = this.caretText();
      if (!caret) return;
      const query = InstructionFieldText.mentionQuery(caret.before) ?? '';
      const start = caret.offset - query.length - 1;
      const range = document.createRange();
      range.setStart(caret.node, start);
      range.setEnd(caret.node, caret.offset);
      range.deleteContents();
      const chip = this.chip(prompt.id);
      const space = document.createTextNode(' ');
      range.insertNode(space);
      range.insertNode(chip);
      const selection = window.getSelection();
      selection.removeAllRanges();
      const after = document.createRange();
      after.setStart(space, 1);
      selection.addRange(after);
      this.menu.hidden = true;
      this.sync();
    }

    showPopover(chip) {
      const id = Number(chip.dataset.promptId);
      const prompt = prompts.find((entry) => entry.id === id);
      const children = [];
      if (prompt) {
        children.push(element('small', '', 'PROMPT'), element('strong', '', prompt.name));
        if (prompt.tags.length) children.push(element('span', 'instruction-popover-tags', prompt.tags.map((tag) => `#${tag}`).join(' ')));
        children.push(element('p', 'instruction-popover-body', prompt.body));
        const edit = element('button', '', 'Edit in Prompts');
        edit.type = 'button';
        edit.addEventListener('click', () => {
          this.popover.hidden = true;
          window.dispatchEvent(new CustomEvent('prompts:open', {detail: {id}}));
          void invoke('prompt_open', {id});
        });
        children.push(edit);
      } else {
        children.push(element('small', '', 'MISSING PROMPT'), element('p', 'instruction-popover-body', 'This prompt was deleted, so it adds nothing to these instructions. Remove the chip or insert another prompt with @.'));
      }
      const remove = element('button', '', 'Remove from field');
      remove.type = 'button';
      remove.addEventListener('click', () => { chip.remove(); this.popover.hidden = true; this.sync(); });
      children.push(remove);
      this.popover.replaceChildren(...children);
      this.popover.hidden = false;
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
  setInterval(() => { void refreshPrompts(); }, 15000);
})();
