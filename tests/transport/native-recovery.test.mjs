import { test } from 'node:test';
import assert from 'node:assert/strict';

globalThis.location = new URL('http://localhost:8081/');
class Media extends EventTarget {
  src = 'http://localhost:8081/api/files/video/preview';
  currentSrc = this.src; currentTime = 5; paused = false; ended = false;
  seeking = false; isConnected = true; loads = 0; plays = 0; error = null; buffer = 0.03; readyState = 4;
  buffered = { length: 1, start: () => 0, end: () => this.currentTime + this.buffer };
  load() {
    this.loads++; this.paused = true; this.currentTime = 0;
    queueMicrotask(() => this.dispatchEvent(new Event('loadedmetadata')));
  }
  async play() { this.plays++; this.paused = false; }
}
globalThis.HTMLMediaElement = Media;
const { watchNativeRecovery } = await import('../../crates/revaro-web/static/transport-client.js');
const wait = () => new Promise(resolve => setTimeout(resolve, 25));
function observe() {
  const listeners = new Map();
  watchNativeRecovery({ addEventListener: (type, listener) => listeners.set(type, listener) }, 5);
  return (element, type) => listeners.get(type)?.({ target: element, type });
}

test('an exhausted native stall recovers its playback position with a bounded reload budget', async () => {
  const emit = observe(), media = new Media();
  for (let i = 0; i < 3; i++) { emit(media, 'stalled'); await wait(); }
  assert.equal(media.loads, 2);
  assert.equal(media.plays, 2);
  assert.equal(media.currentTime, 5);
  media.currentTime = 7; media.buffer = 2; emit(media, 'timeupdate');
  media.buffer = 0.03; emit(media, 'stalled'); await wait();
  assert.equal(media.loads, 3, 'sustained healthy playback renews the recovery budget');
});

test('native recovery preserves active buffering, user pauses, seeks, source changes and decoding errors', async () => {
  const emit = observe();
  for (const interrupt of [
    media => { media.buffer = 2; },
    media => { media.currentTime += 0.5; },
    media => { media.buffer = 2; emit(media, 'progress'); },
    media => { media.paused = true; emit(media, 'pause'); },
    media => { media.seeking = true; emit(media, 'seeking'); },
    media => { media.currentSrc = 'http://localhost:8081/api/files/other/preview'; },
    media => { media.isConnected = false; },
    media => { media.error = { code: 3 }; },
  ]) {
    const media = new Media(); emit(media, 'stalled'); interrupt(media); await wait();
    assert.equal(media.loads, 0);
    assert.equal(media.plays, 0);
  }
});

test('a final progress event after waiting does not strand an exhausted native stream', async () => {
  const emit = observe(), media = new Media();
  media.currentTime = 0; media.readyState = 2;
  emit(media, 'waiting');
  media.currentTime = 0.14; emit(media, 'timeupdate'); emit(media, 'progress');
  await wait();
  assert.equal(media.loads, 1);
  assert.equal(media.currentTime, 0.14);
  assert.equal(media.plays, 1);
});

test('a blocked decoder does not mistake an unplayable buffered range for future frames', async () => {
  const emit = observe(), media = new Media();
  media.currentTime = 0.14; media.buffer = 0.6; media.readyState = 2;
  emit(media, 'stalled'); emit(media, 'progress');
  await wait();
  assert.equal(media.loads, 1);
  assert.equal(media.currentTime, 0.14);
  assert.equal(media.plays, 1);
});

test('a startup stall at zero and a network error retain native recovery', async () => {
  const emit = observe();
  for (const position of [0, 5]) {
    const media = new Media(); media.currentTime = position;
    media.error = position ? { code: 2 } : null;
    emit(media, position ? 'error' : 'waiting'); emit(media, 'progress');
    await wait();
    assert.equal(media.loads, 1);
    assert.equal(media.currentTime, position);
    assert.equal(media.plays, 1);
  }
});

test('native playback that resumes after progress cancels the stalled reload', async () => {
  const emit = observe();
  for (const event of ['timeupdate', 'playing', 'seeked']) {
    const media = new Media();
    emit(media, 'waiting'); emit(media, 'progress');
    media.currentTime += 0.5; emit(media, event);
    await wait();
    assert.equal(media.loads, 0);
    assert.equal(media.plays, 0);
  }
});

test('pausing while a native reload awaits metadata prevents automatic playback', async () => {
  const emit = observe(), media = new Media();
  media.load = () => { media.loads++; media.paused = true; media.currentTime = 0; };
  emit(media, 'stalled'); await wait();
  assert.equal(media.loads, 1);
  emit(media, 'pause');
  media.dispatchEvent(new Event('loadedmetadata'));
  assert.equal(media.plays, 0);
  assert.equal(media.paused, true);
});
