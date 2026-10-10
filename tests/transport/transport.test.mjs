import { test } from 'node:test';
import assert from 'node:assert/strict';

let serial = 0;
class MemoryCache {
  entries = new Map();
  async match(key) { return this.entries.get(String(key))?.clone(); }
  async put(key, response) { this.entries.set(String(key), response.clone()); }
  async keys() { return [...this.entries.keys()]; }
  async delete(key) { return this.entries.delete(String(key)); }
}
async function engine(fetch, cache = new MemoryCache(), location = 'http://localhost:8081/') {
  globalThis.location = new URL(location);
  globalThis.fetch = fetch;
  globalThis.caches = { open: async () => cache, delete: async () => { cache.entries.clear(); return true; } };
  const core = await import(`../../crates/revaro-web/static/transport-core.js?test=${++serial}`);
  Object.assign(core.policy, { chunkBytes: 65536, smallBytes: 4096, idleMs: 25, headersMs: 35, hedgeMs: 15, backoffMs: 1, maxBackoffMs: 2 });
  return core;
}
const payload = Uint8Array.from({ length: 512 * 1024 + 123 }, (_, i) => (i * 31 + 7) % 251);
function bytesResponse(data, start, end, etag = '"version-1"', mime = 'application/octet-stream') {
  end = Math.min(end, data.length - 1);
  return new Response(data.slice(start, end + 1), { status: 206, headers: {
    'content-range': `bytes ${start}-${end}/${data.length}`, 'content-length': String(end - start + 1),
    etag, 'accept-ranges': 'bytes', 'content-type': mime, 'cache-control': 'private, no-cache',
  } });
}
function bounds(init) { return new Headers(init.headers).get('range').slice(6).split('-').map(Number); }
const request = (path = 'file.bin', init = {}) => new Request(new URL(`/api/files/${path}/download`, location), init);

test('deadlines recover when native fetch or stream cancellation ignores abort', { timeout: 2000 }, async () => {
  for (const stall of ['headers', 'body']) {
    let calls = 0;
    const core = await engine(async () => {
      if (++calls > 1) return new Response('{"ok":true}');
      if (stall === 'headers') return new Promise(() => {});
      return new Response(new ReadableStream({
        start(controller) { controller.enqueue(new TextEncoder().encode('{')); },
        cancel() { return new Promise(() => {}); },
      }));
    });
    const response = await core.bufferedRequest(new Request(new URL('/api/listening/session', location)));
    assert.deepEqual(await response.json(), { ok: true });
    assert.equal(calls, 2);
    assert.equal(core.transportState().activeRequests, 0, 'a stuck native request does not retain admission');
  }
});

test('arbitrary MIME types use 2–4 parallel lanes and resume only a stalled block', async () => {
  for (const mime of ['application/pdf', 'application/zip', 'text/plain', 'application/octet-stream', 'image/png', 'video/mp4', 'audio/flac']) {
    let stalled = false, active = 0, maximum = 0;
    const calls = [];
    const core = await engine(async (url, init) => {
      const [start, end] = bounds(init); calls.push([start, end]); active++; maximum = Math.max(maximum, active);
      if (start === 65536 && !stalled) {
        stalled = true;
        const headers = bytesResponse(payload, start, end, undefined, mime).headers;
        return new Response(new ReadableStream({ start(controller) {
          controller.enqueue(payload.slice(start, start + 8192));
          init.signal.addEventListener('abort', () => { active--; controller.error(init.signal.reason); }, { once: true });
        } }), { status: 206, headers });
      }
      await new Promise(r => setTimeout(r, 2)); active--;
      return bytesResponse(payload, start, end, undefined, mime);
    });
    const response = await core.fileResponse(request(mime.replaceAll('/', '-')));
    assert.deepEqual(new Uint8Array(await response.arrayBuffer()), payload, mime);
    await response.transportComplete;
    assert.equal(response.status, 200);
    assert.ok(maximum >= 2 && maximum <= 4, `${mime}: ${maximum} lanes`);
    assert.ok(calls.some(([start, end]) => start === 73728 && end === 131071), 'retry starts after the received prefix');
    assert.equal(calls.filter(([start, end]) => start === 0 && end === 65535).length, 1, 'healthy block was not retried');
  }
});

