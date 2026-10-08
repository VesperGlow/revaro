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
  return new Response(data.slice(start, end + 1), { status: 206, headers: {
    'content-range': `bytes ${start}-${end}/${data.length}`, 'content-length': String(end - start + 1),
    etag, 'accept-ranges': 'bytes', 'content-type': mime, 'cache-control': 'private, no-cache',
  } });
}
function bounds(init) { return new Headers(init.headers).get('range').slice(6).split('-').map(Number); }
const request = (path = 'file.bin', init = {}) => new Request(new URL(`/api/files/${path}/download`, location), init);

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
    if (++calls === 2) {
      return new Promise((resolve, reject) => init.signal.addEventListener('abort', () => { aborted++; reject(init.signal.reason); }, { once: true }));
    }
    return bytesResponse(payload.slice(0, 2048), start, end);
  });
  const result = await (await core.fileResponse(request())).arrayBuffer();
  assert.deepEqual(new Uint8Array(result), payload.slice(0, 2048));
  assert.equal(calls, 3); assert.equal(aborted, 1);
});

test('changed ETag restarts before headers and never joins different versions', async () => {
  let version = 1;
  const core = await engine(async (url, init) => {
    const [start, end] = bounds(init);
    if (end > 0 && version === 1) { version = 2; return new Response(null, { status: 412 }); }
    const data = version === 1 ? payload : Uint8Array.from(payload, b => b ^ 255);
    return bytesResponse(data, start, end, `"version-${version}"`);
  });
  assert.deepEqual(new Uint8Array(await (await core.fileResponse(request())).arrayBuffer()), Uint8Array.from(payload, b => b ^ 255));
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
    for (const start of [5 * 1024 ** 3, 1024 ** 2]) {
      calls.length = 0;
      const response = await core.fileResponse(request('long-audio', { headers: { range: `bytes=${start}-` } }));
      assert.equal(response.status, 206);
      assert.equal(response.headers.get('content-range'), `bytes ${start}-${size - 1}/${size}`);
      assert.equal(response.headers.get('content-type'), mime);
      const reader = response.body.getReader();
      const { value } = await reader.read();
      assert.equal(value.length, core.policy.chunkBytes);
      assert.equal(value[0], start % 251);
      await reader.cancel();
      await response.transportComplete;
      assert.deepEqual(calls[0], [0, 0], 'one-byte authentication probe');
      assert.ok(calls.slice(1).every(([offset]) => offset >= start), 'no preceding audio is downloaded');
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
