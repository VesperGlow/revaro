// Read-only media acceptance on a trusted, real public HTTPS endpoint.
// Each sample has a fresh browser profile and the deployed scripts are fetched
// from that endpoint. The native control bypasses the worker only in the test.
import { spawn, execFileSync } from 'node:child_process';
import { mkdtemp, readFile, writeFile, mkdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
process.env.PW_EXPERIMENTAL_SERVICE_WORKER_NETWORK_EVENTS = '1';
const { chromium } = await import('../e2e/node_modules/playwright/index.mjs');

const target = new URL(process.env.REVARO_PUBLIC_URL);
if (target.protocol !== 'https:' || target.username || target.password) throw new Error('Public HTTPS without embedded credentials required');
const origin = target.origin;
const fileId = process.env.REVARO_PUBLIC_VIDEO_ID;
if (!/^[0-9a-f-]{36}$/.test(fileId || '')) throw new Error('Set REVARO_PUBLIC_VIDEO_ID');
execFileSync('python3', ['-c', `import ipaddress,socket,sys
from urllib.parse import urlsplit
u=urlsplit(sys.argv[1]); assert u.scheme=='https' and not u.username and not u.password
assert all(ipaddress.ip_address(a[4][0]).is_global for a in socket.getaddrinfo(u.hostname,u.port or 443))`, origin]);
const destination = process.env.REVARO_PUBLIC_RESULTS;
if (!destination || !process.env.REVARO_PUBLIC_STORAGE_STATE) throw new Error('Set results and private storage-state paths');
const previewPath = `/api/files/${fileId}/preview`;
const repeatCount = Number(process.env.REVARO_PUBLIC_REPEATS || 2);
if (!Number.isInteger(repeatCount) || repeatCount < 1) throw new Error('Repeats must be a positive integer');
const modes = (process.env.REVARO_PUBLIC_MODES || 'deployed,native-control').split(',');
const requestedProtocols = (process.env.REVARO_PUBLIC_PROTOCOLS || 'h2,h3-auto').split(',');
const scenarios = (process.env.REVARO_PUBLIC_SCENARIOS || 'play-seek,offline-recovery').split(',');
if (modes.some(m => !['deployed','native-control'].includes(m)) || requestedProtocols.some(p => !['h2','h3-auto'].includes(p))
    || scenarios.some(s => !['play-seek','offline-recovery'].includes(s))) throw new Error('Invalid test selection');
const stamp = () => ({ utc: new Date().toISOString(), beijing: new Date().toLocaleString('sv-SE', { timeZone: 'Asia/Shanghai' }) + ' +08:00' });
const report = {
  target: origin, videoId: fileId, peakWindow: 'Asia/Shanghai 18:00-24:00', started: stamp(),
  scope: 'Real public endpoint; controlled native video element with deployed transport scripts. No player progress writes.',
  notes: ['Native-control changes only the disposable test browser, never the public deployment.',
    'Counts are actual network requests observed in page and worker CDP sessions; synthetic worker responses are excluded.',
    'Body bytes use CDP dataReceived, including bytes received by cancelled requests, never advertised Content-Length.',
    'Offline recovery is a 3-second browser network interruption, not measured packet loss or a real mobile handover.',
    'Fresh contexts, browser HTTP cache disabled; worker cache starts empty but can populate during the sample.'],
  samples: [],
};
await mkdir(dirname(destination), { recursive: true });
const save = () => writeFile(destination, JSON.stringify(report, null, 2) + '\n');

// A flat CDP connection observes the worker's actual wire requests as well as
// page requests. It stores only Range and selected response fields, no cookies.
async function observer(endpoint) {
  const ws = new WebSocket(endpoint);
  await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
  let sequence = 0;
  const pending = new Map(), sessions = new Map(), attached = new Set(), records = new Map();
  const commands = [];
  const send = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
    const id = ++sequence; pending.set(id, { resolve, reject });
    ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
  });
  async function attach(info) {
    if (!['page', 'service_worker'].includes(info.type) || attached.has(info.targetId)) return;
    attached.add(info.targetId);
    const { sessionId } = await send('Target.attachToTarget', { targetId: info.targetId, flatten: true });
    sessions.set(sessionId, info.type);
    await send('Network.enable', {}, sessionId);
    await send('Network.setCacheDisabled', { cacheDisabled: true }, sessionId);
  }
  ws.onmessage = event => {
    const message = JSON.parse(event.data);
    if (message.id) {
      const p = pending.get(message.id); if (!p) return;
      pending.delete(message.id);
      if (message.error) p.reject(new Error(message.error.message)); else p.resolve(message.result);
      return;
    }
    const p = message.params || {}, session = message.sessionId;
    if (message.method === 'Target.targetCreated') {
      commands.push(attach(p.targetInfo).catch(() => {})); return;
    }
    const key = `${session}:${p.requestId}`, row = records.get(key);
    if (message.method === 'Network.requestWillBeSent') {
      const url = new URL(p.request.url);
      if (url.origin !== origin) return;
      // No URL query, authorization, file names, or cookie fields are retained.
      const headers = Object.fromEntries(Object.entries(p.request.headers).map(([k,v]) => [k.toLowerCase(), v]));
      records.set(key, { owner: sessions.get(session), path: url.pathname, method: p.request.method,
        range: headers.range || null, started: p.timestamp, bodyBytes: 0, wireBodyBytes: 0 });
    } else if (row && message.method === 'Network.requestWillBeSentExtraInfo') {
      const headers = Object.fromEntries(Object.entries(p.headers).map(([k,v]) => [k.toLowerCase(), v]));
      row.range = headers.range || row.range;
    } else if (row && message.method === 'Network.responseReceived') {
      const h = Object.fromEntries(Object.entries(p.response.headers).map(([k,v]) => [k.toLowerCase(), v]));
      Object.assign(row, { status: p.response.status, protocol: p.response.protocol,
        fromServiceWorker: !!p.response.fromServiceWorker, fromDiskCache: !!p.response.fromDiskCache,
        contentRange: h['content-range'] || null, contentLength: Number(h['content-length']) || null,
        responseAt: p.timestamp, remoteIp: p.response.remoteIPAddress });
    } else if (row && message.method === 'Network.dataReceived') {
      row.bodyBytes += p.dataLength; row.wireBodyBytes += p.encodedDataLength;
      if (row.lastDataAt) row.maxDataGapSeconds = Math.max(row.maxDataGapSeconds || 0, p.timestamp - row.lastDataAt);
      row.firstDataAt ??= p.timestamp; row.lastDataAt = p.timestamp;
    } else if (row && message.method === 'Network.loadingFinished') {
      row.finished = true; row.ended = p.timestamp; row.totalEncodedBytes = p.encodedDataLength;
    } else if (row && message.method === 'Network.loadingFailed') {
      row.failed = p.errorText; row.cancelled = !!p.canceled; row.ended = p.timestamp;
    }
  };
  await send('Target.setDiscoverTargets', { discover: true });
  for (const info of (await send('Target.getTargets')).targetInfos) await attach(info);
  return {
    records, settle: () => Promise.allSettled(commands),
    async offline(value) { await Promise.allSettled([...sessions].map(([session]) => send('Network.emulateNetworkConditions', {
      offline: value, latency: 0, downloadThroughput: -1, uploadThroughput: -1,
    }, session))); },
    close() { ws.close(); for (const p of pending.values()) p.reject(new Error('observer closed')); pending.clear(); },
  };
}

