import { fileResponse, bufferedRequest, configureTransport, clearTransportCache, setPlaybackState, isFileResource, usesNativeFileLoading } from './transport-core.js';
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
  if (event.data?.type === 'playback-state') {
    const state = event.data.state;
    if (state?.key && event.source?.id)
      setPlaybackState({ ...state, key: `${event.source.id}:${state.key}` });
  }
});
self.addEventListener('fetch', event => {
  const request = event.request, url = new URL(request.url);
  // Explicit native forwarding also settles WebKit's fetch event while
  // another worker response streams media. The document owns API recovery.
  if (request.headers.get('x-revaro-managed') === 'document') {
    event.respondWith(fetch(request));
    return;
  }
  // XHR uploads retain their native path, including real upload progress.
  if (url.origin !== self.location.origin || request.headers.has('x-revaro-managed')) return;
  if (!url.pathname.startsWith('/api/') && !url.pathname.startsWith('/s/')) return;
  // Let the browser own these requests entirely. Re-fetching a conditional
  // native image/media request inside the worker interferes with WebKit's
  // HTTP cache merging and media reconnects. The document-managed API case
  // above still explicitly settles its event for WebKit.
  if (usesNativeFileLoading(request)) return;
  const response = (async () => {
    await config();
    if (url.pathname === '/api/auth/logout') {
      await clearTransportCache();
    }
    // Mutable listings/progress metadata are read atomically in one buffered
    // request. File bytes and immutable derived resources use Range windows.
    if (request.method === 'GET' && isFileResource(url.pathname))
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