test('completed blocks survive a new engine instance, with a fresh auth/ETag probe', async () => {
  const cache = new MemoryCache(), calls = [];
  const network = async (url, init) => { const [start, end] = bounds(init); calls.push([start, end]); return bytesResponse(payload, start, end); };
  const first = await engine(network, cache);
  const response = await first.fileResponse(request()); await response.arrayBuffer();
  const second = await engine(network, cache); calls.length = 0;
  assert.deepEqual(new Uint8Array(await (await second.fileResponse(request())).arrayBuffer()), payload);
  assert.deepEqual(calls, [[0, 0]], 'only authentication/validator probe needed');
  const expired = await engine(async () => new Response('expired', { status: 401 }), cache);
  assert.equal((await expired.fileResponse(request())).status, 401);
  await expired.clearTransportCache(); assert.equal(cache.entries.size, 0);
});

test('15% transient failed requests recover without repeating healthy ranges', async () => {
  let sequence = 0, dropped = 0;
  const attempts = new Map();
  const core = await engine(async (url, init) => {
    const [start, end] = bounds(init), key = `${start}-${end}`;
    attempts.set(key, (attempts.get(key) || 0) + 1);
    if (++sequence % 7 === 0) { dropped++; throw new TypeError('simulated dropped connection'); }
    return bytesResponse(payload, start, end);
  });
  assert.deepEqual(new Uint8Array(await (await core.fileResponse(request())).arrayBuffer()), payload);
  assert.ok(dropped > 0); assert.ok([...attempts.values()].filter(n => n > 1).length <= dropped);
});

test('slow small bodies hedge; healthy response wins and cancels the stalled original', async () => {
  let calls = 0, aborted = 0;
  const core = await engine(async (url, init) => {
    const [start, end] = bounds(init);
    if (++calls === 1) {
      return new Promise((resolve, reject) => init.signal.addEventListener('abort', () => { aborted++; reject(init.signal.reason); }, { once: true }));
    }
    return bytesResponse(payload.slice(0, 2048), start, end);
  });
  const result = await (await core.fileResponse(new Request(new URL('/api/files/file.bin/preview', location)))).arrayBuffer();
  assert.deepEqual(new Uint8Array(result), payload.slice(0, 2048));
  assert.equal(calls, 2); assert.equal(aborted, 1);
});

test('changed ETag restarts before headers and never joins different versions', async () => {
  let version = 1;
  const core = await engine(async (url, init) => {
    const [start, end] = bounds(init);
    if (end > 0 && version === 1) { version = 2; return new Response(null, { status: 412 }); }
    const data = version === 1 ? payload : Uint8Array.from(payload, b => b ^ 255);
    return bytesResponse(data, start, end, `"version-${version}"`);
  });
  assert.deepEqual(new Uint8Array(await (await core.fileResponse(request('changed', { headers: { range: 'bytes=65536-' } }))).arrayBuffer()), Uint8Array.from(payload.slice(65536), b => b ^ 255));
});

test('native suffix ranges and If-Range follow HTTP semantics', async () => {
  const core = await engine(async (url, init) => { const [start, end] = bounds(init); return bytesResponse(payload, start, end); });
  const suffix = await core.fileResponse(request('file.bin', { headers: { range: 'bytes=-1024' } }));
  assert.equal(suffix.status, 206); assert.deepEqual(new Uint8Array(await suffix.arrayBuffer()), payload.slice(-1024));
  const changed = await core.fileResponse(request('file.bin', { headers: { range: 'bytes=7-31', 'if-range': '"old"' } }));
  assert.equal(changed.status, 200); assert.deepEqual(new Uint8Array(await changed.arrayBuffer()), payload);
});

test('HTTP/3 connection failure uses the configured HTTP/2-only authority', async () => {
  const urls = []; let failures = 0;
  const core = await engine(async (url, init) => {
    urls.push(url);
    if (new URL(url).port !== '8443' && failures++ < 2) throw new TypeError('QUIC path failed');
    const [start, end] = bounds(init); return bytesResponse(payload, start, end);
  }, undefined, 'https://files.example.test/');
  core.configureTransport({ http2_origin: 'https://files.example.test:8443' });
  assert.deepEqual(new Uint8Array(await (await core.fileResponse(request())).arrayBuffer()), payload);
  assert.ok(urls.some(url => new URL(url).port === '8443')); assert.equal(core.transportState().fallback, true);
});

