import { test } from 'node:test';
import assert from 'node:assert/strict';
import { setTimeout as delay } from 'node:timers/promises';

test('optional worker/config discovery never blocks mounting or duplicates playback watchers', async () => {
  globalThis.location = new URL('http://localhost:8081/');
  globalThis.isSecureContext = true;
  let resolveConfig, resolveRegistration, resolveWorker, listener, watches = 0;
  const messages = [];
  const worker = {
    controller: null,
    addEventListener(type, callback) { if (type === 'controllerchange') listener = callback; },
    register: () => new Promise(resolve => { resolveRegistration = resolve; }),
    ready: new Promise(resolve => { resolveWorker = resolve; }),
  };
  Object.defineProperty(globalThis, 'navigator', { configurable: true, value: { serviceWorker: worker } });
  globalThis.document = { querySelectorAll: () => [], addEventListener: () => { watches++; } };
  globalThis.addEventListener = () => {};
  globalThis.setInterval = () => 1;
  const timeout = globalThis.setTimeout;
  globalThis.setTimeout = (...args) => timeout(...args).unref();
  globalThis.fetch = async input => {
    if (new URL(input.url || input).pathname === '/api/transport/config')
      return new Promise(resolve => { resolveConfig = resolve; });
    return new Response('{}');
  };
  const { initializeTransport } = await import('../../crates/revaro-web/static/transport-client.js');
  const start = performance.now();
  await initializeTransport();
  assert.ok(performance.now() - start < 100, 'mount only waits for synchronous setup');
  const firstWatches = watches;
  await initializeTransport();
  assert.equal(watches, firstWatches);
  assert.equal((await fetch(new Request(new URL('/api/session', location)))).status, 200);
  resolveConfig(new Response(JSON.stringify({ http2_origin: 'https://localhost:8443' })));
  await delay(10);
  let shareReady = false;
  const share = initializeTransport({ waitForWorker: true }).then(() => { shareReady = true; });
  resolveRegistration({});
  resolveWorker({ active: { postMessage: () => {} } });
  await delay(10);
  assert.equal(shareReady, false, 'attachment navigation waits for worker control');
  await initializeTransport();
  assert.equal(shareReady, false, 'ordinary mounting still does not wait');
  worker.controller = { postMessage: message => messages.push(message) };
  listener();
  await share;
  assert.equal(shareReady, true);
  assert.equal(messages.at(-1).type, 'transport-config');
  assert.equal(messages.at(-1).config.http2_origin, 'https://localhost:8443');
});
