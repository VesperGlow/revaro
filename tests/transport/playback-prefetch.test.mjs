import { test } from 'node:test';
import assert from 'node:assert/strict';

test('native playback reports pressure without redundant worker prefetch', async () => {
  const listeners = new Map(), calls = [];
  globalThis.location = new URL('http://localhost:8081/');
  globalThis.self = { location, addEventListener: (name, handler) => listeners.set(name, handler) };
  globalThis.fetch = async request => { calls.push(request); return new Response('native bytes'); };
  const core = await import('../../crates/revaro-web/static/transport-core.js');
  await import('../../crates/revaro-web/static/transport-sw.js');
  const message = state => listeners.get('message')({
    data: { type: 'playback-state', state: { key: 'audio', nextUrl: '/api/files/next/preview', ...state } },
    source: { id: 'player-tab' }, waitUntil: () => { throw new Error('native playback must not start worker prefetch'); },
  });
  message({ playing: true, bufferSeconds: 25 });
  message({ playing: true, bufferSeconds: 5 });
  assert.equal(core.transportState().mediaPressure, true);
  message({ ended: true });
  assert.equal(core.transportState().mediaPressure, false);
  assert.equal(calls.length, 0, 'native HTTP byte caching owns future reuse');

  const request = new Request(new URL('/api/files/current/preview', location), { headers: { range: 'bytes=0-' } });
  Object.defineProperty(request, 'destination', { value: 'video' });
  listeners.get('fetch')({ request, respondWith: () => { throw new Error('the browser must own native loading and HTTP cache merging'); },
    waitUntil: () => { throw new Error('unwrapped native body requires no worker lifetime extension'); } });
  assert.deepEqual(calls, [], 'no worker-owned re-fetch, config or permission probe');
});