test('long FLAC/WAV streams seek in bounded windows and cancel without downloading the file', async () => {
  const size = 8 * 1024 ** 3;
  for (const mime of ['audio/flac', 'audio/wav']) {
    const calls = [];
    const core = await engine(async (url, init) => {
      const [start, end] = bounds(init);
      calls.push([start, end]);
      const data = Uint8Array.from({ length: end - start + 1 }, (_, index) => (start + index) % 251);
      return new Response(data, { status: 206, headers: {
        'content-range': `bytes ${start}-${end}/${size}`, 'content-length': String(data.length),
        etag: '"long-audio"', 'accept-ranges': 'bytes', 'content-type': mime,
      } });
    });
    for (const start of [5 * 1024 ** 3, 1024 ** 2, 5 * 1024 ** 3 + 12345]) {
      calls.length = 0;
      const response = await core.fileResponse(request('long-audio', { headers: { range: `bytes=${start}-` } }));
      assert.equal(response.status, 206);
      assert.equal(response.headers.get('content-range'), `bytes ${start}-${size - 1}/${size}`);
      assert.equal(response.headers.get('content-type'), mime);
      const reader = response.body.getReader();
      const { value } = await reader.read();
      assert.equal(value.length, core.policy.blockBytes - start % core.policy.blockBytes);
      assert.equal(value[0], start % 251);
      await reader.cancel();
      await response.transportComplete;
      assert.deepEqual(calls[0], [0, 0], 'one-byte authentication probe');
      assert.ok(calls.slice(1).every(([offset]) => offset >= Math.floor(start / core.policy.blockBytes) * core.policy.blockBytes), 'only the containing block and following audio are downloaded');
      assert.ok(calls.length <= 8, 'cancellation bounds speculative download windows');
      assert.ok(calls.every(([offset, end]) => end - offset < core.policy.chunkBytes));
    }
  }
});

test('cancellation interrupts backoff immediately; permanent HTTP errors are not retried', async () => {
  const core = await engine(async () => { throw new Error('unused'); });
  const controller = new AbortController(); let attempts = 0;
  const pending = core.retry(async () => { attempts++; controller.abort(); throw new TypeError('offline'); }, { signal: controller.signal });
  await assert.rejects(pending, { name: 'AbortError' }); assert.equal(attempts, 1);
  await assert.rejects(core.retry(async () => { throw new core.TransferError('forbidden', 403); }), /forbidden/);
});

test('upload lost acknowledgements resend only that block and verify SHA-256', async () => {
  const core = await engine(async () => { throw new Error('uploads use XHR'); });
  const counts = new Map(), durable = new Map(), progress = [];
  globalThis.XMLHttpRequest = class {
    upload = {}; headers = new Map(); status = 0; cancelled = false;
    open(method, url) { this.url = url; }
    setRequestHeader(name, value) { this.headers.set(name.toLowerCase(), value); }
    getResponseHeader(name) { return name.toLowerCase() === 'etag' ? `block-${this.url.at(-1)}` : this.headers.get('x-content-sha256'); }
    abort() { this.cancelled = true; this.onabort?.(); }
    async send(body) {
      const count = (counts.get(this.url) || 0) + 1; counts.set(this.url, count);
      const hash = await core.blobHash(body); assert.equal(hash, this.headers.get('x-content-sha256'));
      assert.equal(this.headers.get('x-revaro-managed'), '1');
      durable.set(this.url, hash);
      this.upload.onprogress?.({ loaded: body.size }); this.upload.onload?.();
      if (this.url.endsWith('/2') && count === 1) this.onerror?.();
      else { this.status = 204; this.onload?.(); }
    }
  };
  const signal = new AbortController().signal;
  await Promise.all([1, 2, 3].map(number => core.putBlob(`/api/uploads/session/data/${number}`, new Blob([payload.slice(0, 1024 * number)]), null, signal, loaded => progress.push(loaded))));
  assert.equal(counts.get('http://localhost:8081/api/uploads/session/data/1'), 1);
  assert.equal(counts.get('http://localhost:8081/api/uploads/session/data/2'), 2);
  assert.equal(counts.get('http://localhost:8081/api/uploads/session/data/3'), 1);
  assert.equal(durable.size, 3); assert.ok(progress.includes(0));
});

test('cold small resources use one authenticated data request', async () => {
  const data = payload.slice(0, 3072), calls = [];
  const core = await engine(async (url, init) => {
    const [start, end] = bounds(init); calls.push([start, end]); return bytesResponse(data, start, end);
  });
  const response = await core.fileResponse(request('small'));
  assert.deepEqual(new Uint8Array(await response.arrayBuffer()), data);
  assert.equal(calls.length, 1, 'no separate one-byte round trip');
});

