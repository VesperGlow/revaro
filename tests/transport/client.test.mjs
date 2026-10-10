import { test } from 'node:test';
import assert from 'node:assert/strict';

test('document API recovery works without a worker and preserves keepalive and idempotency', async () => {
  const calls = [], intervals = [];
  const attempts = new Map();
  globalThis.location = new URL('http://localhost:8081/');
  Object.defineProperty(globalThis, 'navigator', { configurable: true, value: {} });
  globalThis.isSecureContext = false;
  globalThis.document = { querySelectorAll: () => [], addEventListener: () => {} };
  globalThis.addEventListener = () => {};
  globalThis.setInterval = callback => { intervals.push(callback); return 1; };
  let cleared = 0;
  globalThis.caches = { delete: async () => { cleared++; } };
  globalThis.fetch = async (url, init) => {
    const request = url instanceof Request ? url : new Request(url, init);
    const path = new URL(request.url).pathname;
    calls.push({ path, ...init, bodyText: await request.clone().text() });
    const count = (attempts.get(path) || 0) + 1;
    attempts.set(path, count);
    if (count === 1 && ['/api/listening/session', '/api/uploads'].includes(path)) {
      throw new TypeError('connection interrupted');
    }
    return new Response('{}', { status: 200 });
  };
  const client = await import('../../crates/revaro-web/static/transport-client.js');
  const core = await import('../../crates/revaro-web/static/transport-core.js');
  Object.assign(core.policy, { backoffMs: 1, maxBackoffMs: 2 });
  await client.initializeTransport();
  for (const callback of intervals) assert.doesNotThrow(callback);
  const request = (path, init) => new Request(new URL(path, location), init);
  assert.equal((await fetch(request('/api/listening/session'))).status, 200);
  assert.equal(attempts.get('/api/listening/session'), 2);
  await fetch(request('/api/files/track/media/progress', { method: 'PUT', body: '{}', keepalive: true }));
  assert.equal(calls.at(-1).keepalive, true);
  assert.equal(calls.at(-1).headers.get('x-revaro-managed'), '1');
  await fetch(request('/native', { method: 'POST', body: 'untouched body' }));
  assert.equal(calls.at(-1).bodyText, 'untouched body', 'native delegation uses the inspected request, whose body remains readable');
  await fetch(request('/api/files/batch-download/prepare', { method: 'POST', body: '{"ids":["folder"]}' }));
  assert.equal(calls.at(-1).headers.get('x-revaro-managed'), '1', 'file-path mutations use document API recovery');
  assert.equal(calls.at(-1).bodyText, '{"ids":["folder"]}');
  await fetch(request('/api/uploads', { method: 'POST', body: JSON.stringify({ idempotency_key: 'upload-1' }) }));
  assert.equal(attempts.get('/api/uploads'), 2);
  attempts.delete('/api/uploads');
  await assert.rejects(fetch(request('/api/uploads', { method: 'POST', body: '{}' })), TypeError);
  assert.equal(attempts.get('/api/uploads'), 1, 'unversioned mutation is not replayed');
  await fetch(request('/api/auth/logout', { method: 'POST' }));
  assert.equal(cleared, 2);
});
