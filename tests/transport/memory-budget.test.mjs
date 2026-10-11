import { test } from 'node:test';
import assert from 'node:assert/strict';

for (const [memory, expected] of [[undefined, 8], [2, 8], [4, 32], [8, 32]]) {
  test(`transport memory budget is ${expected} MiB for device memory ${memory}`, async context => {
    const previous = Object.getOwnPropertyDescriptor(globalThis, 'navigator');
    Object.defineProperty(globalThis, 'navigator', { configurable: true, value: { deviceMemory: memory } });
    context.after(() => {
      if (previous) Object.defineProperty(globalThis, 'navigator', previous);
      else delete globalThis.navigator;
    });
    const { policy } = await import(`../../crates/revaro-web/static/transport-core.js?memory-budget=${memory}`);
    assert.equal(policy.memoryBytes, expected * 1024 * 1024);
    assert.equal(policy.cacheWriteBytes, 8 * 1024 * 1024);
    assert.equal(policy.lanes, 3);
  });
}
