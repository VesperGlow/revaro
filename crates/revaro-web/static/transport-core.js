// All recovery policy lives here. The worker and browser upload adapter use
// the same implementation; neither MIME types nor file extensions select it.
export const policy = {
  chunkBytes: 512 * 1024, smallBytes: 256 * 1024, lanes: 3,
  firstBytes: 64 * 1024, blockBytes: 64 * 1024, windowMs: 500,
  headersMs: 15000, idleMs: 8000, hedgeMs: 1200,
  failoverHeadersMs: 3000, mediaIdleMs: 2500,
  attempts: 10, backoffMs: 400, maxBackoffMs: 8000,
  cacheEntries: 2048, memoryBytes: 8 * 1024 * 1024, cacheWriteBytes: 8 * 1024 * 1024,
  fallbackMs: 60000, progressWindowMs: 20000, minWindowBytes: 8192,
};
let http2Origin = '', fallbackUntil = 0, protocol = '', throughput = 0;
const paths = new Map(), playback = new Map();
const cacheName = 'revaro.transport.v2';
let cacheWrites = Promise.resolve();
let queuedCacheBytes = 0, memoryBytes = 0, cacheGeneration = 0;
const memory = new Map(), descriptors = new Map(), flights = new Map();
const resourceControllers = new Set();
let diskKeys;
let activeRequests = 0, backgroundRequests = 0, activeHedges = 0;
const requestQueue = [];
function mediaPressure() {
  const now = Date.now();
  for (const [key, state] of playback) if (now - state.updated > 15000) playback.delete(key);
  return [...playback.values()].some(state => state.playing && state.bufferSeconds < 15);
}
export function setPlaybackState(state) {
  if (!state?.key) return;
  if (state.ended) playback.delete(state.key);
  else playback.set(state.key, { ...state, bufferSeconds: Math.max(0, Number(state.bufferSeconds) || 0), updated: Date.now() });
  drainRequests();
}
function priorityOf(request) {
  const url = new URL(request.url);
  const explicit = request.headers.get('x-revaro-priority') || url.searchParams.get('revaro_priority');
  if (explicit === 'background') return 4;
  if (explicit === 'media' || ['audio', 'video'].includes(request.destination)) return 0;
  if (url.pathname.endsWith('/download') || url.pathname.includes('/batch-download/')) return 4;
  if (url.pathname.endsWith('/thumbnail')) return 3;
  return 1;
}
function canAdmit(priority) {
  const pressure = mediaPressure();
  const limit = pressure ? 8 : (protocol.startsWith('h3') ? 16 : 12);
  return activeRequests < limit && (priority < 3 || backgroundRequests < (pressure ? 2 : Math.max(2, limit - 4)));
}
function drainRequests() {
  requestQueue.sort((a, b) => a.priority - b.priority || a.queued - b.queued);
  for (;;) {
    const index = requestQueue.findIndex(entry => canAdmit(entry.priority));
    if (index < 0) return;
    const [entry] = requestQueue.splice(index, 1);
    activeRequests++; if (entry.priority >= 3) backgroundRequests++;
    entry.resolve();
  }
}
async function admission(signal, priority = 1) {
  checkAbort(signal);
  if (!canAdmit(priority) || requestQueue.length) {
    await new Promise((resolve, reject) => {
      const entry = { priority, queued: Date.now(), resolve: () => { signal?.removeEventListener('abort', abort); resolve(); } };
      const abort = () => { const index = requestQueue.indexOf(entry); if (index >= 0) requestQueue.splice(index, 1); reject(signal.reason || abortError()); };
      requestQueue.push(entry); signal?.addEventListener('abort', abort, { once: true });
      drainRequests();
    });
  } else { activeRequests++; if (priority >= 3) backgroundRequests++; }
  let released = false;
  return () => {
    if (released) return; released = true;
    activeRequests--; if (priority >= 3) backgroundRequests--;
    drainRequests();
  };
}
const networkFetch = globalThis.fetch.bind(globalThis);

