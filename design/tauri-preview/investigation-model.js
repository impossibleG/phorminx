// Pure helpers for the Investigations tab: labels, citations, filtering and
// diagram geometry. No DOM access, so `investigation-model.test.js` can run
// them under Node.

const INV_KINDS = {
  text: 'Pasted text',
  note: 'Note',
  image: 'Screenshot',
  link: 'Link',
  voice: 'Voice note',
  answer: 'Pinned answer',
};

const INV_NODE_WIDTH = 228;
const INV_ZOOM_MIN = 0.25;
const INV_ZOOM_MAX = 2.5;

function invKindLabel(kind) {
  return INV_KINDS[kind] || 'Card';
}

// Mirrors `card_label` in investigations.rs so exports and the UI agree.
function invCardLabel(item) {
  const firstLine = (item.body || '').split('\n').find((line) => line.trim()) || '';
  const source = (item.title || '').trim() || item.linkTitle || item.url || firstLine;
  let label = source.split(/\s+/).filter(Boolean).join(' ');
  if ([...label].length > 80) label = `${[...label].slice(0, 79).join('')}…`;
  if (label) return label;
  return {image: 'Screenshot', voice: 'Voice note', link: 'Link'}[item.kind] || 'Card';
}

// Splits assistant text into plain segments and `[#12, #13]` citations.
function invSplitCitations(text) {
  const segments = [];
  const pattern = /\[(\s*#\d+\s*(?:,\s*#\d+\s*)*)\]/g;
  let cursor = 0;
  for (const match of String(text || '').matchAll(pattern)) {
    if (match.index > cursor) segments.push({text: text.slice(cursor, match.index)});
    const ids = match[1].split(',').map((part) => Number(part.trim().slice(1))).filter((id) => Number.isSafeInteger(id) && id > 0);
    segments.push({cite: ids, raw: match[0]});
    cursor = match.index + match[0].length;
  }
  if (cursor < String(text || '').length) segments.push({text: String(text).slice(cursor)});
  return segments;
}

function invStartOfDay(ms) {
  const date = new Date(ms);
  date.setHours(0, 0, 0, 0);
  return date.getTime();
}

function invDayLabel(ms, nowMs = Date.now(), locale = undefined) {
  const day = invStartOfDay(ms);
  const today = invStartOfDay(nowMs);
  if (day === today) return 'Today';
  if (today - day <= 26 * 3600 * 1000 && today > day) return 'Yesterday';
  return new Intl.DateTimeFormat(locale, {weekday: 'short', day: 'numeric', month: 'short', year: new Date(ms).getFullYear() === new Date(nowMs).getFullYear() ? undefined : 'numeric'}).format(new Date(ms));
}

// `matches` is null (no search) or the item ids the host returned.
function invVisibleItems(items, kind, matches) {
  return items.filter((item) => (kind === 'all' || item.kind === kind) && (matches === null || matches.has(item.id)));
}

function invNodeHeight(node, item) {
  if (!item) return 58;
  if (item.kind === 'image' && item.imageSha) return 164;
  if (item.kind === 'link' && item.imageSha) return 150;
  return 96;
}

// Viewport transform that shows every node with a margin.
function invFitView(boxes, width, height) {
  if (!boxes.length || width <= 0 || height <= 0) return {x: 40, y: 40, k: 1};
  const left = Math.min(...boxes.map((box) => box.x));
  const top = Math.min(...boxes.map((box) => box.y));
  const right = Math.max(...boxes.map((box) => box.x + box.w));
  const bottom = Math.max(...boxes.map((box) => box.y + box.h));
  const margin = 48;
  const k = invClampZoom(Math.min((width - margin * 2) / Math.max(1, right - left), (height - margin * 2) / Math.max(1, bottom - top), 1.2));
  return {
    k,
    x: (width - (right - left) * k) / 2 - left * k,
    y: (height - (bottom - top) * k) / 2 - top * k,
  };
}

function invClampZoom(k) {
  return Math.min(INV_ZOOM_MAX, Math.max(INV_ZOOM_MIN, Number.isFinite(k) ? k : 1));
}

// Zooms by `factor` keeping the screen point (px, py) fixed.
function invZoomAt(view, factor, px, py) {
  const k = invClampZoom(view.k * factor);
  const ratio = k / view.k;
  return {k, x: px - (px - view.x) * ratio, y: py - (py - view.y) * ratio};
}

function invScreenToWorld(view, px, py) {
  return {x: (px - view.x) / view.k, y: (py - view.y) / view.k};
}

// A gentle curve between two box centres, ending on the box borders.
function invEdgePath(a, b) {
  const ax = a.x + a.w / 2, ay = a.y + a.h / 2;
  const bx = b.x + b.w / 2, by = b.y + b.h / 2;
  const start = invBorderPoint(a, bx, by);
  const end = invBorderPoint(b, ax, ay);
  const dx = (end.x - start.x) / 2;
  return `M${start.x.toFixed(1)},${start.y.toFixed(1)} C${(start.x + dx).toFixed(1)},${start.y.toFixed(1)} ${(end.x - dx).toFixed(1)},${end.y.toFixed(1)} ${end.x.toFixed(1)},${end.y.toFixed(1)}`;
}

function invBorderPoint(box, towardX, towardY) {
  const cx = box.x + box.w / 2, cy = box.y + box.h / 2;
  const dx = towardX - cx, dy = towardY - cy;
  if (dx === 0 && dy === 0) return {x: cx, y: cy};
  const scale = Math.min(Math.abs((box.w / 2) / (dx || 1e-9)), Math.abs((box.h / 2) / (dy || 1e-9)));
  return {x: cx + dx * Math.min(scale, 1), y: cy + dy * Math.min(scale, 1)};
}

function invMidpoint(a, b) {
  return {x: (a.x + a.w / 2 + b.x + b.w / 2) / 2, y: (a.y + a.h / 2 + b.y + b.h / 2) / 2};
}

function invParseLayout(text) {
  try {
    const value = JSON.parse(text || '{}');
    if ([value.x, value.y, value.k].every(Number.isFinite)) return {x: value.x, y: value.y, k: invClampZoom(value.k)};
  } catch (_) { /* fall through */ }
  return null;
}

// Canonical shortcut text from a keydown, matching phorminx-windows parsing.
function invShortcutFromEvent(event) {
  let key = null;
  if (/^Key[A-Z]$/.test(event.code)) key = event.code.slice(3);
  else if (/^Digit[0-9]$/.test(event.code)) key = event.code.slice(5);
  else if (/^F(?:[1-9]|1[0-9]|2[0-4])$/.test(event.code)) key = event.code;
  else if (event.code === 'Space') key = 'Space';
  if (!key) return null;
  const parts = [];
  if (event.ctrlKey) parts.push('Ctrl');
  if (event.altKey) parts.push('Alt');
  if (event.shiftKey) parts.push('Shift');
  parts.push(key);
  return parts.join('+');
}

function invFormatBytes(bytes) {
  if (bytes >= 1e6) return `${(bytes / 1e6).toFixed(1)} MB`;
  if (bytes >= 1e3) return `${Math.round(bytes / 1e3)} KB`;
  return `${bytes} B`;
}

if (typeof module !== 'undefined') {
  module.exports = {invKindLabel, invCardLabel, invSplitCitations, invDayLabel, invVisibleItems, invNodeHeight, invFitView, invZoomAt, invScreenToWorld, invEdgePath, invMidpoint, invParseLayout, invShortcutFromEvent, invFormatBytes, invClampZoom, INV_NODE_WIDTH};
}
