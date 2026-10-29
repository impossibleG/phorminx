// Meeting segments keep their source times in the saved text, for example
// "[01:02.345 · Computer audio] hello". The times stay in storage and in what
// the assistant reads; these helpers only decide what the transcript shows.
const STAMP_PATTERN = /^\s*\[\d{1,4}:\d{2}(?:\.\d{1,3})?\s*·\s*([^\]\n]*)\]\s*/;

function speakerOf(label) {
  const value = label.trim().toLowerCase();
  if (value.startsWith('you') || value.includes('microphone')) return 'microphone';
  if (value.includes('computer')) return 'computer';
  return '';
}

// Splits one segment into display parts: [{source, text}]. A part without a
// recognised time prefix keeps the segment's own source.
function transcriptParts(text, source) {
  const parts = [];
  for (const raw of String(text || '').split('\n')) {
    const match = raw.match(STAMP_PATTERN);
    const body = (match ? raw.slice(match[0].length) : raw).trim();
    if (!body) continue;
    parts.push({source: (match && speakerOf(match[1])) || source, text: body});
  }
  return parts;
}

// Translations usually keep the prefixes; when the line counts match, each
// part gets its own translation, otherwise the whole translation goes last.
function translationParts(text, count) {
  const lines = transcriptParts(text, '').map((part) => part.text);
  if (lines.length === count) return lines;
  const whole = lines.join(' ');
  return Array.from({length: count}, (_, index) => (index === count - 1 ? whole : ''));
}

if (typeof module !== 'undefined') module.exports = {transcriptParts, translationParts};
