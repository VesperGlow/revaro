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
      method: 'PUT', body: '{"position":42}', keepalive: true, headers: { 'x-revaro-managed': '1' },
    }),
    respondWith: response => { forwarded = response; },
  });
  assert.ok(forwarded, 'the native response explicitly settles the worker fetch event');
  const response = await forwarded;
  assert.equal(response.status, 409);
  assert.equal(await response.text(), 'original response');
  assert.deepEqual(calls, [{ method: 'PUT', keepalive: true, body: '{"position":42}' }]);
});