async function sample(mode, transport, scenario, repeat) {
  const profile = await mkdtemp(join(tmpdir(), 'revaro-public-browser-'));
  const chromeProcess = spawn(process.env.CHROMIUM_PATH || '/usr/bin/chromium', [
    '--headless=new', '--no-sandbox', '--disable-dev-shm-usage', '--autoplay-policy=no-user-gesture-required',
    '--remote-debugging-address=127.0.0.1', '--remote-debugging-port=0', `--user-data-dir=${profile}`,
    transport === 'h2' ? '--disable-quic' : '--enable-quic', 'about:blank',
  ], { stdio: ['ignore', 'ignore', 'pipe'] });
  let browser, watch, context, page;
  const row = { mode, requestedProtocol: transport, scenario, repeat, started: stamp() };
  const onWire = r => r.path === previewPath && (row.workerControlled
    ? r.owner === 'service_worker' || (!!r.protocol && !r.fromServiceWorker)
    : !r.fromServiceWorker);
  try {
    const endpoint = await new Promise((resolve, reject) => {
      let text = '';
      const timer = setTimeout(() => reject(new Error('Chromium startup timed out')), 15000);
      chromeProcess.stderr.on('data', value => { text += value.toString(); const match = text.match(/DevTools listening on (ws:\/\/\S+)/);
        if (match) { clearTimeout(timer); resolve(match[1]); } });
      chromeProcess.once('exit', code => { clearTimeout(timer); reject(new Error(`Chromium startup exit ${code}`)); });
    });
    browser = await chromium.connectOverCDP(endpoint);
    watch = await observer(endpoint);
    context = await browser.newContext({ storageState: JSON.parse(await readFile(process.env.REVARO_PUBLIC_STORAGE_STATE, 'utf8')),
      serviceWorkers: mode === 'native-control' ? 'block' : 'allow' });
    page = await context.newPage();
    await watch.settle();
    await page.goto(`${origin}/readyz`, { waitUntil: 'domcontentloaded', timeout: 20000 });
    row.negotiation = await page.evaluate(async () => {
      const probes = [];
      for (let n = 0; n < 5; n++) {
        const url = `${location.origin}/readyz?public_protocol_probe=${n}`;
        const start = performance.now();
        const response = await fetch(url, { cache: 'no-store' }); await response.text();
        probes.push({ status: response.status, protocol: performance.getEntriesByName(url).at(-1)?.nextHopProtocol,
          milliseconds: performance.now() - start });
        await new Promise(resolve => setTimeout(resolve, 250));
      }
      return probes;
    });
    if (transport === 'h3-auto' && !row.negotiation.some(p => p.protocol === 'h3')) {
      row.result = 'HTTP/3 not negotiated; browser used H2 fallback. H3 media not measured.';
      return row;
    }
    if (transport === 'h2' && !row.negotiation.every(p => p.protocol === 'h2')) throw new Error('H2 not verified');
    if (scenario === 'negotiation') { row.result = 'HTTP/3 negotiated naturally'; return row; }
    if (mode === 'deployed') {
      await page.evaluate(async () => { await (await import('/transport-client.js')).initializeTransport(); });
      await page.waitForFunction(() => !!navigator.serviceWorker.controller, null, { timeout: 12000 });
      await watch.settle();
    }
    row.workerControlled = await page.evaluate(() => !!navigator.serviceWorker.controller);
    const setup = await page.evaluate(async path => {
      document.body.textContent = '';
      const video = document.createElement('video'); video.controls = true; video.muted = true;
      video.playsInline = true; video.preload = 'auto'; video.style.width = '640px'; document.body.append(video);
      const m = window.__publicMedia = { events: [], firstFrameMs: null, phase: 'startup', origin: performance.now() };
      for (const name of ['waiting','stalled','playing','pause','seeking','seeked','error','ended','loadedmetadata']) {
        video.addEventListener(name, () => m.events.push({ name, milliseconds: performance.now() - m.origin,
          phase: m.phase, time: video.currentTime, readyState: video.readyState,
          errorCode: video.error?.code || null }));
      }
      const firstFrame = new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error('No decoded frame in 30s')), 30000);
        video.requestVideoFrameCallback(() => { clearTimeout(timer); m.firstFrameMs = performance.now() - m.origin; m.phase = 'play'; resolve(); });
      });
      video.src = path; await Promise.all([video.play(), firstFrame]);
      return { firstFrameMs: m.firstFrameMs, durationSeconds: video.duration, width: video.videoWidth, height: video.videoHeight };
    }, previewPath);
    Object.assign(row, setup);
    if (scenario === 'play-seek') {
      await page.waitForTimeout(8000);
      row.initialPlayback = await page.evaluate(() => {
        const v = document.querySelector('video'), m = window.__publicMedia;
        return { time: v.currentTime, readyState: v.readyState, events: [...m.events],
          elapsedMs: performance.now() - m.origin, decodedFrames: v.getVideoPlaybackQuality().totalVideoFrames };
      });
      const initialRequests = [...watch.records.values()].filter(onWire);
      row.initialPlayback.networkRequestCount = initialRequests.length;
      row.initialPlayback.bodyBytes = initialRequests.reduce((n,r) => n + r.bodyBytes, 0);
      row.seeks = [];
      for (const fraction of [.81, .22, .64, .41]) {
        row.seekInProgress = fraction;
        const before = [...watch.records.values()].filter(onWire).length;
        const seek = await page.evaluate(async fraction => {
          const v = document.querySelector('video'), m = window.__publicMedia;
          m.phase = 'seek'; v.pause(); const target = v.duration * fraction, begun = performance.now();
          const sought = new Promise((resolve, reject) => {
            const timer = setTimeout(() => reject(new Error('Seek timed out in 20s')), 20000);
            v.addEventListener('seeked', () => { clearTimeout(timer); resolve(); }, { once: true });
          });
          v.currentTime = target; await sought;
          const result = { targetSeconds: target, reachedSeconds: v.currentTime, seekReadyMs: performance.now() - begun,
            readyState: v.readyState, buffered: Array.from({ length: v.buffered.length }, (_, i) => [v.buffered.start(i), v.buffered.end(i)]) };
          await v.play();
          await new Promise((resolve, reject) => {
            const timer = setTimeout(() => reject(new Error('Seek decoded frame timed out')), 10000);
            v.requestVideoFrameCallback(() => { clearTimeout(timer); resolve(); });
          });
          result.decodedFrameMs = performance.now() - begun; m.phase = 'play'; return result;
        }, fraction);
        await page.waitForTimeout(700);
        const after = [...watch.records.values()].filter(onWire).length;
        row.seeks.push({ ...seek, newNetworkRequests: after - before });
        delete row.seekInProgress;
      }
      await page.waitForTimeout(2500);
    } else {
      await page.waitForTimeout(800);
      row.interruption = { started: stamp(), before: await page.evaluate(() => {
        const v = document.querySelector('video'); window.__publicMedia.phase = 'offline';
        return { time: v.currentTime, readyState: v.readyState, networkState: v.networkState,
          buffered: Array.from({ length: v.buffered.length }, (_, i) => [v.buffered.start(i), v.buffered.end(i)]) };
      }) };
      await watch.offline(true); await context.setOffline(true);
      await page.waitForTimeout(3000);
      row.interruption.afterOffline = await page.evaluate(() => ({ time: document.querySelector('video').currentTime, readyState: document.querySelector('video').readyState }));
      await context.setOffline(false); await watch.offline(false);
      row.interruption.restored = stamp();
      const recovery = await page.evaluate(async () => {
        const v = document.querySelector('video'), m = window.__publicMedia, before = v.currentTime, begun = performance.now();
        m.phase = 'recovery';
        return new Promise(resolve => {
          let movingSince = null, previousTime = before, firstClockAdvanceMs = null;
          const poll = setInterval(() => {
            const now = performance.now();
            if (v.currentTime > previousTime + .02 && v.readyState >= 2) {
              movingSince ??= now;
              firstClockAdvanceMs ??= now - begun;
            } else movingSince = null;
            previousTime = v.currentTime;
            if (movingSince !== null && now - movingSince >= 5000) {
              clearInterval(poll); clearTimeout(timer);
              resolve({ sustainedPlayback: true, firstClockAdvanceMs,
                stablePlaybackStartedMs: movingSince - begun, confirmationMs: now - begun, sustainedPlaybackMs: now - movingSince });
            }
          }, 200);
          const timer = setTimeout(() => { clearInterval(poll); resolve({ clockAdvanced: false, milliseconds: performance.now() - begun,
            sustainedPlayback: false, firstClockAdvanceMs,
            readyState: v.readyState, networkState: v.networkState, errorCode: v.error?.code || null }); }, 20000);
        });
      });
      row.interruption.recovery = recovery;
      await page.waitForTimeout(2000);
    }
    row.media = await page.evaluate(() => {
      const v = document.querySelector('video'), m = window.__publicMedia;
      const result = { events: m.events, finalTime: v.currentTime, readyState: v.readyState, errorCode: v.error?.code || null,
        totalWallMs: performance.now() - m.origin, decodedFrames: v.getVideoPlaybackQuality().totalVideoFrames,
        droppedFrames: v.getVideoPlaybackQuality().droppedVideoFrames };
      v.pause(); v.removeAttribute('src'); v.load(); return result;
    });
    await page.waitForTimeout(300);
    row.result = 'measured';
  } catch (error) { row.result = 'failed'; row.error = error.message; }
  finally {
    if (!row.media && page && !page.isClosed()) {
      row.media = await page.evaluate(() => {
        const v = document.querySelector('video'), m = window.__publicMedia;
        if (!v || !m) return null;
        const result = { events: m.events, finalTime: v.currentTime, readyState: v.readyState, errorCode: v.error?.code || null,
          totalWallMs: performance.now() - m.origin, decodedFrames: v.getVideoPlaybackQuality().totalVideoFrames,
          droppedFrames: v.getVideoPlaybackQuality().droppedVideoFrames };
        v.pause(); v.removeAttribute('src'); v.load(); return result;
      }).catch(() => null);
      await page.waitForTimeout(300).catch(() => {});
    }
    if (watch) {
      // Freeze the observation window before closing the context. Late teardown
      // events must not mutate recorded requests after totals were computed.
      row.requests = [...watch.records.values()].filter(onWire).map(r => ({ ...r }));
      const counts = {};
      for (const request of row.requests) { const key = request.range || 'full'; counts[key] = (counts[key] || 0) + 1; }
      const ranges = row.requests.map(r => /^bytes=(\d+)-(\d+)$/.exec(r.range || '')).filter(Boolean).map(m => Number(m[2]) - Number(m[1]) + 1);
      const playbackWaits = row.media?.events.filter(e => e.name === 'waiting' && ['play','offline','recovery'].includes(e.phase)) || [];
      // Pause/seeking end a playback stall. Merge repeated waiting notifications
      // so one interrupted interval is never counted twice.
      const intervals = playbackWaits.map(e => {
        const resume = row.media.events.find(n => ['playing','pause','seeking','ended'].includes(n.name) && n.milliseconds > e.milliseconds);
        return [e.milliseconds, resume?.milliseconds ?? row.media.totalWallMs];
      }).sort((a,b) => a[0]-b[0]);
      let stallMs = 0, previousEnd = 0;
      for (const [start,end] of intervals) { stallMs += Math.max(0,end-Math.max(start,previousEnd)); previousEnd = Math.max(previousEnd,end); }
      row.network = { requestCount: row.requests.length, protocols: [...new Set(row.requests.map(r => r.protocol).filter(Boolean))],
        receivedBodyBytes: row.requests.reduce((n,r) => n + r.bodyBytes, 0),
        repeatedIdenticalRangeRequests: Object.values(counts).reduce((n,c) => n + Math.max(0,c - 1), 0),
        requestsAtMost64KiB: ranges.filter(n => n <= 65536).length, requestsAtMost256KiB: ranges.filter(n => n <= 262144).length,
        boundedRangeSizes: [...new Set(ranges)].sort((a,b) => a-b),
        failedRequests: row.requests.filter(r => r.failed).length, nonCancelledFailures: row.requests.filter(r => r.failed && !r.cancelled).length,
        playbackWaitingEvents: playbackWaits.length, playbackStallMs: stallMs };
      if (!row.media) { row.network.playbackWaitingEvents = null; row.network.playbackStallMs = null; }
      row.nonReadOnlyRequestCount = [...watch.records.values()].filter(r => !['GET','HEAD','OPTIONS'].includes(r.method)).length;
    }
    row.finished = stamp();
    await context?.close().catch(() => {});
    watch?.close();
    await browser?.close().catch(() => {});
    chromeProcess.kill('SIGTERM');
    await rm(profile, { recursive: true, force: true }).catch(() => {});
  }
  return row;
}