export function configureTransport(config = {}) {
  if (Object.hasOwn(config, 'http2_origin') && !config.http2_origin) { http2Origin = ''; fallbackUntil = 0; }
  if (config.http2_origin) {
    const url = new URL(config.http2_origin);
    const here = new URL(globalThis.location.href);
    // Host-only session cookies work on another port, never another host.
    if (url.protocol === 'https:' && url.hostname === here.hostname && url.origin !== here.origin)
      http2Origin = url.origin;
  }
}
export function transportState() {
  return { protocol, fallback: fallbackUntil > Date.now(), http2Origin, throughput,
    activeRequests, backgroundRequests, queuedRequests: requestQueue.length,
    mediaPressure: mediaPressure(), memoryBytes, queuedCacheBytes,
    paths: Object.fromEntries(paths) };
}
function endpoint(url, origin) {
  const target = new URL(url, globalThis.location?.href);
  if (origin) return origin + target.pathname + target.search;
  if (http2Origin && fallbackUntil > Date.now())
    return http2Origin + target.pathname + target.search;
  return target.href;
}
function noteFailure(error) {
  if (error.status && error.status !== 408) return;
  const origin = error.origin || new URL(endpoint(globalThis.location.href)).origin;
  const state = paths.get(origin) || { failures: 0 };
  state.failures += error.failedAttempts || 1; state.failedAt = Date.now(); paths.set(origin, state);
  if (!http2Origin) return;
  if (origin === http2Origin) fallbackUntil = 0;
  else if (error.status === 408 || state.failures >= 2) fallbackUntil = Date.now() + policy.fallbackMs;
}
function noteSuccess(url, length = 0, elapsed = 0) {
  const origin = new URL(url).origin, state = paths.get(origin) || {};
  state.failures = 0; state.succeededAt = Date.now(); paths.set(origin, state);
  if (length >= policy.blockBytes && elapsed > 0) {
    const sample = length * 1000 / elapsed;
    throughput = throughput ? throughput * 0.75 + sample * 0.25 : sample;
  }
  const entries = globalThis.performance?.getEntriesByName?.(url);
  const hint = entries?.at(-1)?.nextHopProtocol;
  if (hint) state.protocol = hint;
  protocol = state.protocol || (origin === http2Origin ? 'h2' : protocol);
}
const abortError = () => new DOMException('Transfer cancelled', 'AbortError');
function checkAbort(signal) { if (signal?.aborted) throw signal.reason || abortError(); }
// Browser cancellation is advisory for some native/keepalive requests. The
// recovery deadline must settle even when fetch, read or cancel never does.
function abortable(operation, signal, onLate) {
  return new Promise((resolve, reject) => {
    let settled = false;
    const finish = (callback, value) => {
      if (settled) { if (callback === resolve) onLate?.(value); return; }
      settled = true; signal.removeEventListener('abort', abort); callback(value);
    };
    const abort = () => finish(reject, signal.reason || abortError());
    signal.addEventListener('abort', abort, { once: true });
    if (signal.aborted) abort();
    Promise.resolve(operation).then(value => finish(resolve, value), error => finish(reject, error));
  });
}
function cancelBody(body) { try { body?.cancel().catch(() => {}); } catch {} }
function linkedController(signal) {
  const controller = new AbortController();
  const abort = () => controller.abort(signal.reason || abortError());
  if (signal?.aborted) abort(); else signal?.addEventListener('abort', abort, { once: true });
  return { controller, dispose: () => signal?.removeEventListener('abort', abort) };
}
export class TransferError extends Error {
  constructor(message, status = 0, retryAfter = 0) {
    super(message); this.name = 'TransferError'; this.status = status; this.retryAfter = retryAfter;
  }
}
function retryable(error) {
  return error.name !== 'AbortError' && (!error.status || [408, 425, 429, 500, 502, 503, 504].includes(error.status));
}
export function delay(ms, signal) {
  checkAbort(signal);
  return new Promise((resolve, reject) => {
    const abort = () => { clearTimeout(timer); reject(signal.reason || abortError()); };
    const timer = setTimeout(() => { signal?.removeEventListener('abort', abort); resolve(); }, ms);
    signal?.addEventListener('abort', abort, { once: true });
  });
}
export async function retry(operation, { signal, attempts = policy.attempts } = {}) {
  for (let attempt = 0; ; attempt++) {
    checkAbort(signal);
    try { return await operation(attempt); }
    catch (error) {
      checkAbort(signal);
      if (!retryable(error) || attempt + 1 >= attempts) throw error;
      noteFailure(error);
      const ceiling = Math.min(policy.maxBackoffMs, policy.backoffMs * 2 ** Math.min(attempt, 6));
      await delay(Math.max(error.retryAfter || 0, ceiling * (0.5 + Math.random() * 0.5)), signal);
    }
  }
}
// A hedge wins only after the whole body has been read and validated. A fast
// header followed by a stalled body must not cancel the healthy alternative.
export async function hedge(operation, signal, enabled = true) {
  if (!enabled || activeHedges >= 2 || mediaPressure() || requestQueue.length) return operation(signal);
  activeHedges++;
  const first = linkedController(signal), second = linkedController(signal);
  let timer, startSecond, started = false;
  const secondary = new Promise((resolve, reject) => {
    startSecond = () => {
      if (started) return; started = true;
      const alternate = http2Origin && fallbackUntil <= Date.now() ? http2Origin : undefined;
      operation(second.controller.signal, alternate).then(resolve, reject);
    };
    timer = setTimeout(startSecond, protocol.startsWith('h3') ? policy.hedgeMs * 0.6 : policy.hedgeMs);
  });
  const primary = operation(first.controller.signal).catch(error => {
    clearTimeout(timer); startSecond(); throw error;
  });
  try {
    const result = await Promise.any([primary, secondary]);
    if (result?.origin === http2Origin && http2Origin) fallbackUntil = Date.now() + policy.fallbackMs;
    return result;
  }
  catch (error) {
    const selected = error.errors?.find(e => e.name !== 'AbortError') || error;
    selected.failedAttempts = error.errors?.filter(e => e.name !== 'AbortError').length || 1;
    throw selected;
  }
  finally {
    clearTimeout(timer); first.controller.abort(); second.controller.abort();
    first.dispose(); second.dispose(); activeHedges--;
  }
}
function retryAfter(response) {
  const value = response.headers.get('retry-after');
  if (!value) return 0;
  const seconds = Number(value);
  return Math.min(60000, Math.max(0, Number.isFinite(seconds) ? seconds * 1000 : Date.parse(value) - Date.now()));
}
async function attempt(url, init, { signal, validate, onBytes, maxBytes = 16 * 1024 * 1024,
  headersMs = policy.headersMs, priority = 1, origin } = {}) {
  const release = await admission(signal, priority);
  const { controller, dispose } = linkedController(signal);
  let timer, reader, stalled = false;
  const arm = ms => {
    clearTimeout(timer);
    timer = setTimeout(() => { stalled = true; controller.abort(); }, ms);
  };
  const target = endpoint(url, origin), startedAt = Date.now();
  try {
    arm(headersMs === policy.headersMs && http2Origin && new URL(target).origin !== http2Origin ? Math.min(headersMs, policy.failoverHeadersMs) : headersMs);
    const headers = new Headers(init.headers);
    headers.delete('x-revaro-priority');
    headers.set('priority', `u=${Math.min(7, priority + 1)}, i`);
    const response = await abortable(networkFetch(target, { ...init, headers, signal: controller.signal, credentials: 'include', cache: 'no-store' }),
      controller.signal, late => cancelBody(late.body));
    if ([408, 425, 429, 500, 502, 503, 504].includes(response.status)) {
      cancelBody(response.body);
      throw new TransferError(`HTTP ${response.status}`, response.status, retryAfter(response));
    }
    validate?.(response);
    if (!response.body) return { response, bytes: new Uint8Array() };
    reader = response.body.getReader();
    const chunks = []; let length = 0, windowBytes = 0, windowStart = Date.now();
    while (true) {
      const idle = priority === 0 && http2Origin ? Math.min(policy.idleMs, policy.mediaIdleMs) : policy.idleMs;
      arm(protocol.startsWith('h3') ? idle * 0.65 : idle);
      const { value, done } = await abortable(reader.read(), controller.signal);
      if (done) break;
      length += value.byteLength;
      windowBytes += value.byteLength;
      if (Date.now() - windowStart >= policy.progressWindowMs) {
        if (windowBytes < policy.minWindowBytes) throw new TransferError('Range making too little progress; reconnecting', 408);
        windowStart = Date.now(); windowBytes = 0;
      }
      if (length > maxBytes) throw new TransferError('Response exceeded the bounded transfer size', 413);
      if (onBytes) onBytes(value); else chunks.push(value);
    }
    if (!onBytes) {
      const advertised = response.headers.get('content-length');
      if (advertised && !response.headers.get('content-encoding') && length !== Number(advertised))
        throw new TransferError('Truncated response');
    }
    noteSuccess(target, length, Date.now() - startedAt);
    const bytes = new Uint8Array(onBytes ? 0 : length);
    let offset = 0; for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
    return { response, bytes, origin: new URL(target).origin };
  } catch (error) {
    if (stalled && !signal?.aborted) error = new TransferError('Request stalled; reconnecting', 408);
    error.origin = new URL(target).origin;
    throw error;
  } finally {
    clearTimeout(timer); controller.abort(); dispose(); release();
    if (reader) {
      try { reader.cancel().catch(() => {}); } catch {}
      try { reader.releaseLock(); } catch {}
    }
  }
}
function responseHeaders(headers) {
  const result = new Headers(headers);
  for (const name of ['content-encoding', 'transfer-encoding', 'connection']) result.delete(name);
  result.set('x-revaro-transport', 'range-v1');
  return result;
}
function restoredResponse({ response, bytes }) {
  const headers = responseHeaders(response.headers);
  if ([204, 205, 304].includes(response.status)) return new Response(null, { status: response.status, headers });
  headers.set('content-length', String(bytes.length));
  return new Response(bytes, { status: response.status, statusText: response.statusText, headers });
}
export async function bufferedRequest(request, safe = true) {
  const body = ['GET', 'HEAD'].includes(request.method) ? undefined : await request.clone().arrayBuffer();
  const init = { method: request.method, headers: request.headers, body, redirect: 'follow', keepalive: request.keepalive };
  const complete = /\/uploads\/[^/]+\/complete$/.test(new URL(request.url).pathname);
  const result = await retry(() => attempt(request.url, init, {
    signal: request.signal, headersMs: complete ? 120000 : policy.headersMs,
    priority: priorityOf(request),
  }), { signal: request.signal, attempts: safe ? policy.attempts : 1 });
  if (request.method === 'HEAD') return new Response(null, { status: result.response.status, headers: responseHeaders(result.response.headers) });
  return restoredResponse(result);
}
const strongETag = etag => /^"[^"\r\n]*"$/.test(etag || '');
function contentRange(value) {
  const m = /^bytes (\d+)-(\d+)\/(\d+)$/.exec(value || '');
  return m ? { start: Number(m[1]), end: Number(m[2]), size: Number(m[3]) } : null;
}
function requestedRange(value, size) {
  if (!value) return { start: 0, end: size - 1, partial: false };
  const m = /^bytes=(\d*)-(\d*)$/.exec(value);
  if (!m || (!m[1] && !m[2])) return null;
  const start = m[1] ? Number(m[1]) : Math.max(0, size - Number(m[2]));
  const end = m[1] && m[2] ? Math.min(size - 1, Number(m[2])) : size - 1;
  return { start, end, partial: true };
}
function cacheKey(url, etag, start, end) {
  const key = new URL('/.revaro-transport-cache', globalThis.location.href);
  key.search = new URLSearchParams({ url: resourceURL(url), etag, start, end }).toString(); return key.href;
}
function resourceURL(url) {
  const result = new URL(url, globalThis.location.href);
  result.searchParams.delete('revaro_priority'); return result.href;
}
function metaKey(url) {
  const key = new URL('/.revaro-transport-meta', globalThis.location.href);
  key.search = new URLSearchParams({ url: resourceURL(url) }).toString(); return key.href;
}
function remember(key, bytes) {
  const old = memory.get(key); if (old) memoryBytes -= old.length;
  memory.delete(key); memory.set(key, bytes); memoryBytes += bytes.length;
  while (memoryBytes > policy.memoryBytes && memory.size) {
    const oldest = memory.keys().next().value;
    memoryBytes -= memory.get(oldest).length; memory.delete(oldest);
  }
}
async function cachedChunk(key, length) {
  const generation = cacheGeneration;
  const hot = memory.get(key);
  if (hot?.length === length) { memory.delete(key); memory.set(key, hot); return hot; }
  try {
    const cache = await globalThis.caches?.open(cacheName), hit = await cache?.match(key);
    if (hit && Number(hit.headers.get('content-length')) === length) {
      const bytes = new Uint8Array(await hit.arrayBuffer());
      if (bytes.length === length && generation === cacheGeneration) { remember(key, bytes); return bytes; }
    }
  } catch { /* Cache availability never determines network success. */ }
  return null;
}
function queueCacheWrite(key, response, length) {
  if (queuedCacheBytes + length > policy.cacheWriteBytes) return;
  queuedCacheBytes += length;
  const generation = cacheGeneration;
  cacheWrites = cacheWrites.catch(() => {}).then(async () => {
    try {
      if (generation !== cacheGeneration) return;
      const cache = await globalThis.caches?.open(cacheName); if (!cache) return;
      diskKeys ||= new Map((await cache.keys()).map(key => [key.url || String(key), true]));
      await cache.put(key, response);
      diskKeys.delete(key); diskKeys.set(key, true);
      while (diskKeys.size > policy.cacheEntries) {
        const oldest = diskKeys.keys().next().value;
        await cache.delete(oldest); diskKeys.delete(oldest);
      }
    } catch { /* Private mode and quota errors must not stall playback. */ }
    finally { queuedCacheBytes -= length; }
  });
}
function saveChunk(key, bytes) {
  remember(key, bytes);
  queueCacheWrite(key, new Response(bytes, { headers: { 'content-length': String(bytes.length) } }), bytes.length);
}
async function knownMeta(url) {
  url = resourceURL(url);
  if (descriptors.has(url)) return descriptors.get(url);
  try {
    const cache = await globalThis.caches?.open(cacheName), response = await cache?.match(metaKey(url));
    const value = await response?.json();
    if (value && strongETag(value.etag) && Number.isSafeInteger(value.size) && value.size > 0) return value;
  } catch {}
  return null;
}
function saveMeta(url, meta) {
  url = resourceURL(url);
  if (/no-store/i.test(meta.headers.get('cache-control') || '')) return;
  descriptors.delete(url); descriptors.set(url, { etag: meta.etag, size: meta.size });
  if (descriptors.size > 128) descriptors.delete(descriptors.keys().next().value);
  queueCacheWrite(metaKey(url), new Response(JSON.stringify({ etag: meta.etag, size: meta.size })), 128);
}
export async function clearTransportCache() {
  cacheGeneration++; memory.clear(); memoryBytes = 0; descriptors.clear();
  for (const controller of resourceControllers) controller.abort();
  resourceControllers.clear();
  for (const flight of new Set([...flights.values()].map(entry => entry.flight))) flight.controller.abort();
  flights.clear();
  await cacheWrites.catch(() => {}); diskKeys = undefined;
  try { await globalThis.caches?.delete(cacheName); await globalThis.caches?.delete('revaro.transport.v1'); } catch {}
}
function awaitFlight(flight, signal) {
  checkAbort(signal); flight.users++;
  return new Promise((resolve, reject) => {
    let done = false;
    const finish = (error, bytes) => {
      if (done) return; done = true; signal?.removeEventListener('abort', abort);
      if (--flight.users === 0 && !flight.done) flight.controller.abort();
      error ? reject(error) : resolve(bytes);
    };
    const abort = () => finish(signal.reason || abortError());
    signal?.addEventListener('abort', abort, { once: true });
    flight.promise.then(bytes => finish(null, bytes), error => finish(error));
  });
}
function startFlight(request, meta, blocks, priority) {
  const controller = new AbortController(), start = blocks[0].start, end = blocks.at(-1).end;
  const flight = { controller, users: 0, done: false };
  const generation = cacheGeneration;
  const load = async (signal, origin) => {
    const bytes = new Uint8Array(end - start + 1); let received = 0;
    await retry(async () => {
      const offset = start + received; if (offset > end) return;
      const headers = new Headers(request.headers);
      headers.delete('if-none-match'); headers.set('range', `bytes=${offset}-${end}`);
      headers.set('if-range', meta.etag); headers.set('if-match', meta.etag);
      await attempt(request.url, { method: 'GET', headers }, {
        signal, origin, priority, maxBytes: end - offset + 1,
        validate: response => {
          const range = contentRange(response.headers.get('content-range'));
          if (response.status === 412 || (response.ok && response.headers.get('etag') !== meta.etag))
            throw new TransferError('File changed during transfer', 412);
          if (!response.ok) throw new TransferError(`HTTP ${response.status}`, response.status);
          if (response.status !== 206 || !range || range.start !== offset || range.end !== end || range.size !== meta.size)
            throw new TransferError('Invalid Range response', 502);
        },
        onBytes: chunk => { bytes.set(chunk, received); received += chunk.length; },
      });
      if (received !== bytes.length) throw new TransferError('Truncated range');
    }, { signal });
    return { bytes, origin };
  };
  flight.promise = hedge(load, controller.signal, priority < 3 && end - start + 1 <= policy.smallBytes)
    .then(result => {
      if (generation === cacheGeneration && !/no-store/i.test(meta.headers.get('cache-control') || ''))
        for (const block of blocks) saveChunk(block.key, result.bytes.slice(block.start - start, block.end - start + 1));
      return result.bytes;
    }).finally(() => {
      flight.done = true;
      for (const block of blocks) if (flights.get(block.key)?.flight === flight) flights.delete(block.key);
    });
  flight.promise.catch(() => {});
  for (const block of blocks) flights.set(block.key, { flight, start });
  return flight;
}
async function fetchChunk(request, meta, start, end, signal, priority) {
  const persist = !/no-store/i.test(meta.headers.get('cache-control') || '');
  const blocks = [];
  for (let offset = Math.floor(start / policy.blockBytes) * policy.blockBytes; offset <= end; offset += policy.blockBytes) {
    const last = Math.min(meta.size - 1, offset + policy.blockBytes - 1);
    blocks.push({ start: offset, end: last, key: cacheKey(request.url, meta.etag, offset, last) });
  }
  await Promise.all(blocks.map(async block => { if (persist) block.bytes = await cachedChunk(block.key, block.end - block.start + 1); }));
  checkAbort(signal);
  if (persist) for (const block of blocks) block.bytes ||= memory.get(block.key);
  for (let index = 0; index < blocks.length;) {
    if (blocks[index].bytes || flights.has(blocks[index].key)) { index++; continue; }
    const run = [blocks[index++]];
    while (index < blocks.length && !blocks[index].bytes && !flights.has(blocks[index].key)) run.push(blocks[index++]);
    startFlight(request, meta, run, priority);
  }
  // A consumer subscribes once per shared network flight. Cancelling one
  // preview cannot cancel another preview still consuming the same bytes.
  const subscriptions = new Map();
  for (const block of blocks) {
    if (block.bytes) continue;
    const entry = flights.get(block.key);
    if (!subscriptions.has(entry.flight)) subscriptions.set(entry.flight, awaitFlight(entry.flight, signal));
    block.promise = subscriptions.get(entry.flight).then(bytes => bytes.subarray(block.start - entry.start, block.end - entry.start + 1));
  }
  await Promise.all(blocks.map(async block => { block.bytes ||= await block.promise; }));
  const output = new Uint8Array(end - start + 1);
  for (const block of blocks) {
    const from = Math.max(start, block.start), to = Math.min(end, block.end);
    output.set(block.bytes.subarray(from - block.start, to - block.start + 1), from - start);
  }
  return output;
}
function windowBytes() {
  const maximum = Math.max(policy.blockBytes, policy.chunkBytes);
  const estimated = throughput ? throughput * policy.windowMs / 1000 : maximum;
  return Math.min(maximum, Math.max(policy.blockBytes, Math.floor(estimated / policy.blockBytes) * policy.blockBytes));
}
export async function fileResponse(request, restarted = false) {
  const generation = cacheGeneration;
  const priority = priorityOf(request), previous = await knownMeta(request.url);
  if (generation !== cacheGeneration) throw abortError();
  const rangeHeader = request.headers.get('range');
  const beginning = !rangeHeader || /^bytes=0-\d*$/.test(rangeHeader);
  const combined = !previous && beginning && !request.headers.has('if-range');
  const requestedEnd = /^bytes=0-(\d+)$/.exec(rangeHeader || '')?.[1];
  const initialEnd = Math.min(policy.firstBytes, policy.blockBytes, policy.chunkBytes, requestedEnd === undefined ? Infinity : Number(requestedEnd) + 1) - 1;
  const probeHeaders = new Headers(request.headers);
  probeHeaders.set('range', combined ? `bytes=0-${initialEnd}` : 'bytes=0-0');
  probeHeaders.delete('if-range'); probeHeaders.delete('if-match'); probeHeaders.delete('if-none-match');
  const probe = await retry(() => hedge((signal, origin) => attempt(request.url, { method: 'GET', headers: probeHeaders }, {
    signal, origin, priority, maxBytes: 64 * 1024 * 1024,
  }), request.signal, priority < 3), { signal: request.signal });
  if (generation !== cacheGeneration) throw abortError();
  if (!probe.response.ok) return restoredResponse(probe);
  const range = contentRange(probe.response.headers.get('content-range')), etag = probe.response.headers.get('etag');
  if (!range || !strongETag(etag)) return probe.response.status === 206 ? bufferedRequest(request) : restoredResponse(probe);
  if (range.start !== 0 || range.end >= range.size || probe.bytes.length !== range.end + 1)
    throw new TransferError('Invalid initial Range response', 502);
  const meta = { size: range.size, etag, headers: probe.response.headers };
  const ifMatch = request.headers.get('if-match');
  if (ifMatch && ifMatch !== '*' && !ifMatch.split(',').some(value => value.trim() === etag))
    return new Response(null, { status: 412, headers: { etag } });
  const ifRange = request.headers.get('if-range');
  const wanted = requestedRange(ifRange && ifRange !== etag ? null : rangeHeader, meta.size);
  if (!wanted) return bufferedRequest(request);
  if (wanted.start > wanted.end || wanted.start >= meta.size)
    return new Response(null, { status: 416, headers: { 'content-range': `bytes */${meta.size}`, etag } });
  saveMeta(request.url, meta);
  if (combined && !/no-store/i.test(meta.headers.get('cache-control') || '')
      && probe.bytes.length === Math.min(policy.blockBytes, meta.size))
    saveChunk(cacheKey(request.url, etag, 0, probe.bytes.length - 1), probe.bytes);
  const headers = responseHeaders(meta.headers);
  headers.set('content-length', String(wanted.end - wanted.start + 1));
  if (wanted.partial) headers.set('content-range', `bytes ${wanted.start}-${wanted.end}/${meta.size}`);
  else headers.delete('content-range');
  const { controller, dispose } = linkedController(request.signal);
  resourceControllers.add(controller);
  let next = wanted.start, first;
  if (combined && wanted.start === 0) {
    first = probe.bytes.subarray(0, Math.min(probe.bytes.length, wanted.end + 1)); next += first.length;
  }
  const pending = [];
  const laneCount = () => priority >= 3 && mediaPressure() ? 1 : throughput && throughput < 128 * 1024 ? 2 : protocol.startsWith('h3') ? 4 : Math.max(2, Math.min(4, policy.lanes));
  const schedule = () => {
    while (pending.length < laneCount() && next <= wanted.end) {
      const start = next, target = windowBytes();
      const end = Math.min(wanted.end, (Math.floor(start / policy.blockBytes) * policy.blockBytes) + target - 1);
      next = end + 1;
      const promise = fetchChunk(request, meta, start, end, controller.signal, priority);
      promise.catch(() => {}); pending.push(promise);
    }
  };
  // A cold request returns its authenticated first segment immediately. Seek
  // requests use a single small first segment before growing the read window.
  if (!first) {
    const end = Math.min(wanted.end, wanted.start + policy.firstBytes - 1,
      (Math.floor(wanted.start / policy.blockBytes) + 1) * policy.blockBytes - 1);
    try { first = await fetchChunk(request, meta, wanted.start, end, controller.signal, priority); next = end + 1; }
    catch (error) {
      controller.abort(); dispose(); resourceControllers.delete(controller);
      if (error.status === 412 && !restarted) return fileResponse(request, true);
      throw error;
    }
  }
  let finished;
  const transportComplete = new Promise(resolve => { finished = resolve; });
  const finish = () => { dispose(); resourceControllers.delete(controller); cacheWrites.catch(() => {}).then(finished); };
  const stream = new ReadableStream({
    async pull(output) {
      try {
        checkAbort(controller.signal);
        const bytes = first || await pending.shift(); first = null;
        if (bytes) output.enqueue(bytes);
        schedule();
        if (!pending.length && next > wanted.end) { output.close(); finish(); }
      } catch (error) { controller.abort(); output.error(error); finish(); }
    },
    cancel() { controller.abort(); finish(); },
  }, { highWaterMark: 1 });
  const response = new Response(stream, { status: wanted.partial ? 206 : 200, headers });
  response.transportComplete = transportComplete;
  return response;
}