test('subsequent windows adapt to measured throughput instead of always fetching 512 KiB', async () => {
  const calls = [];
  const core = await engine(async (url, init) => {
    const [start, end] = bounds(init); calls.push([start, end]);
    await new Promise(resolve => setTimeout(resolve, 160));
    return bytesResponse(payload, start, end);
  });
  Object.assign(core.policy, { chunkBytes: 512 * 1024, headersMs: 1000, idleMs: 1000 });
  const response = await core.fileResponse(request('adaptive'));
  assert.deepEqual(new Uint8Array(await response.arrayBuffer()), payload);
  await response.transportComplete;
  assert.deepEqual(calls[0], [0, 65535]);
  assert.ok(calls.slice(1).some(([start, end]) => end - start + 1 > 65536 && end - start + 1 < 512 * 1024));
  assert.ok(calls.every(([start, end]) => end - start + 1 <= 512 * 1024));
});

test('slow persistent cache writes do not delay first bytes or body delivery', async () => {
  let release; const gate = new Promise(resolve => { release = resolve; });
  const cache = new MemoryCache();
  const put = cache.put.bind(cache); cache.put = async (...args) => { await gate; return put(...args); };
  const data = payload.slice(0, 2048);
  const core = await engine(async (url, init) => { const [start, end] = bounds(init); return bytesResponse(data, start, end); }, cache);
  try {
    const delivered = await Promise.race([
      core.fileResponse(request('slow-cache')).then(async response => new Uint8Array(await response.arrayBuffer())),
      new Promise((resolve, reject) => setTimeout(() => reject(new Error('cache blocked the response')), 500)),
    ]);
    assert.deepEqual(delivered, data);
    assert.ok(core.transportState().queuedCacheBytes > 0);
  } finally { release(); await core.clearTransportCache(); }
});

test('unaligned seeks reuse canonical blocks while always checking current permissions', async () => {
  const calls = [], cache = new MemoryCache();
  const network = async (url, init) => { const [start, end] = bounds(init); calls.push([start, end]); return bytesResponse(payload, start, end); };
  const core = await engine(network, cache);
  const first = await core.fileResponse(request('seek-cache', { headers: { range: 'bytes=65541-131068' } }));
  assert.deepEqual(new Uint8Array(await first.arrayBuffer()), payload.slice(65541, 131069)); await first.transportComplete;
  calls.length = 0;
  const second = await core.fileResponse(request('seek-cache', { headers: { range: 'bytes=70000-90000' } }));
  assert.deepEqual(new Uint8Array(await second.arrayBuffer()), payload.slice(70000, 90001));
  assert.deepEqual(calls, [[0, 0]], 'the overlapping seek only reauthenticates');
  const revoked = await engine(async () => new Response('revoked', { status: 403 }), cache);
  assert.equal((await revoked.fileResponse(request('seek-cache', { headers: { range: 'bytes=70000-90000' } }))).status, 403);
});

test('concurrent consumers share a block and one cancellation does not abort the other', async () => {
  let release, started; const gate = new Promise(resolve => { release = resolve; }), entered = new Promise(resolve => { started = resolve; });
  let ranges = 0, aborts = 0;
  const core = await engine(async (url, init) => {
    const [start, end] = bounds(init);
    if (start >= 65536) {
      ranges++; started();
      await Promise.race([gate, new Promise((resolve, reject) => init.signal.addEventListener('abort', () => { aborts++; reject(init.signal.reason); }, { once: true }))]);
    }
    return bytesResponse(payload, start, end);
  });
  const cancelled = new AbortController();
  const first = core.fileResponse(request('shared', { signal: cancelled.signal, headers: { range: 'bytes=65536-131071' } })).catch(error => error);
  await entered;
  const second = core.fileResponse(request('shared', { headers: { range: 'bytes=65541-131068' } }));
  await new Promise(resolve => setTimeout(resolve, 5)); cancelled.abort(); release();
  assert.equal((await first).name, 'AbortError');
  assert.deepEqual(new Uint8Array(await (await second).arrayBuffer()), payload.slice(65541, 131069));
  assert.equal(ranges, 1);
  // A completed fetch is cancelled when its reader is disposed; the important
  // invariant is that no live shared request was aborted before gate release.
  assert.ok(aborts <= 1);
});

test('next-track prefetch and normal playback share the same cache identity', async () => {
  const calls = [];
  const core = await engine(async (url, init) => { const [start, end] = bounds(init); calls.push([start, end]); return bytesResponse(payload, start, end); });
  const warm = await core.fileResponse(new Request(new URL('/api/files/next/preview?revaro_priority=background', location), {
    headers: { range: 'bytes=0-131071', 'x-revaro-priority': 'background' },
  }));
  await warm.arrayBuffer(); await warm.transportComplete; calls.length = 0;
  const played = await core.fileResponse(new Request(new URL('/api/files/next/preview', location), { headers: { range: 'bytes=0-131071' } }));
  assert.deepEqual(new Uint8Array(await played.arrayBuffer()), payload.slice(0, 131072));
  assert.deepEqual(calls, [[0, 0]]);
});

