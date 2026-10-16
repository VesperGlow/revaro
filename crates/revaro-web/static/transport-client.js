import { putBlob, blobHash, setPlaybackState, bufferedRequest, fileResponse, clearTransportCache, isFileResource } from './transport-core.js';
export { putBlob, blobHash };
const nativeFetch = globalThis.fetch.bind(globalThis);
let directFiles = false, apiInstalled = false;
function installAPITransport() {
  if (apiInstalled) return;
  apiInstalled = true;
  // Mutable control requests use the same recovery engine in their document.
  // They remain responsive if a worker is stopped, replaced or unavailable.
  globalThis.fetch = async (input, init) => {
    const request = new Request(input, init), url = new URL(request.url);
    if (url.origin !== location.origin || request.headers.has('x-revaro-managed')
        || (!url.pathname.startsWith('/api/') && !url.pathname.startsWith('/s/'))
        || (!directFiles && isFileResource(url.pathname) && ['GET', 'HEAD'].includes(request.method))) return nativeFetch(request);
    request.headers.set('x-revaro-managed', 'document');
    if (url.pathname === '/api/auth/logout') {
      await clearTransportCache();
      navigator.serviceWorker?.controller?.postMessage({ type: 'clear-transport' });
    }
    if (request.method === 'GET' && isFileResource(url.pathname)) return fileResponse(request);
    let safe = ['GET', 'HEAD'].includes(request.method) || /\/uploads\/[^/]+\/complete$/.test(url.pathname);
    if (request.method === 'POST' && url.pathname === '/api/uploads') {
      try { safe = Boolean((await request.clone().json()).idempotency_key); } catch {}
    }
    return bufferedRequest(request, safe);
  };
}
let watching = false;
// Some native media loaders leave a disconnected response without reporting
// an error or stall, even in NETWORK_IDLE. Reload only after playback has stopped
// advancing without playable future data, with a bounded budget per source.
// All bytes and Range decisions still belong to the browser.
export function watchNativeRecovery(root = document, delayMs = 3000) {
  const states = new WeakMap(), pending = new Set();
  const cancel = state => { clearTimeout(state.timer); state.timer = undefined; pending.delete(state); };
  const cancelRestore = (element, state) => {
    if (state.restore) element.removeEventListener('loadedmetadata', state.restore);
    state.restore = undefined;
  };
  const sourceOf = element => element.currentSrc || element.src;
  const bufferSeconds = element => {
    for (let i = 0; i < element.buffered.length; i++)
      if (element.buffered.start(i) <= element.currentTime && element.buffered.end(i) >= element.currentTime)
        return element.buffered.end(i) - element.currentTime;
    return 0;
  };
  const observe = event => {
    const element = event.target;
    if (!(element instanceof HTMLMediaElement)) return;
    const source = sourceOf(element);
    let url;
    try { url = new URL(source, location.href); } catch { return; }
    if (url.origin !== location.origin || !isFileResource(url.pathname)) return;
    let state = states.get(element);
    if (!state || state.source !== source) {
      if (state) { cancel(state); cancelRestore(element, state); }
      state = { source, attempts: 0, restoredAt: 0, waiting: false }; states.set(element, state);
    }
    if (event.type === 'timeupdate') {
      if (element.currentTime > state.restoredAt + 1 && bufferSeconds(element) > 1) state.attempts = 0;
      // A progress event can arrive after waiting/stalled. Keep checking until
      // the clock actually resumes, rather than relying on another stall event.
      if (element.currentTime > state.position + 0.1) {
        state.waiting = element.readyState < 3; cancel(state);
      }
      if (!state.waiting) return;
    }
    if (['pause', 'seeking', 'ended', 'emptied', 'playing', 'seeked'].includes(event.type)) {
      state.waiting = false;
      cancel(state);
      if (['pause', 'seeking', 'ended'].includes(event.type)) cancelRestore(element, state);
      return;
    }
    if (event.type === 'progress') {
      cancel(state);
      if (!state.waiting) return;
    }
    if (event.type === 'check' && bufferSeconds(element) > 0.15 && element.readyState >= 3 && !element.error) return;
    if (['waiting', 'stalled', 'error', 'check'].includes(event.type)) state.waiting = true;
    if (state.timer || state.attempts >= 2 || element.paused || element.ended || element.seeking
        || (element.error && element.error.code !== 2)) return;
    const position = element.currentTime;
    state.position = position;
    pending.add(state);
    state.timer = setTimeout(() => {
      cancel(state);
      if (!element.isConnected || sourceOf(element) !== source || element.paused || element.ended || element.seeking
          || element.currentTime > position + 0.1 || (bufferSeconds(element) > 0.15 && element.readyState >= 3)
          || (element.error && element.error.code !== 2)) return;
      // buffered may include demuxed bytes that cannot supply the next frame.
      // HAVE_FUTURE_DATA is required before treating that range as playable.
      state.waiting = false;
      state.attempts++; state.restoredAt = element.currentTime;
      const restore = () => {
        state.restore = undefined;
        if (!element.isConnected || sourceOf(element) !== source) return;
        element.currentTime = state.restoredAt;
        element.play().catch(() => {});
      };
      state.restore = restore;
      element.addEventListener('loadedmetadata', restore, { once: true });
      element.load();
    }, delayMs);
  };
  for (const event of ['waiting', 'stalled', 'error', 'pause', 'seeking', 'ended', 'emptied', 'playing', 'seeked', 'progress', 'timeupdate'])
    root.addEventListener(event, observe, true);
  globalThis.addEventListener?.('pagehide', () => { for (const state of pending) cancel(state); });
  return element => observe({ target: element, type: 'check' });
}
function watchPlayback() {
  if (watching) return; watching = true;
  const checkRecovery = watchNativeRecovery();
  const live = new Map(); let nextKey = 0, reportedAt = 0;
  const report = () => {
    reportedAt = Date.now();
    const present = new Set(document.querySelectorAll('audio:not([aria-hidden="true"]), video'));
    for (const [element, key] of live) if (!present.has(element)) {
      const state = { key, ended: true }; setPlaybackState(state);
      navigator.serviceWorker?.controller?.postMessage({ type: 'playback-state', state }); live.delete(element);
    }
    for (const element of present) {
      if (!element.currentSrc && !element.src) continue;
      checkRecovery(element);
      if (!live.has(element)) live.set(element, `media-${++nextKey}`);
      let bufferSeconds = 0;
      for (let i = 0; i < element.buffered.length; i++) {
        if (element.buffered.start(i) <= element.currentTime && element.buffered.end(i) >= element.currentTime)
          bufferSeconds = (element.buffered.end(i) - element.currentTime) / Math.max(0.25, element.playbackRate);
      }
      if (element.seeking) bufferSeconds = 0;
      const playing = !element.paused && !element.ended;
      if (playing && element.preload !== 'auto') element.preload = 'auto';
      const state = { key: live.get(element), url: element.currentSrc || element.src, playing, bufferSeconds };
      setPlaybackState(state);
      navigator.serviceWorker?.controller?.postMessage({ type: 'playback-state', state });
    }
  };
  for (const event of ['play', 'pause', 'waiting', 'seeking', 'seeked', 'ended']) document.addEventListener(event, report, true);
  for (const event of ['progress', 'timeupdate']) document.addEventListener(event, () => { if (Date.now() - reportedAt >= 1000) report(); }, true);
  setInterval(report, 2000);
  addEventListener('pagehide', () => {
    for (const key of live.values()) {
      const state = { key, ended: true }; setPlaybackState(state);
      navigator.serviceWorker?.controller?.postMessage({ type: 'playback-state', state });
    }
    live.clear();
  });
}
let initialized = false;
let workerReady = Promise.resolve();
export async function initializeTransport({ waitForWorker = false } = {}) {
  installAPITransport();
  if (initialized) {
    if (waitForWorker) await workerReady;
    return;
  }
  initialized = true;
  // Document transport works immediately; worker activation never holds up mounting.
  directFiles = true;
  watchPlayback();
  if (!globalThis.isSecureContext || !navigator.serviceWorker) return;
  let resolveClaim;
  const claimed = new Promise(resolve => { resolveClaim = resolve; });
  const applyController = () => {
    directFiles = !navigator.serviceWorker.controller;
    if (!directFiles) resolveClaim();
  };
  navigator.serviceWorker.addEventListener('controllerchange', applyController);
  applyController();
  // The classic worker keeps Firefox on the same policy as Chromium. A late
  // claim still takes over file requests after the startup deadline expires.
  workerReady = (async () => {
    let timer;
    try {
      await Promise.race([(async () => {
        await navigator.serviceWorker.register('/transport-worker.js', { scope: '/', updateViaCache: 'none' });
        const registration = await navigator.serviceWorker.ready;
        if (!navigator.serviceWorker.controller) registration.active?.postMessage({ type: 'claim-clients' });
        await claimed;
        applyController();
      })(), new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('worker unavailable')), 8000); })]);
    } catch {} finally { clearTimeout(timer); }
  })();
  // Public attachment navigation needs the worker's resumable stream before
  // it starts. Only that caller waits, with the same bounded native fallback.
  if (waitForWorker) await workerReady;
}