export async function blobHash(blob) {
  const digest = await crypto.subtle.digest('SHA-256', await blob.arrayBuffer());
  return Array.from(new Uint8Array(digest), b => b.toString(16).padStart(2, '0')).join('');
}
// XHR is only an upload progress adapter. Stall detection, retry budgets,
// jitter, cancellation and endpoint failover are shared with file reads.
export async function putBlob(url, body, contentType, signal, onProgress) {
  const hash = await blobHash(body);
  return retry(async () => {
    const release = await admission(signal, 4);
    try { return await new Promise((resolve, reject) => {
    checkAbort(signal);
    const xhr = new XMLHttpRequest(), target = endpoint(url); let timer, settled = false;
    const arm = ms => { clearTimeout(timer); timer = setTimeout(() => finish(new TransferError('Upload stalled', 408)), ms); };
    const abort = () => finish(signal.reason || abortError());
    const finish = (error, etag) => {
      if (settled) return; settled = true; clearTimeout(timer);
      signal?.removeEventListener('abort', abort);
      xhr.onload = xhr.onerror = xhr.onabort = null;
      xhr.upload.onprogress = xhr.upload.onload = null;
      xhr.abort();
      if (error) { error.origin = new URL(target).origin; reject(error); } else resolve(etag);
    };
    xhr.open('PUT', target, true); xhr.withCredentials = true;
    xhr.setRequestHeader('X-Revaro-Managed', '1');
    xhr.setRequestHeader('Priority', 'u=5, i');
    xhr.setRequestHeader('X-Content-SHA256', hash);
    if (contentType) xhr.setRequestHeader('Content-Type', contentType);
    xhr.upload.onprogress = event => { arm(policy.idleMs); onProgress(event.loaded); };
    xhr.upload.onload = () => arm(30000); // durable part acknowledgement
    xhr.onload = () => {
      if (xhr.status < 200 || xhr.status >= 300) return finish(new TransferError(`Upload HTTP ${xhr.status}`, xhr.status));
      const verified = xhr.getResponseHeader('X-Content-SHA256'), etag = xhr.getResponseHeader('ETag');
      if (verified !== hash || !etag) return finish(new TransferError('Upload checksum acknowledgement mismatch', 502));
      noteSuccess(target); onProgress(body.size); finish(null, etag);
    };
    xhr.onerror = () => finish(new TransferError('Upload connection interrupted'));
    xhr.onabort = () => finish(abortError());
    signal?.addEventListener('abort', abort, { once: true });
    arm(policy.headersMs); onProgress(0); xhr.send(body);
    }); } finally { release(); }
  }, { signal });
}
