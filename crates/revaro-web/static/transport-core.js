// All recovery policy lives here. The worker and browser upload adapter use
// the same implementation; neither MIME types nor file extensions select it.
export const policy = {
  chunkBytes: 512 * 1024, smallBytes: 256 * 1024, lanes: 3,
  headersMs: 15000, idleMs: 8000, hedgeMs: 1200,
  attempts: 10, backoffMs: 400, maxBackoffMs: 8000,
  cacheEntries: 256, fallbackMs: 300000, progressWindowMs: 20000, minWindowBytes: 8192,
};
let http2Origin = '', fallbackUntil = 0, failures = 0, protocol = '';
const cacheName = 'revaro.transport.v1';
let cacheWrites = Promise.resolve();
let activeRequests = 0, activeHedges = 0;
const requestQueue = [];
async function admission(signal) {
  checkAbort(signal);
  if (activeRequests >= (protocol.startsWith('h3') ? 16 : 12)) {
    await new Promise((resolve, reject) => {
      const entry = { resolve: () => { signal?.removeEventListener('abort', abort); resolve(); } };
      const abort = () => { const index = requestQueue.indexOf(entry); if (index >= 0) requestQueue.splice(index, 1); reject(signal.reason || abortError()); };
      requestQueue.push(entry); signal?.addEventListener('abort', abort, { once: true });
    });
  } else activeRequests++;
  let released = false;
  return () => {
    if (released) return; released = true;
    const waiting = requestQueue.shift();
    if (waiting) waiting.resolve(); else activeRequests--;
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
  return { protocol, fallback: fallbackUntil > Date.now(), http2Origin };
}
function endpoint(url) {
  const target = new URL(url, globalThis.location?.href);
  if (http2Origin && fallbackUntil > Date.now())
    return http2Origin + target.pathname + target.search;
  return target.href;
}
function noteFailure(error) {
  if (error.status && error.status !== 408) return;
  failures += error.failedAttempts || 1;
  if (http2Origin && failures >= 2) fallbackUntil = Date.now() + policy.fallbackMs;
}
function noteSuccess(url) {
  failures = 0;
  const entries = globalThis.performance?.getEntriesByName?.(url);
  const hint = entries?.at(-1)?.nextHopProtocol;
  if (hint) protocol = hint;
}
const abortError = () => new DOMException('Transfer cancelled', 'AbortError');
function checkAbort(signal) { if (signal?.aborted) throw signal.reason || abortError(); }
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
  if (!enabled || activeHedges >= 2) return operation(signal);
  activeHedges++;
  const first = linkedController(signal), second = linkedController(signal);
  let timer, startSecond, started = false;
  const secondary = new Promise((resolve, reject) => {
    startSecond = () => { if (started) return; started = true; operation(second.controller.signal).then(resolve, reject); };
    timer = setTimeout(startSecond, protocol.startsWith('h3') ? policy.hedgeMs * 0.6 : policy.hedgeMs);
  });
  const primary = operation(first.controller.signal).catch(error => {
    clearTimeout(timer); startSecond(); throw error;
  });
  try { return await Promise.any([primary, secondary]); }
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
async function attempt(url, init, { signal, validate, onBytes, maxBytes = 16 * 1024 * 1024, headersMs = policy.headersMs } = {}) {
  const release = await admission(signal);
  const { controller, dispose } = linkedController(signal);
  let timer, reader, stalled = false;
  const arm = ms => {
    clearTimeout(timer);
    timer = setTimeout(() => { stalled = true; controller.abort(); }, ms);
  };
  const target = endpoint(url);
  try {
    arm(headersMs);
    const response = await networkFetch(target, { ...init, signal: controller.signal, credentials: 'include', cache: 'no-store' });
    if ([408, 425, 429, 500, 502, 503, 504].includes(response.status)) {
      await response.body?.cancel();
      throw new TransferError(`HTTP ${response.status}`, response.status, retryAfter(response));
    }
    validate?.(response);
    if (!response.body) return { response, bytes: new Uint8Array() };
    reader = response.body.getReader();
    const chunks = []; let length = 0, windowBytes = 0, windowStart = Date.now();
    while (true) {
      arm(protocol.startsWith('h3') ? policy.idleMs * 0.65 : policy.idleMs);
      const { value, done } = await reader.read();
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
    noteSuccess(target);
    const bytes = new Uint8Array(onBytes ? 0 : length);
    let offset = 0; for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
    return { response, bytes };
  } catch (error) {
    if (stalled && !signal?.aborted) throw new TransferError('Request stalled; reconnecting', 408);
    throw error;
  } finally {
    clearTimeout(timer); controller.abort(); dispose(); release();
    if (reader) { try { await reader.cancel(); } catch {} reader.releaseLock(); }
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
  const init = { method: request.method, headers: request.headers, body, redirect: 'follow' };
  const complete = /\/uploads\/[^/]+\/complete$/.test(new URL(request.url).pathname);
  const result = await retry(() => attempt(request.url, init, {
    signal: request.signal, headersMs: complete ? 120000 : policy.headersMs,
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
  key.search = new URLSearchParams({ url, etag, start, end }).toString(); return key.href;
}
async function cachedChunk(key, length) {
  try {
    const cache = await globalThis.caches?.open(cacheName), hit = await cache?.match(key);
    if (hit && Number(hit.headers.get('content-length')) === length) {
      const bytes = new Uint8Array(await hit.arrayBuffer());
      if (bytes.length === length) return bytes;
    }
  } catch { /* Private mode/quota errors must never stop a transfer. */ }
  return null;
}
async function saveChunk(key, bytes) {
  cacheWrites = cacheWrites.catch(() => {}).then(async () => {
    try {
      const cache = await globalThis.caches?.open(cacheName); if (!cache) return;
      await cache.put(key, new Response(bytes, { headers: { 'content-length': String(bytes.length) } }));
      const keys = await cache.keys();
      for (const stale of keys.slice(0, Math.max(0, keys.length - policy.cacheEntries))) await cache.delete(stale);
    } catch { /* Network success does not depend on persistent cache availability. */ }
  });
  await cacheWrites;
}
export async function clearTransportCache() {
  await cacheWrites.catch(() => {});
  try { await globalThis.caches?.delete(cacheName); } catch {}
}
async function fetchChunk(request, meta, start, end, signal) {
  const length = end - start + 1, key = cacheKey(request.url, meta.etag, start, end);
  const persist = !/no-store/i.test(meta.headers.get('cache-control') || '');
  if (persist) { const hit = await cachedChunk(key, length); if (hit) return hit; }
  const load = async innerSignal => {
    const bytes = new Uint8Array(length); let received = 0;
    await retry(async () => {
      const offset = start + received;
      if (offset > end) return;
      const headers = new Headers(request.headers);
      headers.delete('if-none-match'); headers.set('range', `bytes=${offset}-${end}`);
      headers.set('if-range', meta.etag); headers.set('if-match', meta.etag);
      await attempt(request.url, { method: 'GET', headers }, {
        signal: innerSignal, maxBytes: end - offset + 1,
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
      if (received !== length) throw new TransferError('Truncated range');
    }, { signal: innerSignal });
    return bytes;
  };
  const bytes = await hedge(load, signal, length <= policy.smallBytes);
  if (persist) await saveChunk(key, bytes);
  return bytes;
}
export async function fileResponse(request, restarted = false) {
  // This fresh, authenticated probe precedes every cache lookup: revoked shares,
  // expired sessions and changed validators can never expose stale cached bytes.
  const probeHeaders = new Headers(request.headers);
  probeHeaders.set('range', 'bytes=0-0'); probeHeaders.delete('if-range'); probeHeaders.delete('if-match');
  const probe = await retry(() => hedge(s => attempt(request.url, { method: 'GET', headers: probeHeaders }, {
    signal: s, maxBytes: 64 * 1024 * 1024,
  }), request.signal), { signal: request.signal });
  if (!probe.response.ok || probe.response.status === 304) return restoredResponse(probe);
  const range = contentRange(probe.response.headers.get('content-range'));
  const etag = probe.response.headers.get('etag');
  if (!range || !strongETag(etag)) return probe.response.status === 206 ? bufferedRequest(request) : restoredResponse(probe);
  const meta = { size: range.size, etag, headers: probe.response.headers };
  const ifMatch = request.headers.get('if-match');
  if (ifMatch && ifMatch !== '*' && !ifMatch.split(',').some(value => value.trim() === etag))
    return new Response(null, { status: 412, headers: { etag } });
  const ifRange = request.headers.get('if-range');
  const wanted = requestedRange(ifRange && ifRange !== etag ? null : request.headers.get('range'), meta.size);
  if (!wanted) return bufferedRequest(request);
  if (wanted.start > wanted.end || wanted.start >= meta.size) {
    return new Response(null, { status: 416, headers: { 'content-range': `bytes */${meta.size}`, etag } });
  }
  const headers = responseHeaders(meta.headers);
  headers.set('content-length', String(wanted.end - wanted.start + 1));
  if (wanted.partial) headers.set('content-range', `bytes ${wanted.start}-${wanted.end}/${meta.size}`);
  else headers.delete('content-range');
  const { controller, dispose } = linkedController(request.signal);
  const lanes = protocol.startsWith('h3') ? 4 : Math.max(2, Math.min(4, policy.lanes));
  // Fixed windows bound memory even for a terabyte file. Only the failed range
  // resumes, from its last received byte; completed windows survive reloads.
  let next = wanted.start;
  const pending = [];
  const schedule = () => {
    if (next > wanted.end) return;
    const start = next, end = Math.min(wanted.end, start + policy.chunkBytes - 1); next = end + 1;
    const promise = fetchChunk(request, meta, start, end, controller.signal);
    promise.catch(() => {}); pending.push(promise);
  };
  for (let i = 0; i < lanes; i++) schedule();
  // Resolve the first block before exposing headers. A validator change at this
  // point can safely restart; once bytes are delivered we must fail, never mix.
  let first;
  try { first = await pending.shift(); }
  catch (error) {
    controller.abort(); dispose();
    if (error.status === 412 && !restarted) return fileResponse(request, true);
    throw error;
  }
  let finished;
  const transportComplete = new Promise(resolve => { finished = resolve; });
  const stream = new ReadableStream({
    async pull(output) {
      try {
        checkAbort(controller.signal);
        const bytes = first || await pending.shift(); first = null;
        if (bytes) { output.enqueue(bytes); schedule(); }
        if (!pending.length && next > wanted.end) { output.close(); dispose(); finished(); }
      } catch (error) { controller.abort(); dispose(); output.error(error); finished(); }
    },
    cancel() { controller.abort(); dispose(); finished(); },
  // Queue one block so native attachment navigations can start consuming the
  // response. A zero-sized queue can leave fresh public downloads unconsumed.
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
    const release = await admission(signal);
    try { return await new Promise((resolve, reject) => {
    checkAbort(signal);
    const xhr = new XMLHttpRequest(); let timer, settled = false;
    const arm = ms => { clearTimeout(timer); timer = setTimeout(() => finish(new TransferError('Upload stalled', 408)), ms); };
    const abort = () => finish(signal.reason || abortError());
    const finish = (error, etag) => {
      if (settled) return; settled = true; clearTimeout(timer);
      signal?.removeEventListener('abort', abort);
      xhr.onload = xhr.onerror = xhr.onabort = null;
      xhr.upload.onprogress = xhr.upload.onload = null;
      xhr.abort(); error ? reject(error) : resolve(etag);
    };
    xhr.open('PUT', endpoint(url), true); xhr.withCredentials = true;
    xhr.setRequestHeader('X-Revaro-Managed', '1');
    xhr.setRequestHeader('X-Content-SHA256', hash);
    if (contentType) xhr.setRequestHeader('Content-Type', contentType);
    xhr.upload.onprogress = event => { arm(policy.idleMs); onProgress(event.loaded); };
    xhr.upload.onload = () => arm(30000); // durable part acknowledgement
    xhr.onload = () => {
      if (xhr.status < 200 || xhr.status >= 300) return finish(new TransferError(`Upload HTTP ${xhr.status}`, xhr.status));
      const verified = xhr.getResponseHeader('X-Content-SHA256'), etag = xhr.getResponseHeader('ETag');
      if (verified !== hash || !etag) return finish(new TransferError('Upload checksum acknowledgement mismatch', 502));
      noteSuccess(endpoint(url)); onProgress(body.size); finish(null, etag);
    };
    xhr.onerror = () => finish(new TransferError('Upload connection interrupted'));
    xhr.onabort = () => finish(abortError());
    signal?.addEventListener('abort', abort, { once: true });
    arm(policy.headersMs); onProgress(0); xhr.send(body);
    }); } finally { release(); }
  }, { signal });
}
