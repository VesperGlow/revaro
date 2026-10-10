import { test } from 'node:test';
import assert from 'node:assert/strict';

test('8 GiB concurrent random seeks share ranges, recover a prefix and hit cache within a byte budget', async context => {
  const size = 8 * 1024 ** 3, blockBytes = 65536, cache = new Map(), calls = [];
  let seed = 42, delivered = 0, active = 0, maximum = 0, faulted = false;
  const offsets = Array.from({ length: 16 }, () => {
    seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
    return (1 + seed % (size / blockBytes - 1)) * blockBytes;
  });
  globalThis.location = new URL('http://localhost:8081/');
  globalThis.caches = {
    open: async () => ({ match: async key => cache.get(key)?.clone(),
      put: async (key, response) => cache.set(key, response.clone()),
      keys: async () => [...cache.keys()], delete: async key => cache.delete(key) }),
  };
  globalThis.fetch = async (url, init) => {
    const [start, end] = new Headers(init.headers).get('range').slice(6).split('-').map(Number);
    calls.push([start, end]);
    const failed = start === offsets[0] && !faulted;
    if (failed) faulted = true;
    const length = failed ? 32768 : end - start + 1;
    active++; maximum = Math.max(maximum, active);
    const body = new ReadableStream({ start(output) {
      setTimeout(() => {
        const bytes = Uint8Array.from({ length }, (_, index) => (start + index) % 251);
        delivered += bytes.length; output.enqueue(bytes); active--;
        if (failed) setTimeout(() => output.error(new TypeError('weak link disconnected')), 2);
        else output.close();
      }, 4);
    } });
    return new Response(body, { status: 206, headers: {
      'content-range': `bytes ${start}-${end}/${size}`, 'content-length': String(end - start + 1),
      etag: '"large-original"', 'cache-control': 'private, no-cache',
    } });
  };
  const core = await import('../../crates/revaro-web/static/transport-core.js');
  Object.assign(core.policy, { backoffMs: 1, maxBackoffMs: 2, headersMs: 1000, idleMs: 1000 });
  core.setPlaybackState({ key: 'foreground-player', playing: true, bufferSeconds: 1 });
  const read = async start => {
    const response = await core.fileResponse(new Request(new URL('/api/files/random/download', location), {
      headers: { range: `bytes=${start}-${start + 1023}` },
    }));
    assert.equal(response.status, 206);
    const bytes = new Uint8Array(await response.arrayBuffer());
    assert.deepEqual(bytes, Uint8Array.from({ length: 1024 }, (_, i) => (start + i) % 251));
    await response.transportComplete;
  };
  const began = performance.now();
  for (let group = 0; group < offsets.length; group += 4)
    await Promise.all(offsets.slice(group, group + 4).flatMap(offset => [0, 64, 128, 192].map(delta => read(offset + delta))));
  const coldBodies = calls.filter(([start, end]) => start !== 0 || end !== 0);
  assert.ok(faulted);
  assert.ok(maximum <= 2, 'low media buffer limits background network concurrency');
  assert.equal(coldBodies.length, offsets.length + 1, 'one canonical range per block plus one partial recovery');
  assert.ok(coldBodies.some(([start, end]) => start === offsets[0] + 32768 && end === offsets[0] + 65535));
  const beforeWarm = delivered, beforeCalls = calls.length;
  await Promise.all(offsets.map(offset => read(offset + 512)));
  assert.equal(delivered - beforeWarm, offsets.length, 'warm random reads transfer authentication bytes only');
  assert.ok(calls.slice(beforeCalls).every(([start, end]) => start === 0 && end === 0));
  assert.ok(delivered <= offsets.length * blockBytes + 80, 'no healthy bytes are retransmitted');
  assert.ok(core.transportState().metrics.cacheHits >= offsets.length);
  assert.equal(core.transportState().activeRequests, 0);
  context.diagnostic(JSON.stringify({ objectBytes: size, logicalReads: 80, networkRequests: calls.length,
    networkBytes: delivered, maxConcurrent: maximum, cacheHits: core.transportState().metrics.cacheHits,
    elapsedMs: Math.round(performance.now() - began) }));
});
