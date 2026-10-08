import { test } from 'node:test';
import assert from 'node:assert/strict';

test('worker warms only with sufficient playback buffer, cancels on pressure and reuses warmed bytes', async () => {
  const listeners = new Map(), entries = new Map(), calls = [], pending = [];
  let hold = true, cancelled = 0;
  const data = Uint8Array.from({ length: 512 * 1024 }, (_, i) => i % 251);
  globalThis.location = new URL('http://localhost:8081/');
  globalThis.self = {
    location, addEventListener: (name, handler) => listeners.set(name, handler),
    clients: { claim: async () => {} }, skipWaiting: async () => {},
  };
  const cache = {
    match: async key => entries.get(String(key))?.clone(),
    put: async (key, value) => { entries.set(String(key), value.clone()); },
    keys: async () => [...entries.keys()], delete: async key => entries.delete(String(key)),
  };
  globalThis.caches = { open: async () => cache, delete: async () => { entries.clear(); return true; } };
  let started;
  let networkStarted = new Promise(resolve => { started = resolve; });
  globalThis.fetch = async (url, init) => {
    const range = new Headers(init.headers).get('range'); calls.push({ url, range }); started();
    if (hold) return new Promise((resolve, reject) => {
      init.signal.addEventListener('abort', () => { cancelled++; reject(init.signal.reason); }, { once: true });
    });
    const [start, requestedEnd] = range.slice(6).split('-').map(Number), end = Math.min(data.length - 1, requestedEnd);
    return new Response(data.slice(start, end + 1), { status: 206, headers: {
      'content-range': `bytes ${start}-${end}/${data.length}`, etag: '"next-track"',
      'content-length': String(end - start + 1), 'cache-control': 'private, no-cache',
    } });
  };
  const core = await import('../../crates/revaro-web/static/transport-core.js');
  await import('../../crates/revaro-web/static/transport-sw.js');
  const message = state => listeners.get('message')({
    data: { type: 'playback-state', state: { key: 'audio', nextUrl: '/api/files/next/preview?revaro_priority=background', ...state } },
    source: { id: 'player-tab' }, waitUntil: promise => pending.push(promise),
  });
  message({ playing: true, bufferSeconds: 19 });
  message({ playing: false, bufferSeconds: 30 });
  assert.equal(calls.length, 0, 'low buffer and paused playback do not prefetch');
  message({ playing: true, bufferSeconds: 25 }); await networkStarted;
  assert.equal(calls.length, 1);
  message({ playing: true, bufferSeconds: 25 });
  message({ playing: true, bufferSeconds: 5 });
  await Promise.all(pending.splice(0));
  assert.equal(cancelled, 1, 'buffer warning interrupts the background network request');
  hold = false;
  networkStarted = new Promise(resolve => { started = resolve; });
  message({ playing: true, bufferSeconds: 25 }); await networkStarted;
  await Promise.all(pending.splice(0));
  assert.ok(calls.slice(1).every(({ range }) => Number(range.split('-')[1]) < 256 * 1024), 'prefetch stays within the next introduction');
  const before = calls.length;
  message({ playing: true, bufferSeconds: 25 });
  assert.equal(pending.length, 0, 'completed prefetch is not repeatedly downloaded');
  const response = await core.fileResponse(new Request(new URL('/api/files/next/preview', location), { headers: { range: 'bytes=0-262143' } }));
  assert.deepEqual(new Uint8Array(await response.arrayBuffer()), data.slice(0, 256 * 1024));
  await response.transportComplete;
  assert.deepEqual(calls.slice(before).map(({ range }) => range), ['bytes=0-0'], 'formal playback revalidates and reuses the warmed blocks');
  message({ ended: true }); await core.clearTransportCache();
});
