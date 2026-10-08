// Deterministic shared-link model, not a real ISP/QUIC measurement.
// Optional comparison: node .../peak-benchmark.mjs /path/to/before.mjs
import { pathToFileURL } from 'node:url';
import { performance } from 'node:perf_hooks';

const bytesPerSecond = 128 * 1024, latencyMs = 80, tickMs = 10;
const payload = new Uint8Array(4 * 1024 * 1024), results = [];
let serial = 0;
async function measure(moduleURL, label, scenario) {
  const transfers = new Set(); let requests = 0;
  globalThis.location = new URL('http://localhost:8081/'); globalThis.caches = undefined;
  globalThis.fetch = async (url, init) => {
    requests++;
    await new Promise((resolve, reject) => {
      const abort = () => { clearTimeout(timer); reject(init.signal.reason); };
      const timer = setTimeout(() => { init.signal.removeEventListener('abort', abort); resolve(); }, latencyMs);
      init.signal.addEventListener('abort', abort, { once: true });
      if (init.signal.aborted) abort();
    });
    const [start, wantedEnd] = new Headers(init.headers).get('range').slice(6).split('-').map(Number);
    const end = Math.min(wantedEnd, payload.length - 1), length = end - start + 1;
    return new Response(new ReadableStream({ start(controller) {
      const transfer = { controller, start, length, received: 0, budget: 0 };
      transfers.add(transfer);
      init.signal.addEventListener('abort', () => {
        if (transfers.delete(transfer)) controller.error(init.signal.reason);
      }, { once: true });
    } }), { status: 206, headers: {
      'content-range': `bytes ${start}-${end}/${payload.length}`, 'content-length': String(length),
      etag: '"unchanged"', 'cache-control': 'no-store',
    } });
  };
  const timer = setInterval(() => {
    const budget = bytesPerSecond * tickMs / 1000 / transfers.size;
    for (const transfer of transfers) {
      transfer.budget += budget;
      const bytes = Math.min(transfer.length - transfer.received, Math.floor(transfer.budget));
      if (!bytes) continue;
      transfer.controller.enqueue(payload.subarray(transfer.start + transfer.received, transfer.start + transfer.received + bytes));
      transfer.received += bytes; transfer.budget -= bytes;
      if (transfer.received === transfer.length) { transfer.controller.close(); transfers.delete(transfer); }
    }
  }, tickMs);
  const core = await import(`${moduleURL}?benchmark=${++serial}`), controller = new AbortController();
  let bulk;
  const url = path => new URL(`/api/files/${path}`, location);
  try {
    if (scenario === 'foreground-with-bulk') {
      core.setPlaybackState?.({ key: 'player', playing: true, bufferSeconds: 3 });
      bulk = Promise.allSettled(Array.from({ length: 12 }, (_, index) => core.bufferedRequest(new Request(url(`bulk-${index}/download`), {
        signal: controller.signal, headers: { range: 'bytes=0-65535' },
      }))));
      await new Promise(resolve => setTimeout(resolve, 100));
    }
    const before = performance.now();
    const response = scenario === 'foreground-with-bulk'
      ? await core.bufferedRequest(new Request(url('foreground/preview'), { signal: controller.signal, headers: { range: 'bytes=0-2047' } }))
      : await core.fileResponse(new Request(url('large/download'), { signal: controller.signal }));
    const reader = response.body.getReader(), first = await reader.read();
    results.push({ label, scenario, firstDeliveryMs: Math.round(performance.now() - before), firstBytes: first.value.length, requests });
    await reader.cancel(); await response.transportComplete;
  } finally {
    controller.abort(); await bulk; clearInterval(timer);
  }
}
const current = new URL('../../crates/revaro-web/static/transport-core.js', import.meta.url).href;
for (const scenario of ['cold-large', 'foreground-with-bulk']) {
  if (process.argv[2]) await measure(pathToFileURL(process.argv[2]).href, 'before', scenario);
  await measure(current, 'after', scenario);
}
console.log(JSON.stringify({ model: { bytesPerSecond, latencyMs, fairSharing: true }, results }, null, 2));
