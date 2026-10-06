// Standard browser HTTP/3 interoperability and the existing shared worker.
// Run against a disposable instance; creates and then purges its own fixtures.
import { chromium, firefox } from '../e2e/node_modules/playwright/index.mjs';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { readlinkSync, writeFileSync } from 'node:fs';
import assert from 'node:assert/strict';

const url = process.env.REVARO_QUIC_URL || 'https://localhost:18443';
const cert = process.env.REVARO_QUIC_CERT;
assert(cert, 'REVARO_QUIC_CERT must point to the disposable server certificate');
const pem = execFileSync('openssl', ['x509', '-in', cert, '-pubkey', '-noout']);
const der = execFileSync('openssl', ['pkey', '-pubin', '-outform', 'DER'], { input: pem });
const spki = createHash('sha256').update(der).digest('base64');
const useNetem = process.argv.includes('--netem');
const blockUdp = process.argv.includes('--block-udp');
if (useNetem || blockUdp) assert(process.env.REVARO_BENCH_PARENT_NS &&
  readlinkSync('/proc/self/ns/net') !== process.env.REVARO_BENCH_PARENT_NS,
  'Refusing to change host qdisc');
function netem(loss) {
  execFileSync('/usr/sbin/tc', ['qdisc', 'replace', 'dev', 'lo', 'root', 'netem',
    'delay', '60ms', '10ms', 'loss', `${loss}%`, 'rate', '20mbit', 'limit', '1000']);
}
function clearNetem() {
  try { execFileSync('/usr/sbin/tc', ['qdisc', 'del', 'dev', 'lo', 'root'], { stdio: 'ignore' }); } catch {}
}
const engine = process.env.REVARO_QUIC_BROWSER || 'chromium';
const firefoxOptions = { firefoxUserPrefs: { 'network.http.http3.enable': true,
  'network.dns.disableIPv6': true, 'network.http.http3.disable_when_third_party_roots_found': false } };
const browser = engine === 'firefox' && !process.env.REVARO_QUIC_NSS_PROFILE ? await firefox.launch(firefoxOptions)
  : engine === 'firefox' ? null : await chromium.launch({ executablePath: process.env.CHROMIUM_PATH || '/usr/bin/chromium',
  args: ['--no-sandbox', '--enable-quic', '--ignore-certificate-errors', '--host-resolver-rules=MAP localhost 127.0.0.1',
    ...(process.env.REVARO_QUIC_NETLOG ? [`--log-net-log=${process.env.REVARO_QUIC_NETLOG}`] : []),
    ...(process.env.REVARO_QUIC_FORCE ? [`--origin-to-force-quic-on=${new URL(url).host}`] : []),
    `--ignore-certificate-errors-spki-list=${spki}`] });
const context = process.env.REVARO_QUIC_NSS_PROFILE && engine === 'firefox'
  ? await firefox.launchPersistentContext(process.env.REVARO_QUIC_NSS_PROFILE, { ...firefoxOptions, ignoreHTTPSErrors: true })
  : await browser.newContext({ ignoreHTTPSErrors: true });
