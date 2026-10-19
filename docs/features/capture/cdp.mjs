// Drives a running Phorminx WebView2 window over the DevTools protocol.
//
//   node cdp.mjs <port> <page> op [op ...]
//
// <page> is a substring of the window URL (product.html, companion.html).
// Ops run in order on one connection, so a colour-scheme emulation applies to
// the screenshots that follow it:
//   media:dark | media:light   emulate prefers-color-scheme
//   locale:en-US               format dates and numbers for this locale
//   eval:<js>                  run JavaScript in the page and print the result
//   wait:<ms>                  pause
//   shot:<file.png>            capture the page as PNG
// Needs Node 18+ (global fetch and WebSocket). Nothing here talks to the
// network beyond 127.0.0.1.
import { writeFileSync } from 'node:fs';

const [port, pageMatch, ...ops] = process.argv.slice(2);
const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
const target = targets.find((t) => t.type === 'page' && t.url.includes(pageMatch));
if (!target) {
  console.error(`No page matching ${pageMatch}:`, targets.map((t) => t.url));
  process.exit(1);
}
const socket = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = reject; });
let nextId = 1;
const pending = new Map();
socket.onmessage = (event) => {
  const message = JSON.parse(event.data);
  if (message.id && pending.has(message.id)) {
    const { resolve, reject } = pending.get(message.id);
    pending.delete(message.id);
    if (message.error) reject(new Error(message.error.message)); else resolve(message.result);
  }
};
const send = (method, params = {}) => new Promise((resolve, reject) => {
  const id = nextId++;
  pending.set(id, { resolve, reject });
  socket.send(JSON.stringify({ id, method, params }));
});

for (const op of ops) {
  const colon = op.indexOf(':');
  const [name, arg] = [op.slice(0, colon), op.slice(colon + 1)];
  if (name === 'media') {
    await send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-color-scheme', value: arg }] });
  } else if (name === 'locale') {
    await send('Emulation.setLocaleOverride', { locale: arg });
  } else if (name === 'eval') {
    const result = await send('Runtime.evaluate', { expression: arg, awaitPromise: true, returnByValue: true });
    if (result.exceptionDetails) console.error(result.exceptionDetails.text, result.exceptionDetails.exception?.description);
    else if (result.result.value !== undefined) console.log(JSON.stringify(result.result.value));
  } else if (name === 'wait') {
    await new Promise((resolve) => setTimeout(resolve, Number(arg)));
  } else if (name === 'shot') {
    const { data } = await send('Page.captureScreenshot', { format: 'png' });
    writeFileSync(arg, Buffer.from(data, 'base64'));
    console.log(`saved ${arg}`);
  } else {
    throw new Error(`unknown op ${op}`);
  }
}
socket.close();
