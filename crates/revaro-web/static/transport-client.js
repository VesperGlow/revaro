import { putBlob, blobHash, configureTransport, setPlaybackState } from './transport-core.js';
export { putBlob, blobHash };
let watching = false;
function watchPlayback() {
  if (watching) return; watching = true;
  const live = new Map(); let nextKey = 0, reportedAt = 0;
  const report = () => {
    reportedAt = Date.now();
    const present = new Set(document.querySelectorAll('audio:not([aria-hidden="true"]), video'));
    for (const [element, key] of live) if (!present.has(element)) {
      const state = { key, ended: true }; setPlaybackState(state);
      navigator.serviceWorker.controller?.postMessage({ type: 'playback-state', state }); live.delete(element);
    }
    for (const element of present) {
      if (!element.currentSrc && !element.src) continue;
      if (!live.has(element)) live.set(element, `media-${++nextKey}`);
      let bufferSeconds = 0;
      for (let i = 0; i < element.buffered.length; i++) {
        if (element.buffered.start(i) <= element.currentTime && element.buffered.end(i) >= element.currentTime)
          bufferSeconds = (element.buffered.end(i) - element.currentTime) / Math.max(0.25, element.playbackRate);
      }
      if (element.seeking) bufferSeconds = 0;
      const playing = !element.paused && !element.ended;
      if (playing) element.preload = 'auto';
      const state = { key: live.get(element), url: element.currentSrc || element.src, playing, bufferSeconds,
        nextUrl: element.tagName === 'AUDIO' ? document.querySelector('audio[data-revaro-next]')?.src : '' };
      setPlaybackState(state);
      navigator.serviceWorker.controller?.postMessage({ type: 'playback-state', state });
    }
  };
  for (const event of ['play', 'pause', 'waiting', 'seeking', 'seeked', 'ended']) document.addEventListener(event, report, true);
  for (const event of ['progress', 'timeupdate']) document.addEventListener(event, () => { if (Date.now() - reportedAt >= 1000) report(); }, true);
  setInterval(report, 2000);
  addEventListener('pagehide', () => {
    for (const key of live.values()) {
      const state = { key, ended: true }; setPlaybackState(state);
      navigator.serviceWorker.controller?.postMessage({ type: 'playback-state', state });
    }
    live.clear();
  });
}
export async function initializeTransport() {
  if (!globalThis.isSecureContext || !navigator.serviceWorker)
    throw new Error('文件传输需要 HTTPS 或 localhost，以启用统一传输层。');
  const ready = new Promise(resolve => {
    if (navigator.serviceWorker.controller) return resolve();
    navigator.serviceWorker.addEventListener('controllerchange', () => resolve(), { once: true });
  });
  // xtask emits a classic worker from the same shared core. Firefox versions
  // without module service workers must use the identical recovery policy.
  await navigator.serviceWorker.register('/transport-worker.js', { scope: '/', updateViaCache: 'none' });
  const registration = await navigator.serviceWorker.ready;
  if (!navigator.serviceWorker.controller) registration.active?.postMessage({ type: 'claim-clients' });
  await ready;
  watchPlayback();
  // Uploads and the worker share policy and receive the same endpoint setting.
  try {
    const config = await (await fetch('/api/transport/config')).json();
    configureTransport(config);
    navigator.serviceWorker.controller.postMessage({ type: 'transport-config', config });
  } catch {}
}
