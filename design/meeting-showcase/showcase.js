const frame = document.getElementById('window-frame');
const stage = document.getElementById('stage');
const companion = document.getElementById('companion');
const hiddenState = document.getElementById('hidden-state');
const scaleNote = document.getElementById('scale-note');
let view = 'together';
let size = 'wide';
let recording = true;

function syncButtons(selector, attribute, selected) {
  document.querySelectorAll(selector).forEach(button => {
    const active = button.getAttribute(attribute) === selected;
    button.classList.toggle('active', active);
    if (selector === '[data-room-tab]') button.setAttribute('aria-current', active ? 'page' : 'false');
    else button.setAttribute('aria-pressed', String(active));
  });
}

function setView(next) {
  if (size === 'compact' && next === 'together') return;
  view = next;
  frame.dataset.view = view;
  syncButtons('[data-view]', 'data-view', view);
  syncButtons('[data-room-tab]', 'data-room-tab', view);
}

function setSize(next) {
  size = next;
  frame.classList.toggle('wide', size === 'wide');
  frame.classList.toggle('compact', size === 'compact');
  document.querySelectorAll('[data-room-tab="together"], [data-view="together"]').forEach(button => {
    button.disabled = size === 'compact';
    button.title = size === 'compact' ? 'Together is available when the window is wide.' : '';
  });
  if (size === 'compact' && view === 'together') setView('transcript');
  syncButtons('[data-size]', 'data-size', size);
  fitToStage();
}

function fitToStage() {
  const desiredWidth = size === 'wide' ? 980 : 430;
  const desiredHeight = size === 'wide' ? 720 : 680;
  const available = Math.max(310, stage.clientWidth - 2);
  const scale = Math.min(1, available / desiredWidth);
  frame.style.zoom = scale;
  stage.style.minHeight = `${Math.ceil(desiredHeight * scale) + 45}px`;
  scaleNote.textContent = scale < .999 ? `Window preview scaled to ${Math.round(scale * 100)}% to fit this browser.` : '';
}

document.querySelectorAll('[data-view], [data-room-tab]').forEach(button => button.addEventListener('click', () => setView(button.dataset.view || button.dataset.roomTab)));
document.querySelectorAll('[data-size]').forEach(button => button.addEventListener('click', () => setSize(button.dataset.size)));
document.querySelectorAll('[data-theme]').forEach(button => button.addEventListener('click', () => {
  frame.classList.toggle('light', button.dataset.theme === 'light');
  frame.classList.toggle('dark', button.dataset.theme === 'dark');
  syncButtons('[data-theme]', 'data-theme', button.dataset.theme);
}));
document.querySelectorAll('[data-language-choice]').forEach(button => button.addEventListener('click', () => {
  frame.dataset.language = button.dataset.languageChoice;
  syncButtons('[data-language-choice]', 'data-language-choice', button.dataset.languageChoice);
}));
['mic', 'computer'].forEach(id => document.getElementById(id).addEventListener('click', event => {
  const button = event.currentTarget;
  const on = button.getAttribute('aria-pressed') !== 'true';
  button.setAttribute('aria-pressed', String(on));
  button.querySelector('em').textContent = on ? 'on' : 'paused';
}));
document.getElementById('commentary').addEventListener('click', event => {
  const button = event.currentTarget;
  const on = button.getAttribute('aria-checked') !== 'true';
  button.setAttribute('aria-checked', String(on));
  button.classList.toggle('on', on);
  document.getElementById('insight').hidden = !on;
});
document.getElementById('hide').addEventListener('click', () => { companion.hidden = true; hiddenState.hidden = false; });
document.getElementById('show-again').addEventListener('click', () => { companion.hidden = false; hiddenState.hidden = true; });
document.getElementById('finish').addEventListener('click', event => {
  recording = !recording;
  document.getElementById('record-status').textContent = recording ? 'LISTENING' : 'FINISHED';
  event.currentTarget.textContent = recording ? 'Finish' : 'Restart demo';
  frame.classList.toggle('finished', !recording);
});

document.getElementById('composer').addEventListener('submit', event => {
  event.preventDefault();
  const input = document.getElementById('question');
  const value = input.value.trim();
  if (!value) return;
  const scroller = document.getElementById('conversation-scroll');
  const question = document.createElement('article');
  question.className = 'message from-you';
  const questionLabel = document.createElement('small');
  questionLabel.textContent = 'YOU · NOW';
  const questionText = document.createElement('p');
  questionText.textContent = value;
  question.append(questionLabel, questionText);
  const answer = document.createElement('article');
  answer.className = 'message from-phorminx';
  const answerLabel = document.createElement('small');
  answerLabel.textContent = 'PHORMINX · DEMO';
  const answerText = document.createElement('p');
  answerText.textContent = 'The review moved to Thursday. Confirm who signs off on the narrowed scope and what must be ready by Friday.';
  answer.append(answerLabel, answerText);
  scroller.append(question, answer);
  scroller.scrollTop = scroller.scrollHeight;
  input.value = '';
});

const divider = document.getElementById('divider');
const room = document.getElementById('room');
let dragging = false;
function setSplit(clientX) {
  const bounds = room.getBoundingClientRect();
  const pct = Math.max(35, Math.min(65, ((clientX - bounds.left) / bounds.width) * 100));
  room.style.setProperty('--split', `${pct}%`);
}
divider.addEventListener('pointerdown', event => {
  if (size !== 'wide' || view !== 'together') return;
  dragging = true;
  divider.setPointerCapture(event.pointerId);
  setSplit(event.clientX);
});
divider.addEventListener('pointermove', event => { if (dragging) setSplit(event.clientX); });
divider.addEventListener('pointerup', () => { dragging = false; });
divider.addEventListener('pointercancel', () => { dragging = false; });
divider.addEventListener('dblclick', () => room.style.setProperty('--split', '57%'));
divider.addEventListener('keydown', event => {
  if (!['ArrowLeft', 'ArrowRight'].includes(event.key)) return;
  event.preventDefault();
  const current = parseFloat(room.style.getPropertyValue('--split')) || 57;
  room.style.setProperty('--split', `${Math.max(35, Math.min(65, current + (event.key === 'ArrowRight' ? 3 : -3)))}%`);
});
window.addEventListener('resize', fitToStage);
setSize('wide');
setView('together');
