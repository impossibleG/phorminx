// Theme from ?theme=light|dark, and simple line icons for <i data-icon="name">.
const params = new URLSearchParams(location.search);
document.documentElement.dataset.theme = params.get('theme') === 'light' ? 'light' : 'dark';

const ICONS = {
  keyboard: '<rect x="3" y="6" width="18" height="12" rx="1.5"/><path d="M7 10h.01M11 10h.01M15 10h.01M7 14h10"/>',
  mic: '<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5 11a7 7 0 0 0 14 0M12 18v3"/>',
  speaker: '<rect x="3" y="4" width="18" height="13" rx="1.5"/><path d="M8 21h8M12 17v4"/>',
  wave: '<path d="M3 12h2M7 8v8M11 5v14M15 9v6M19 11v2"/>',
  chip: '<rect x="7" y="7" width="10" height="10" rx="1"/><path d="M10 3v4M14 3v4M10 17v4M14 17v4M3 10h4M3 14h4M17 10h4M17 14h4"/>',
  sparkle: '<path d="M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8z"/><path d="M19 16l.7 2 2 .7-2 .7-.7 2-.7-2-2-.7 2-.7z"/>',
  cursor: '<path d="M4 6h10M4 10h14M4 14h8"/><path d="M17 13v8M15 13h4M15 21h4"/>',
  translate: '<path d="M4 5h9M8.5 3v2M6 5c0 4 3 7 6 8M11 5c0 4-3 7-6 9"/><path d="M13 21l4-9 4 9M14.5 18h5"/>',
  chat: '<path d="M4 5h16v11H9l-5 4z"/><path d="M8 9h8M8 12h5"/>',
  clipboard: '<rect x="5" y="4" width="14" height="17" rx="1.5"/><path d="M9 4V3h6v1M9 10h6M9 14h6"/>',
  image: '<rect x="3" y="5" width="18" height="14" rx="1.5"/><circle cx="9" cy="10" r="1.6"/><path d="M21 16l-5-5-8 8"/>',
  link: '<path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1"/><path d="M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"/>',
  note: '<path d="M5 3h10l4 4v14H5z"/><path d="M15 3v4h4M8 12h8M8 16h5"/>',
  timeline: '<path d="M6 3v18"/><circle cx="6" cy="7" r="2"/><circle cx="6" cy="17" r="2"/><path d="M11 7h9M11 17h7"/>',
  graph: '<circle cx="6" cy="6" r="2.5"/><circle cx="18" cy="8" r="2.5"/><circle cx="9" cy="18" r="2.5"/><path d="M8.3 7l7.3.8M7 8.4l1.4 7.2M16.4 10l-5.6 6.4"/>',
  quote: '<path d="M4 5h16v11H9l-5 4z"/><path d="M9 9l-1.5 3M13 9l-1.5 3"/>',
  user: '<circle cx="12" cy="8" r="4"/><path d="M4 21c1.5-4 4.5-6 8-6s6.5 2 8 6"/>',
  onion: '<path d="M12 3c-1 3-6 5-6 11a6 6 0 0 0 12 0c0-6-5-8-6-11z"/><path d="M12 9c-1 2-3 3-3 6a3 3 0 0 0 6 0c0-3-2-4-3-6z"/>',
  shield: '<path d="M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6z"/><path d="M8.5 12l2.5 2.5 4.5-5"/>',
  nocloud: '<path d="M7 18h10a4 4 0 0 0 .5-8A6 6 0 0 0 6 11a3.5 3.5 0 0 0 1 7z"/><path d="M4 4l16 16"/>',
  search: '<circle cx="11" cy="11" r="6"/><path d="M20 20l-4.5-4.5"/>',
  bolt: '<path d="M13 3L5 14h6l-1 7 8-11h-6z"/>',
  box: '<path d="M3 7l9-4 9 4v10l-9 4-9-4z"/><path d="M3 7l9 4 9-4M12 11v10"/>',
  refresh: '<path d="M20 11a8 8 0 0 0-14.5-4.5L3 9M4 13a8 8 0 0 0 14.5 4.5L21 15"/><path d="M3 4v5h5M21 20v-5h-5"/>',
  signature: '<path d="M5 3h10l4 4v14H5z"/><path d="M15 3v4h4"/><path d="M8 16c2-3 3-3 3-1s2 2 4-1"/>',
  hash: '<path d="M9 3L7 21M17 3l-2 18M4 8h17M3 16h17"/>',
  power: '<path d="M12 3v8"/><path d="M6.3 7a8 8 0 1 0 11.4 0"/>',
  lock: '<rect x="5" y="10" width="14" height="11" rx="1.5"/><path d="M8 10V7a4 4 0 0 1 8 0v3"/>',
};
for (const node of document.querySelectorAll('[data-icon]')) {
  node.innerHTML = `<svg viewBox="0 0 24 24" aria-hidden="true">${ICONS[node.dataset.icon] || ''}</svg>`;
}
