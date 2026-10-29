// Shared by the main window and the companion: commentary cards, important
// items and plan times. Pure helpers; the pages own the DOM.

// Commentary can end with "PIN: item" to suggest an important item. The line
// is not shown as text; it becomes a button instead.
const PIN_PATTERN = /^\s*(?:[*_]{1,2})?PIN:(?:[*_]{1,2})?\s*(.+?)\s*(?:[*_]{1,2})?\s*$/i;

function commentaryParts(text) {
  const pins = [];
  const kept = [];
  for (const line of String(text || '').split('\n')) {
    const match = line.match(PIN_PATTERN);
    if (match && match[1].trim()) {
      const pin = match[1].replace(/^[*_]+|[*_]+$/g, '').trim();
      if (pin && pin.length <= 500 && !pins.includes(pin)) pins.push(pin);
    } else kept.push(line);
  }
  return {body: kept.join('\n').trim(), pins};
}

function hasItem(items, text) {
  const key = String(text).trim().toLowerCase();
  return (items || []).some((item) => item.text.trim().toLowerCase() === key);
}

const COMMENTARY_STYLES = [
  ['', 'Balanced'],
  ['replies', 'Suggest replies'],
  ['facts', 'Fact-check'],
  ['actions', 'Track actions'],
  ['coach', 'Coach me'],
];

const pad = (value) => String(value).padStart(2, '0');

// Value for <input type="datetime-local"> in local time.
function toLocalInput(ms) {
  if (ms === null || ms === undefined) return '';
  const date = new Date(ms);
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

function fromLocalInput(value) {
  if (!value) return null;
  const ms = new Date(value).getTime();
  return Number.isFinite(ms) ? ms : null;
}

function dayStart(ms) {
  const date = new Date(ms);
  return new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
}

// "Today 14:30", "Tomorrow 09:00", "Mon 3 Nov 14:30" or "Not scheduled".
function planWhen(ms, now = Date.now()) {
  if (ms === null || ms === undefined) return 'Not scheduled';
  const date = new Date(ms);
  const time = `${pad(date.getHours())}:${pad(date.getMinutes())}`;
  const days = Math.round((dayStart(ms) - dayStart(now)) / 86400000);
  if (days === 0) return `Today ${time}`;
  if (days === 1) return `Tomorrow ${time}`;
  if (days === -1) return `Yesterday ${time}`;
  const weekday = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'][date.getDay()];
  const month = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'][date.getMonth()];
  const year = date.getFullYear() !== new Date(now).getFullYear() ? ` ${date.getFullYear()}` : '';
  return `${weekday} ${date.getDate()} ${month}${year} ${time}`;
}

// A plan worth offering on the Live page: starts within the hour or started
// less than an hour ago, and has not been recorded yet.
function nextPlan(plans, now = Date.now()) {
  return (plans || []).find((plan) => plan.sessionId === null && plan.scheduledAtMs !== null
    && plan.scheduledAtMs - now <= 3600000 && now - plan.scheduledAtMs <= 3600000) || null;
}

if (typeof module !== 'undefined') {
  module.exports = {commentaryParts, hasItem, COMMENTARY_STYLES, toLocalInput, fromLocalInput, planWhen, nextPlan};
}