test('buffer pressure bounds bulk requests and admits a foreground click ahead of queued bulk work', async () => {
  const gates = [], order = [];
  const core = await engine(async (url, init) => {
    const name = new URL(url).pathname.split('/').at(-2); order.push(name);
    await new Promise(resolve => gates.push(resolve));
    return new Response('ok', { headers: { 'content-length': '2' } });
  });
  core.setPlaybackState({ key: 'player', playing: true, bufferSeconds: 2 });
  const bulk = [1, 2, 3, 4].map(number => core.bufferedRequest(request(`bulk-${number}`)));
  await new Promise(resolve => setTimeout(resolve, 5));
  assert.equal(order.length, 2, 'only two bulk requests compete with the starving player');
  const foreground = core.bufferedRequest(new Request(new URL('/api/files/click/preview', location)));
  await new Promise(resolve => setTimeout(resolve, 5));
  assert.equal(order[2], 'click');
  while (gates.length) gates.shift()();
  for (let i = 0; i < 4; i++) { await new Promise(resolve => setTimeout(resolve, 3)); while (gates.length) gates.shift()(); }
  await Promise.all([...bulk, foreground]);
  assert.equal(core.transportState().activeRequests, 0);
});

test('a stalled primary races the backup before the ordinary header deadline', async () => {
  const calls = [];
  const core = await engine(async (url, init) => {
    calls.push(url);
    if (new URL(url).port !== '8443') {
      return new Promise((resolve, reject) => init.signal.addEventListener('abort', () => reject(init.signal.reason), { once: true }));
    }
    const [start, end] = bounds(init); return bytesResponse(payload.slice(0, 2048), start, end);
  }, undefined, 'https://files.example.test/');
  core.configureTransport({ http2_origin: 'https://files.example.test:8443' });
  core.policy.headersMs = 500;
  const before = Date.now();
  assert.deepEqual(new Uint8Array(await (await core.fileResponse(new Request('https://files.example.test/api/files/failover/preview'))).arrayBuffer()), payload.slice(0, 2048));
  assert.ok(Date.now() - before < core.policy.headersMs);
  assert.equal(core.transportState().fallback, true);
  assert.equal(calls.length, 2);
});

test('no-store resources never write body blocks or persistent descriptors', async () => {
  const cache = new MemoryCache();
  const core = await engine(async (url, init) => {
    const [start, end] = bounds(init), response = bytesResponse(payload.slice(0, 2048), start, end);
    response.headers.set('cache-control', 'no-store'); return response;
  }, cache);
  const response = await core.fileResponse(request('private-share'));
  await response.arrayBuffer(); await response.transportComplete;
  assert.equal(cache.entries.size, 0); assert.equal(core.transportState().memoryBytes, 0);
});

test('cache clearing during an authenticated probe prevents late cache repopulation', async () => {
  const cache = new MemoryCache(); let release, started;
  const waiting = new Promise(resolve => { started = resolve; });
  const core = await engine(async (url, init) => {
    const [start, end] = bounds(init); started();
    await new Promise(resolve => { release = resolve; });
    return bytesResponse(payload, start, end);
  }, cache);
  const response = core.fileResponse(request('logout-race'));
  const rejected = assert.rejects(response, { name: 'AbortError' });
  await waiting; await core.clearTransportCache(); release(); await rejected;
  assert.equal(cache.entries.size, 0);
  assert.equal(core.transportState().memoryBytes, 0);
});

test('cache clearing also prevents a late disk read from restoring private memory blocks', async () => {
  const cache = new MemoryCache();
  const network = async (url, init) => { const [start, end] = bounds(init); return bytesResponse(payload, start, end); };
  const first = await engine(network, cache);
  const warm = await first.fileResponse(request('delayed-cache', { headers: { range: 'bytes=65536-131071' } }));
  await warm.arrayBuffer(); await warm.transportComplete;
  let release, started;
  const entered = new Promise(resolve => { started = resolve; }), gate = new Promise(resolve => { release = resolve; });
  cache.match = async key => {
    const hit = await MemoryCache.prototype.match.call(cache, key);
    if (new URL(key).pathname === '/.revaro-transport-cache') { started(); await gate; }
    return hit;
  };
  const core = await engine(network, cache);
  const response = core.fileResponse(request('delayed-cache', { headers: { range: 'bytes=65540-131070' } }));
  const rejected = assert.rejects(response, { name: 'AbortError' });
  await entered; await core.clearTransportCache(); release(); await rejected;
  assert.equal(core.transportState().memoryBytes, 0);
});
