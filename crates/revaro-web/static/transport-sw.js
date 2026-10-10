import { fileResponse, bufferedRequest, configureTransport, clearTransportCache, setPlaybackState, policy } from './transport-core.js';
self.addEventListener('install', event => event.waitUntil(self.skipWaiting()));
self.addEventListener('activate', event => event.waitUntil(self.clients.claim()));
let configuration;
const prefetches = new Map();
function config() {
  return configuration ||= bufferedRequest(new Request(new URL('/api/transport/config', self.location.origin)), false)
    .then(r => r.json()).then(configureTransport).catch(() => { configuration = null; });
}
self.addEventListener('message', event => {
  if (event.data?.type === 'claim-clients') event.waitUntil(self.clients.claim());
  if (event.data?.type === 'transport-config') configureTransport(event.data.config);
  if (event.data?.type === 'clear-transport') {
    for (const entry of prefetches.values()) entry.controller.abort();
    prefetches.clear(); event.waitUntil(clearTransportCache());
  }
  if (event.data?.type === 'playback-state') {
    for (const [key, entry] of prefetches) if (Date.now() - entry.updated > 15000) { entry.controller.abort(); prefetches.delete(key); }
    const state = event.data.state;
    if (!state?.key || !event.source?.id) return;
    const key = `${event.source.id}:${state.key}`;
    setPlaybackState({ ...state, key });
    const previous = prefetches.get(key);
    if (previous) previous.updated = Date.now();
    const shouldPrefetch = !state.ended && state.playing && state.bufferSeconds >= 20 && state.nextUrl;
    if (previous && (!shouldPrefetch || previous.url !== state.nextUrl)) {
      previous.controller.abort(); prefetches.delete(key);
    }
    if (!shouldPrefetch || prefetches.has(key) || prefetches.size >= 2) return;
    const url = new URL(state.nextUrl, self.location.origin);
    if (url.origin !== self.location.origin || !/^\/api\/files\/[^/]+\/preview$/.test(url.pathname)) return;
    const controller = new AbortController();
    const entry = { controller, url: state.nextUrl, updated: Date.now() };
    prefetches.set(key, entry);
    // Only the next track's bounded introduction is warmed. A buffer warning,
    // seek or queue change cancels it immediately; completed data is reusable.
    event.waitUntil((async () => {
      let response;
      const timer = setTimeout(() => { controller.abort(); if (prefetches.get(key) === entry) prefetches.delete(key); }, 12000);
      try {
        response = await fileResponse(new Request(url, { signal: controller.signal,
          headers: { range: `bytes=0-${Math.min(256 * 1024, policy.blockBytes * 4) - 1}`, 'x-revaro-priority': 'background' } }));
        if (response.ok) await response.arrayBuffer();
        else await response.body?.cancel();
        await response.transportComplete;
      } catch { if (prefetches.get(key) === entry) prefetches.delete(key); }
      finally { clearTimeout(timer); }
    })());
  }
});
self.addEventListener('fetch', event => {
  const request = event.request, url = new URL(request.url);
  if (url.origin !== self.location.origin) return;
  // Explicit native forwarding also settles WebKit's fetch event while
  // another worker response streams media. The document owns API recovery.
  if (request.headers.has('x-revaro-managed')) {
    event.respondWith(fetch(request));
    return;
  }
  if (!url.pathname.startsWith('/api/') && !url.pathname.startsWith('/s/')) return;
  const response = (async () => {
    await config();
    if (url.pathname === '/api/auth/logout') {
      for (const entry of prefetches.values()) entry.controller.abort();
      prefetches.clear(); await clearTransportCache();
    }
    // Mutable listings/progress metadata are read atomically in one buffered
    // request. File bytes and immutable derived resources use Range windows.
    const fileBody = /^\/api\/files\/[^/]+\/(download|preview|thumbnail|content|versions\/[^/]+\/content|book\/(cover|assets\/[^/]+|flow(\/chunks\/[^/]+)?))$/.test(url.pathname);
    const archive = /^\/api\/files\/batch-download\/[^/]+$/.test(url.pathname);
    if (request.method === 'GET' && (fileBody || archive || url.pathname.startsWith('/s/')))
      return fileResponse(request);
    let safe = ['GET', 'HEAD'].includes(request.method) || /\/uploads\/[^/]+\/complete$/.test(url.pathname);
    if (request.method === 'POST' && url.pathname === '/api/uploads') {
      try { safe = Boolean((await request.clone().json()).idempotency_key); } catch {}
    }
    return bufferedRequest(request, safe);
  })();
  event.respondWith(response);
  // Headers resolve before the streamed download is consumed. Keep the event
  // alive until EOF/cancellation, including fresh public recipients.
  event.waitUntil(response.then(result => result.transportComplete).catch(() => {}));
});
