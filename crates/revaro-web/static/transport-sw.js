import { fileResponse, bufferedRequest, configureTransport, clearTransportCache } from './transport-core.js';
self.addEventListener('install', event => event.waitUntil(self.skipWaiting()));
self.addEventListener('activate', event => event.waitUntil(self.clients.claim()));
let configuration;
function config() {
  return configuration ||= bufferedRequest(new Request(new URL('/api/transport/config', self.location.origin)), false)
    .then(r => r.json()).then(configureTransport).catch(() => { configuration = null; });
}
self.addEventListener('message', event => {
  if (event.data?.type === 'claim-clients') event.waitUntil(self.clients.claim());
  if (event.data?.type === 'transport-config') configureTransport(event.data.config);
  if (event.data?.type === 'clear-transport') event.waitUntil(clearTransportCache());
});
self.addEventListener('fetch', event => {
  const request = event.request, url = new URL(request.url);
  if (url.origin !== self.location.origin || request.headers.has('x-revaro-managed')) return;
  if (!url.pathname.startsWith('/api/') && !url.pathname.startsWith('/s/')) return;
  const response = (async () => {
    await config();
    if (url.pathname === '/api/auth/logout') await clearTransportCache();
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
