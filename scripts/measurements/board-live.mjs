// CAD-611: real Chromium network bytes, against an isolated board only.
// Node >=22 (native WebSocket); no browser automation dependency.
import { spawn } from 'node:child_process';
import { mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const [endpointFile, outputFile] = process.argv.slice(2);
if (!endpointFile || !outputFile) throw new Error('usage: node board-live.mjs ENDPOINT_FILE OUTPUT_JSON');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let origin;
for (let i = 0; i < 600; i++) {
  try { origin = new URL((await readFile(endpointFile, 'utf8')).trim()); break; } catch { await sleep(100); }
}
if (!origin || origin.hostname !== '127.0.0.1' || +origin.port < 3110 || +origin.port > 3198) {
  throw new Error('only an isolated loopback board on ports 3110–3198 is allowed');
}
const profile = await mkdtemp(join(tmpdir(), 'cad611-chrome-'));
const chrome = spawn(process.env.CADENCE_CHROME ?? 'google-chrome', [
  '--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check',
  '--remote-debugging-port=3199', '--remote-debugging-address=127.0.0.1',
  `--user-data-dir=${profile}`, 'about:blank',
], { stdio: ['ignore', 'ignore', 'pipe'] });
// Do not attach to an existing process on the debugging port. Proceed only
// after OUR child announces its DevTools endpoint. A port conflict fails.
const started = new Promise((resolve, reject) => {
  let output = '';
  chrome.stderr.on('data', (data) => {
    output += data.toString();
    if (/DevTools listening on ws:\/\/127\.0\.0\.1:3199\/devtools\/browser\//.test(output)) resolve();
  });
  chrome.once('error', reject);
  chrome.once('exit', (code) => reject(new Error(`our Chrome exited (${code}): ${output}`)));
});
let ws;
try {
  await started;
  let target;
  for (let i = 0; i < 200; i++) {
    if (chrome.exitCode !== null) throw new Error('our Chrome failed to start');
    try { target = await (await fetch('http://127.0.0.1:3199/json/new?about:blank', { method: 'PUT' })).json(); break; } catch { await sleep(100); }
  }
  if (!target) throw new Error('Chrome debugging endpoint unavailable');
  ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { ws.addEventListener('open', resolve, { once: true }); ws.addEventListener('error', reject, { once: true }); });
  let sequence = 0;
  const pending = new Map();
  const requests = new Map();
  const samples = { idle: { bytes: 0, requests: 0, paths: {} }, busy: { bytes: 0, requests: 0, paths: {} } };
  let phase = null;
  const send = (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++sequence; pending.set(id, { resolve, reject }); ws.send(JSON.stringify({ id, method, params }));
  });
  ws.addEventListener('message', ({ data }) => {
    const message = JSON.parse(data);
    if (message.id) {
      const wait = pending.get(message.id); pending.delete(message.id);
      if (message.error) wait?.reject(new Error(JSON.stringify(message.error))); else wait?.resolve(message.result);
      return;
    }
    const p = message.params;
    if (message.method === 'Network.requestWillBeSent') {
      const url = new URL(p.request.url);
      requests.set(p.requestId, { path: url.pathname, type: p.type, phase });
      if (phase && url.origin === origin.origin) {
        samples[phase].requests++;
        samples[phase].paths[url.pathname] = (samples[phase].paths[url.pathname] ?? 0) + 1;
      }
    }
    if (message.method === 'Network.responseReceived') {
      const request = requests.get(p.requestId);
      if (request) request.status = p.response.status;
    }
    if (!phase) return;
    const request = requests.get(p?.requestId);
    // SSE never finishes: count transferred chunks. For finite reads,
    // loadingFinished includes response headers and wire-encoded body.
    if (message.method === 'Network.dataReceived' && request?.type === 'EventSource') samples[phase].bytes += p.encodedDataLength;
    if (message.method === 'Network.loadingFinished' && request?.type !== 'EventSource' && request?.phase === phase) samples[phase].bytes += p.encodedDataLength;
  });
  await send('Network.enable');
  await send('Network.setCacheDisabled', { cacheDisabled: true });
  await send('Page.enable');
  await send('Page.navigate', { url: origin.href });
  await sleep(10_000); // initial loads excluded, same in both trees
  if (![...requests.values()].some((r) => r.path === '/api/issues' && r.status === 200) ||
      ![...requests.values()].some((r) => r.path === '/api/stream' && r.status === 200)) {
    throw new Error('built board UI did not load its collection and live stream');
  }
  phase = 'idle'; await sleep(60_000);
  phase = 'busy'; await writeFile(`${endpointFile}.busy`, 'start\n'); await sleep(60_000);
  phase = null;
  const output = {
    scenario: '420 issues, 12 agents, 160 jobs; warmup 10s; idle 60s; twenty title changes 3s apart during busy 60s',
    measured: samples,
    targets: { idleBytesPerMinute: 50_000, busyBytesPerMinute: 2_000_000 },
    pass: { idle: samples.idle.bytes < 50_000, busy: samples.busy.bytes < 2_000_000 },
  };
  await writeFile(outputFile, `${JSON.stringify(output, null, 2)}\n`);
  console.log(JSON.stringify(output));
  if (!output.pass.idle || !output.pass.busy) process.exitCode = 1;
} finally {
  ws?.close();
  // Only the Chrome child this harness started is signalled.
  if (chrome.pid) {
    chrome.kill('SIGTERM');
    await new Promise((resolve) => { if (chrome.exitCode !== null) resolve(); else chrome.once('exit', resolve); });
  }
  await rm(profile, { recursive: true, force: true });
}
