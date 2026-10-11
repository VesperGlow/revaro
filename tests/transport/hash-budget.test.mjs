import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { blobHash, policy } from '../../crates/revaro-web/static/transport-core.js';

test('hashing admits bytes before allocating, cancels queued work and permits oversized parts alone', async context => {
  const previous = Object.getOwnPropertyDescriptor(globalThis, 'crypto'), budget = policy.hashBytes;
  let active = 0, peak = 0, started = 0;
  policy.hashBytes = 8;
  Object.defineProperty(globalThis, 'crypto', { configurable: true, value: { subtle: {
    async digest(_, bytes) { await delay(15); active -= bytes.byteLength; return createHash('sha256').update(new Uint8Array(bytes)).digest(); },
  } } });
  context.after(() => { policy.hashBytes = budget; Object.defineProperty(globalThis, 'crypto', previous); });
  const blob = size => ({ size, async arrayBuffer() { started++; active += size; peak = Math.max(peak, active); return new Uint8Array(size).buffer; } });
  const values = await Promise.all(Array.from({ length: 12 }, () => blobHash(blob(4))));
  assert.equal(peak, 8);
  assert.ok(values.every(hash => hash === createHash('sha256').update(new Uint8Array(4)).digest('hex')));
  peak = 0;
  await Promise.all([blobHash(blob(12)), blobHash(blob(4))]);
  assert.equal(peak, 12, 'oversized input is exclusive without changing its bytes');
  const running = blobHash(blob(8)), controller = new AbortController();
  const before = started;
  const queued = blobHash(blob(4), controller.signal);
  controller.abort();
  await assert.rejects(queued, { name: 'AbortError' });
  await running;
  assert.equal(started, before + 1, 'cancelled queued hash never allocates');
  assert.equal(active, 0);
  assert.equal(await blobHash(blob(4)), values[0], 'permits are released after cancellation');
});
