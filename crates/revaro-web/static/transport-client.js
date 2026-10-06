import { putBlob, blobHash, configureTransport } from './transport-core.js';
export { putBlob, blobHash };
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
  // Uploads and the worker share policy and receive the same endpoint setting.
  try {
    const config = await (await fetch('/api/transport/config')).json();
    configureTransport(config);
    navigator.serviceWorker.controller.postMessage({ type: 'transport-config', config });
  } catch {}
}
