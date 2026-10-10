import { test } from 'node:test';
import assert from 'node:assert/strict';

let serial = 0;
async function engine(network) {
  globalThis.location = new URL('http://localhost:8081/');
  globalThis.fetch = network;
  globalThis.caches = { open: () => { throw new Error('native requests must keep the browser HTTP cache'); } };
  return import(`../../crates/revaro-web/static/transport-core.js?native=${++serial}`);
}
function request(destination, headers = {}) {
  const result = new Request(new URL('/api/files/original/preview', location), { headers });
  Object.defineProperty(result, 'destination', { value: destination });
  return result;
}

test('native high-bitrate reads stream a large open-ended Range without amplification or buffering', async () => {
  let reads = 0, cancelled = 0;
  const calls = [], size = 32 * 1024 ** 3;
  const response = new Response(new ReadableStream({
    pull(output) { reads++; output.enqueue(new Uint8Array(1024 * 1024)); },
    cancel() { cancelled++; },
  }, { highWaterMark: 0 }), { status: 206, headers: {
    'content-range': `bytes 0-${size - 1}/${size}`, 'content-length': String(size),
    etag: '"high-bitrate"', 'cache-control': 'private, no-cache',
  } });
  const core = await engine(async original => { calls.push(original); return response; });
  const original = request('video', { range: 'bytes=0-' });
  assert.equal(await core.fileResponse(original), response, 'the original body is returned unchanged');
  assert.equal(reads, 0, 'headers resolve without waiting for file bytes');
  const reader = response.body.getReader();
  for (let i = 0; i < 16; i++) assert.equal((await reader.read()).value.length, 1024 * 1024);
  assert.deepEqual(calls, [original], '16 MiB playback needs one network request');
  assert.equal(reads, 16, 'downstream demand controls native reads');
  await reader.cancel();
  assert.equal(cancelled, 1);
  assert.equal(core.transportState().metrics.nativeRequests, 1);
  assert.equal(core.transportState().metrics.networkRequests, 0);
});

test('native consumers preserve conditions, HTTP status, body and cache behavior', async () => {
  for (const destination of ['audio', 'video', 'image']) {
    for (const status of [200, 206, 304, 403, 404, 416]) {
      const response = new Response(status === 304 ? null : 'original', { status, headers: {
        etag: '"current"', 'content-range': status === 416 ? 'bytes */8' : 'bytes 0-7/8',
      } });
      const original = request(destination, { range: 'bytes=-8', 'if-range': '"current"', 'if-none-match': '"cached"' });
      const core = await engine(async forwarded => {
        assert.equal(forwarded, original);
        assert.equal(forwarded.cache, 'default');
        return response;
      });
      assert.equal(await core.fileResponse(original), response);
      assert.equal(response.status, status);
      await response.body?.cancel();
    }
  }
});

test('native cancellation and network failure propagate once without application retries', async () => {
  let calls = 0;
  const controller = new AbortController();
  const original = new Request(new URL('/api/files/media/preview', location), { signal: controller.signal });
  Object.defineProperty(original, 'destination', { value: 'audio' });
  const core = await engine(async forwarded => {
    calls++;
    return new Promise((_, reject) => forwarded.signal.addEventListener('abort', () => reject(forwarded.signal.reason), { once: true }));
  });
  const result = core.fileResponse(original);
  controller.abort();
  await assert.rejects(result, { name: 'AbortError' });
  assert.equal(calls, 1);
  assert.equal(core.transportState().metrics.nativeFailures, 1);

  const failed = await engine(async () => { calls++; throw new TypeError('connection lost'); });
  await assert.rejects(failed.fileResponse(request('video')), TypeError);
  assert.equal(calls, 2);
  assert.equal(failed.transportState().metrics.retries, 0);
});