report.browser = execFileSync(process.env.CHROMIUM_PATH || '/usr/bin/chromium', ['--version'], { encoding: 'utf8' }).trim();
const enabled = new Map();
for (const mode of modes) {
  const probe = await sample(mode, 'h3-auto', 'negotiation', 0);
  report.samples.push(probe); await save();
  console.log(JSON.stringify({ mode, requestedProtocol: 'h3-auto', result: probe.result, protocols: probe.negotiation?.map(p => p.protocol) }));
  enabled.set(mode, (probe.result === 'HTTP/3 negotiated naturally' ? ['h2','h3-auto'] : ['h2']).filter(p => requestedProtocols.includes(p)));
}
for (let repeat = 0; repeat < repeatCount; repeat++) {
  for (const mode of repeat % 2 ? [...modes].reverse() : modes) {
    for (const transport of repeat % 2 ? [...enabled.get(mode)].reverse() : enabled.get(mode)) {
      for (const scenario of scenarios) {
        const row = await sample(mode, transport, scenario, repeat);
        report.samples.push(row); await save();
        console.log(JSON.stringify({ mode, requestedProtocol: transport, scenario, repeat, result: row.result, error: row.error, firstFrameMs: row.firstFrameMs,
          network: row.network, recovery: row.interruption?.recovery }));
      }
    }
  }
}
report.finished = stamp(); await save();