const page = await context.newPage();
page.on('pageerror', error => console.error('browser page error:', error.message));
page.on('console', message => { if (message.type() === 'error') console.error('browser console:', message.text()); });
const owned = [], results = [], fallbackRequests = [];
context.on('request', request => {
  if (new URL(request.url()).port === '18444') fallbackRequests.push(request.url());
});
try {
  await page.goto(url);
  await page.waitForFunction(() => !!navigator.serviceWorker.controller, undefined, { timeout: 10000 })
    .catch(async error => { throw new Error(`${engine} transport initialization: ${await page.locator('#app').innerText()}; ${error.message}`); });
  await page.evaluate(async ({ password }) => {
    const { initializeTransport } = await import('/transport-client.js');
    await initializeTransport();
    const login = await fetch('/api/auth/login', { method: 'POST',
      headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ username: 'admin', password }) });
    if (!login.ok) throw new Error(`login HTTP ${login.status}`);
  }, { password: process.env.REVARO_QUIC_PASSWORD || 'quic-benchmark-password' });
  let protocol;
  // Temporary certificates may require force-QUIC to allow an unknown root.
  // This is a test-browser flag; the wire protocol is still standard HTTP/3.
  for (let n = 0; n < 10; n++) {
    protocol = await page.evaluate(async n => {
      const url = new URL(`/readyz?quic_probe=${n}`, location.origin).href;
      await (await fetch(url, { headers: { 'X-Revaro-Managed': '1' } })).text();
      return performance.getEntriesByName(url).at(-1)?.nextHopProtocol;
    }, n);
    if (protocol === 'h3') break;
    await page.waitForTimeout(250);
  }
  assert.equal(protocol, 'h3', `${engine} must negotiate standard HTTP/3`);
  const files = await page.evaluate(async () => {
    const { putBlob } = await import('/transport-client.js');
    const files = [];
    for (const [extension, mime] of [['pdf', 'application/pdf'], ['zip', 'application/zip'],
      ['txt', 'text/plain'], ['dat', 'application/octet-stream']]) {
      const size = extension === 'dat' ? 6 * 1024 * 1024 : 1024 * 1024;
      const bytes = Uint8Array.from({ length: size }, (_, n) => n % 251);
      const hash = Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', bytes)), n => n.toString(16).padStart(2, '0')).join('');
      const created = await fetch('/api/uploads', { method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ parent_id: '00000000-0000-0000-0000-000000000000',
          name: `quic-browser-${Date.now()}.${extension}`, size, mime_type: mime }) });
      if (!created.ok) throw new Error(`create HTTP ${created.status}`);
      const upload = await created.json();
      files.push({ id: upload.file_id, extension, size, hash });
      const parts = [];
      for (let start = 0, part = 1; start < size; start += upload.part_size, part++) {
        const etag = await putBlob(`/api/uploads/${upload.upload_id}/data/${part}`,
          new Blob([bytes.subarray(start, start + upload.part_size)]), mime, undefined, () => {});
        parts.push({ part_number: part, etag });
      }
      const completed = await fetch(`/api/uploads/${upload.upload_id}/complete`, { method: 'POST',
        headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ parts }) });
      if (!completed.ok) throw new Error(`complete HTTP ${completed.status}`);
    }
    return files;
  });
  owned.push(...files.map(f => f.id));
  for (const file of files) {
    if (useNetem) netem(15);
    const download = page.evaluate(async file => {
      const started = performance.now(), gaps = [];
      let last, received = 0;
      const response = await fetch(`/api/files/${file.id}/download`);
      if (!response.ok || !response.headers.get('etag')) throw new Error(`download HTTP ${response.status}`);
      const reader = response.body.getReader(), chunks = [];
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        const now = performance.now();
        if (last !== undefined) gaps.push(now - last);
        last = now; received += value.length; chunks.push(value);
      }
      const bytes = new Uint8Array(received);
      let offset = 0;
      for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
      const hash = Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', bytes)), n => n.toString(16).padStart(2, '0')).join('');
      if (received !== file.size || hash !== file.hash) throw new Error('download hash/length mismatch');
      return { extension: file.extension, bytes: received, hashVerified: true,
        seconds: (performance.now() - started) / 1000, maxProgressGapSeconds: Math.max(0, ...gaps) / 1000 };
    }, file);
    if (useNetem && file.extension === 'dat') {
      await page.waitForTimeout(3000);
      netem(100);
      await page.waitForTimeout(2000);
      netem(15);
    }
    const result = await download;
    results.push(result);
    console.log(JSON.stringify(result));
    if (useNetem) clearNetem();
  }
  if (blockUdp) {
    // Leave TCP working: the configured second authority must continue over h2.
    execFileSync('/usr/sbin/tc', ['qdisc', 'add', 'dev', 'lo', 'root', 'handle', '1:', 'prio']);
    execFileSync('/usr/sbin/tc', ['qdisc', 'add', 'dev', 'lo', 'parent', '1:3', 'handle', '30:', 'netem', 'loss', '100%']);
    execFileSync('/usr/sbin/tc', ['filter', 'add', 'dev', 'lo', 'protocol', 'ip', 'parent', '1:', 'prio', '1',
      'u32', 'match', 'ip', 'protocol', '17', '0xff', 'flowid', '1:3']);
    const result = await page.evaluate(async () => {
      const { configureTransport, bufferedRequest, transportState } = await import('/transport-core.js');
      configureTransport({ http2_origin: `${location.protocol}//${location.hostname}:18444` });
      const started = performance.now();
      const response = await bufferedRequest(new Request(`${location.origin}/api/transport/config?fallback_probe=1`,
        { headers: { 'X-Revaro-Managed': '1' } }), true);
      const state = transportState();
      if (!response.ok) throw new Error(`fallback failed: ${JSON.stringify(state)}`);
      let primaryProtocol = null;
      if (!state.fallback) {
        const primaryUrl = `${location.origin}/api/transport/config?browser_fallback_protocol=1`;
        await (await fetch(primaryUrl, { headers: { 'X-Revaro-Managed': '1' }, signal: AbortSignal.timeout(3000) })).text();
        primaryProtocol = performance.getEntriesByName(primaryUrl).at(-1)?.nextHopProtocol;
      }
      if (!state.fallback && primaryProtocol !== 'h2') throw new Error(`neither browser nor transport fell back: ${JSON.stringify(state)}`);
      const url = `${state.http2Origin}/api/transport/config?protocol_probe=1`;
      await (await fetch(url, { headers: { 'X-Revaro-Managed': '1' }, credentials: 'include' })).text();
      return { seconds: (performance.now() - started) / 1000, state,
        primaryProtocol, nextHopProtocol: performance.getEntriesByName(url).at(-1)?.nextHopProtocol };
    });
    assert.equal(result.nextHopProtocol, 'h2');
    results.push({ http2Fallback: result });
    clearNetem();
    console.log(JSON.stringify({ http2Fallback: result }));
  }
  if (process.env.REVARO_QUIC_RESULT) writeFileSync(process.env.REVARO_QUIC_RESULT,
    JSON.stringify({ engine, browser: (browser || context.browser()).version(), protocol,
      testForcedQuic: Boolean(process.env.REVARO_QUIC_FORCE), netem: useNetem, results, fallbackRequests }, null, 2));
} finally {
  if (useNetem || blockUdp) clearNetem();
  for (const id of owned) {
    await page.evaluate(async id => {
      await fetch(`/api/files/${id}`, { method: 'DELETE' });
      await fetch(`/api/trash/${id}`, { method: 'DELETE' });
    }, id).catch(() => {});
  }
  await context.close();
  await browser?.close();
}
