import { test } from 'node:test';
import assert from 'node:assert/strict';

test('worker forwards document-managed writes once with the body and keepalive intact', async () => {
  const handlers = new Map(), calls = [];
  globalThis.location = new URL('http://localhost:8081/');
  globalThis.self = { location, addEventListener: (name, handler) => handlers.set(name, handler) };
  globalThis.fetch = async request => {
    calls.push({ method: request.method, keepalive: request.keepalive, body: await request.text() });
    return new Response('original response', { status: 409 });
  };
  await import('../../crates/revaro-web/static/transport-sw.js');
  let forwarded;
  handlers.get('fetch')({
    request: new Request(new URL('/api/files/track/media/progress', location), {
      method: 'PUT', body: '{"position":42}', keepalive: true, headers: { 'x-revaro-managed': 'document' },
    }),
    respondWith: response => { forwarded = response; },
  });
  assert.ok(forwarded, 'the native response explicitly settles the worker fetch event');
  const response = await forwarded;
  assert.equal(response.status, 409);
  assert.equal(await response.text(), 'original response');
  assert.deepEqual(calls, [{ method: 'PUT', keepalive: true, body: '{"position":42}' }]);
  handlers.get('fetch')({
    request: new Request('https://localhost:8443/api/listening/session', {
      headers: { 'x-revaro-managed': 'document' },
    }),
    respondWith: response => { forwarded = response; },
  });
  assert.equal((await forwarded).status, 409, 'configured alternate origins also settle their native fetch event');
  assert.equal(calls.length, 2);
  handlers.get('fetch')({
    request: new Request(new URL('/api/uploads/upload/data', location), {
      method: 'PUT', body: 'upload', headers: { 'x-revaro-managed': '1' },
    }),
    respondWith: () => { throw new Error('XHR upload progress must retain its native network path'); },
  });
  assert.equal(calls.length, 2, 'XHR upload bodies are not transferred through worker fetch');
});
