import { selectRenderer } from './renderer.js';

const error = document.getElementById('error');
const canvas = document.getElementById('siglus-canvas');
const loading = document.getElementById('loading');
const audioHint = document.getElementById('audio-hint');
const token = location.pathname.split('/').filter(Boolean)[1];
const archiveUrl = `/g/${encodeURIComponent(token)}/archive`;
const files = new Map();
const overlay = new Map();
const children = new Map();
const cache = new Map();
const startupPins = new Set();
const cacheLimit = 64 * 1024 * 1024;
let cacheBytes = 0;
// One current voice archive, independent of the small-resource LRU. Total
// retained resource bytes stay bounded by 64 MiB + 64 MiB.
let voicePack;
const voicePackLimit = 64 * 1024 * 1024;
let meta;
let db;
let profile;
let failed = false;
let engineError = '';
// Browsers suspend automatically created audio contexts until the first gesture.
// Resume them on the game canvas; no separate launch button is needed.
const audioContexts = new Set();
let disposedAudioContexts = 0;
const NativeAudioContext = globalThis.AudioContext || globalThis.webkitAudioContext;
if (NativeAudioContext) {
    const TrackedAudioContext = new Proxy(NativeAudioContext, {
        construct(Target, args) {
            const context = Reflect.construct(Target, args);
            // CPAL can retire probe streams with resume/close promises still
            // pending. Closing twice and resuming a retired stream must not
            // become unhandled rejections on stricter WebAudio implementations.
            const nativeClose = context.close.bind(context);
            let closing;
            context.close = () => {
                if (closing) return closing;
                disposedAudioContexts++;
                if (context.state === 'closed') return Promise.resolve();
                closing = nativeClose().catch(reason => {
                    if (context.state === 'closed' && isClosedAudioError(reason)) return;
                    throw reason;
                });
                return closing;
            };
            for (const method of ['resume', 'suspend']) {
                const original = context[method].bind(context);
                context[method] = () => {
                    if (closing || context.state === 'closed') return Promise.resolve();
                    return original().catch(reason => {
                        if ((closing || context.state === 'closed') && isClosedAudioError(reason)) return;
                        throw reason;
                    });
                };
            }
            audioContexts.add(context);
            context.addEventListener('statechange', updateAudioHint);
            updateAudioHint();
            return context;
        },
    });
    if (globalThis.AudioContext) globalThis.AudioContext = TrackedAudioContext;
    else globalThis.webkitAudioContext = TrackedAudioContext;
}
function updateAudioHint() {
    audioHint.hidden = failed || !Array.from(audioContexts).some(context => context.state === 'suspended');
}
function resumeAudio() {
    for (const context of audioContexts) {
        if (context.state === 'closed') audioContexts.delete(context);
        else if (context.state === 'suspended') context.resume().catch(() => {});
    }
}
function startSoftwareCanvas() {
    const context = canvas.getContext('2d', { alpha: false });
    if (!context) throw Error('无法创建 Canvas2D 软件画布。');
    let image, paintedBytes, paintedRevision, paintedWidth, paintedHeight;
    const backing = document.createElement('canvas');
    const backingContext = backing.getContext('2d', { alpha: false });
    const resize = () => {
        const dpr = globalThis.devicePixelRatio || 1;
        canvas.width = Math.max(1, Math.round(canvas.clientWidth * dpr));
        canvas.height = Math.max(1, Math.round(canvas.clientHeight * dpr));
    };
    resize();
    const paint = () => {
        const dpr = globalThis.devicePixelRatio || 1;
        const canvasWidth = Math.max(1, Math.round(canvas.clientWidth * dpr));
        const canvasHeight = Math.max(1, Math.round(canvas.clientHeight * dpr));
        if (canvas.width !== canvasWidth || canvas.height !== canvasHeight) resize();
        const bytes = globalThis.__siglusSoftwareFrame;
        const size = globalThis.__siglusSoftwareSize;
        if (bytes && size?.length === 2) {
            const width = Number(size[0]), height = Number(size[1]);
            const revision = globalThis.__siglusSoftwareFrameRevision;
            if (bytes === paintedBytes && revision === paintedRevision && canvas.width === paintedWidth && canvas.height === paintedHeight) {
                requestAnimationFrame(paint);
                return;
            }
            // The published frame is already a JS-owned snapshot. Share that
            // buffer with ImageData instead of copying another 8 MB per frame.
            if (!image || image.data.buffer !== bytes.buffer || image.data.byteOffset !== bytes.byteOffset
                || image.width !== width || image.height !== height) {
                image = new ImageData(new Uint8ClampedArray(bytes.buffer, bytes.byteOffset, bytes.byteLength), width, height);
            }
            if (backing.width !== width || backing.height !== height) { backing.width = width; backing.height = height; }
            backingContext.putImageData(image, 0, 0);
            context.imageSmoothingEnabled = false;
            // Canvas2D fallback fills the CSS canvas. GPU mode keeps the
            // engine's aspect-ratio viewport; software mode avoids large
            // black bars on ultrawide displays where no GPU viewport exists.
            context.drawImage(backing, 0, 0, canvas.width, canvas.height);
            paintedBytes = bytes; paintedRevision = revision;
            paintedWidth = canvas.width; paintedHeight = canvas.height;
        }
        requestAnimationFrame(paint);
    };
    requestAnimationFrame(paint);
}
for (const type of ['pointerdown', 'keydown']) document.addEventListener(type, resumeAudio, { capture: true });
const reads = [];
window.siglusProbe = { reads, voiceReads: [], overlayReads: [], saveWrites: [], phase: 'loading' };

function fail(reason) {
    // A Rust panic logs its cause before aborting with a generic WASM trap.
    // Keep that cause visible instead of replacing it with "unreachable".
    if (failed && /^(?:RuntimeError: )?unreachable$/i.test(reason?.message || String(reason))) return;
    failed = true;
    loading.hidden = true;
    audioHint.hidden = true;
    error.hidden = false;
    error.textContent = `游戏启动失败\n${reason?.message || String(reason)}`;
    window.siglusProbe.phase = 'failed';
}

// Host initialization happens asynchronously inside the WASM event loop and
// reports failures through console.error instead of rejecting its start call.
const originalConsoleError = console.error.bind(console);
console.error = (...args) => {
    originalConsoleError(...args);
    const message = args.map(String).join(' ');
    if (/host init:|renderer init:|create wasm window:|siglus wasm step failed:|panicked at|RuntimeError:/i.test(message)) {
        engineError = message;
        fail(message);
    }
};
function isClosedAudioError(reason) {
    const message = reason?.message || String(reason);
    return /^(?:AudioContext is closed\.?|Cannot (?:close|resume|suspend) a closed AudioContext\.?)$/i.test(message);
}
function reportBrowserError(event, reason) {
    // A retired CPAL probe can report its disposal after another stream is
    // already playing. That lifecycle notification does not stop the VM.
    // Actual host/panic/import failures still go through fail unconditionally.
    if (isClosedAudioError(reason) && disposedAudioContexts > 0
        && Array.from(audioContexts).some(context => context.state !== 'closed')) {
        event.preventDefault();
        console.warn('Ignored a retired audio context notification:', reason?.message || reason);
        return;
    }
    fail(reason);
}
window.addEventListener('error', event => reportBrowserError(event, event.error || event.message));
window.addEventListener('unhandledrejection', event => reportBrowserError(event, event.reason));

function norm(path) {
    path = String(path).replaceAll('\\', '/');
    if (path.split('/').includes('..') || path.includes('\0')) throw Error('Invalid game path');
    return path.split('/').filter(part => part && part !== '.').join('/').toLowerCase();
}
function indexChildren(path) {
    let dir = '';
    for (const part of path.split('/')) {
        if (!children.has(dir)) children.set(dir, new Set());
        children.get(dir).add(part);
        dir = dir ? `${dir}/${part}` : part;
    }
}
function index(path, value) {
    const key = norm(path);
    files.set(key, value);
    indexChildren(key);
}
const crcTable = Uint32Array.from({ length: 256 }, (_, value) => {
    for (let i = 0; i < 8; i++) value = (value >>> 1) ^ ((value & 1) ? 0xedb88320 : 0);
    return value >>> 0;
});
function crc32(bytes) {
    let crc = -1;
    for (let i = 0; i < bytes.length; i++) crc = (crc >>> 8) ^ crcTable[(crc ^ bytes[i]) & 255];
    return (crc ^ -1) >>> 0;
}
function record(list, value) {
    if (list.length === 128) list.shift();
    list.push(value);
}
function cacheResource(file, data) {
    if (data.length > cacheLimit || cache.has(file.path)) return;
    while (cacheBytes + data.length > cacheLimit && cache.size) {
        const oldest = Array.from(cache.keys()).find(key => !startupPins.has(key));
        if (oldest === undefined) return;
        cacheBytes -= cache.get(oldest).length;
        cache.delete(oldest);
    }
    cache.set(file.path, data);
    cacheBytes += data.length;
}
async function warmStartupResources() {
    const candidates = new Map();
    for (const path of ['Gameexe.dat', 'Scene.pck']) {
        const file = files.get(norm(path));
        if (file) candidates.set(file.path, file);
    }
    for (const file of files.values()) {
        if (/(^|\/)(font[^/]*\.(?:otf|ttf|ttc))$/i.test(file.path)) candidates.set(file.path, file);
    }
    let budget = 48 * 1024 * 1024;
    const tasks = Array.from(candidates.values()).filter(file => {
        if (file.size <= 0 || file.size > 32 * 1024 * 1024 || file.packed <= 0
            || file.packed > 32 * 1024 * 1024 || file.size > budget) return false;
        budget -= file.size;
        return true;
    });
    if (!tasks.length || !globalThis.Worker) return;
    let worker;
    try {
        worker = new Worker(new URL('./resource-worker.js', import.meta.url));
        for (let id = 0; id < tasks.length; id++) {
            const file = tasks[id];
            const bytes = await new Promise((resolve, reject) => {
                const timeout = setTimeout(() => reject(Error('Resource prefetch timed out')), 30000);
                worker.onmessage = ({ data }) => {
                    if (data.id !== id) return;
                    clearTimeout(timeout);
                    data.error ? reject(Error(data.error)) : resolve(data.bytes);
                };
                worker.onerror = event => { event.preventDefault(); clearTimeout(timeout); reject(Error('Resource prefetch worker failed')); };
                worker.postMessage({ id, url: new URL(archiveUrl, location.href).href,
                    offset: file.offset, packed: file.packed, size: file.size, method: file.method, crc: file.crc });
            });
            cacheResource(file, bytes);
            if (/\.(?:otf|ttf|ttc)$/i.test(file.path) && cache.has(file.path)) startupPins.add(file.path);
            record(reads, { path: file.path, source: file.path, bytes: file.packed, status: 206, prefetch: true });
        }
    } catch (error) {
        // Optional warmup: unsupported workers/offline requests keep the VM's
        // validated read fallback and any already-warmed resources.
        console.warn('Resource prefetch unavailable:', error.message);
    } finally { worker?.terminate(); }
}
function read(path, retain = true) {
    const key = norm(path);
    if (overlay.has(key)) {
        record(siglusProbe.overlayReads, key);
        return overlay.get(key);
    }
    const file = files.get(key);
    if (!file) throw Error(`Missing game resource: ${path}`);
    if (voicePack?.path === file.path && voicePack.source === file) return voicePack.bytes;
    if (cache.has(file.path)) {
        const hit = cache.get(file.path);
        cache.delete(file.path);
        cache.set(file.path, hit);
        return hit;
    }
    if (!file.packed) return new Uint8Array();
    if (![file.offset, file.packed, file.size].every(Number.isSafeInteger)
        || file.offset < 0 || file.packed < 0 || file.size < 0
        || file.packed > 512 * 1024 * 1024 || file.size > 512 * 1024 * 1024) {
        throw Error(`Invalid or oversized ZIP resource: ${path}`);
    }
    const xhr = new XMLHttpRequest();
    xhr.open('GET', archiveUrl, false);
    xhr.setRequestHeader('Range', `bytes=${file.offset}-${file.offset + file.packed - 1}`);
    xhr.overrideMimeType('text/plain; charset=x-user-defined');
    xhr.send();
    if (xhr.status !== 206) throw Error(`Range read failed (${xhr.status}): ${path}`);
    const text = xhr.responseText;
    const packed = new Uint8Array(text.length);
    for (let i = 0; i < text.length; i++) packed[i] = text.charCodeAt(i) & 255;
    const data = file.method === 0 ? packed : fflate.inflateSync(packed, { out: new Uint8Array(file.size) });
    if (data.length !== file.size || crc32(data) !== file.crc) throw Error(`Resource size/CRC mismatch: ${path}`);
    if (retain) cacheResource(file, data);
    record(reads, { path, source: file.path, bytes: packed.length, status: xhr.status });
    return data;
}
globalThis.siglusReadOvkEntry = (path, number) => {
    const key = norm(path), file = files.get(key);
    const identity = file?.path || key;
    const source = overlay.get(key) || file;
    let pack = voicePack?.path === identity && voicePack.source === source ? voicePack : null;
    if (!pack) {
        const bytes = read(path, false);
        const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
        if (bytes.length < 4) throw Error('OVK header truncated');
        const count = view.getUint32(0, true);
        if (!count || count > Math.floor((bytes.length - 4) / 16)) throw Error('OVK entry table truncated');
        const entries = new Map();
        for (let i = 0; i < count; i++) {
            const at = 4 + i * 16, size = view.getUint32(at, true);
            const offset = view.getUint32(at + 4, true), no = view.getUint32(at + 8, true);
            if (size && offset + size > bytes.length) throw Error('OVK entry out of range');
            if (!entries.has(no)) entries.set(no, { offset, size });
        }
        pack = { path: identity, source, bytes, entries };
        voicePack = bytes.length <= voicePackLimit ? pack : undefined;
    }
    const entry = pack.entries.get(number);
    if (!entry?.size) throw Error(`OVK voice not found: ${number}`);
    record(siglusProbe.voiceReads, { path, number, bytes: entry.size });
    return pack.bytes.subarray(entry.offset, entry.offset + entry.size);
};
globalThis.siglusFileExists = path => overlay.has(norm(path)) || files.has(norm(path));
globalThis.siglusKnownFileCount = () => files.size + overlay.size;
globalThis.siglusListDir = path => Array.from(children.get(norm(path)) || []);
globalThis.siglusReadFile = read;
globalThis.gameFsWrite = (path, bytes) => {
    const key = norm(path), value = new Uint8Array(bytes);
    overlay.set(key, value);
    indexChildren(key);
    const transaction = db.transaction('files', 'readwrite');
    transaction.objectStore('files').put(value, key);
    transaction.oncomplete = () => record(siglusProbe.saveWrites, { path: key, size: value.length });
    transaction.onerror = () => fail('无法保存浏览器存档，请检查浏览器存储空间。');
};
globalThis.gameFsRemove = path => {
    const key = norm(path);
    overlay.delete(key);
    db.transaction('files', 'readwrite').objectStore('files').delete(key);
};
function openDb() {
    return new Promise((resolve, reject) => {
        const request = indexedDB.open(`${meta.save_space}-v2-${profile}`, 1);
        request.onupgradeneeded = () => request.result.createObjectStore('files');
        request.onerror = () => reject(request.error);
        request.onsuccess = () => resolve(request.result);
    });
}
async function start() {
    try {
        siglusProbe.renderer = await selectRenderer();
        globalThis.siglusRendererBackend = siglusProbe.renderer.backend;
        if (siglusProbe.renderer.backend === 'software') {
            const dpr = globalThis.devicePixelRatio || 1;
            canvas.width = Math.max(1, Math.round(canvas.clientWidth * dpr));
            canvas.height = Math.max(1, Math.round(canvas.clientHeight * dpr));
        }
        const response = await fetch(`/g/${encodeURIComponent(token)}/index`);
        if (!response.ok) throw Error(`无法读取游戏目录（HTTP ${response.status}）`);
        meta = await response.json();
        profile = norm(meta.entry) === 'scene.chs' ? 'chinese' : 'original';
        siglusProbe.profile = profile;
        for (const file of meta.entries) {
            if (!/^(savedata|save)\//i.test(file.path)) index(file.path, file);
        }
        if (profile === 'chinese') {
            for (const [from, to] of [['scene.chs', 'Scene.pck'], ['gameexe.chs', 'Gameexe.dat']]) {
                const file = files.get(norm(from));
                if (!file) throw Error(`Selected Chinese resource missing: ${from}`);
                index(to, file);
            }
            for (const file of meta.entries) {
                if (/^g00\/.*\.g01$/i.test(file.path)) index(file.path.slice(0, -1) + '0', file);
            }
        }
        await warmStartupResources();
        db = await openDb();
        await new Promise((resolve, reject) => {
            const request = db.transaction('files').objectStore('files').openCursor();
            request.onsuccess = () => {
                const cursor = request.result;
                if (cursor) {
                    overlay.set(cursor.key, new Uint8Array(cursor.value));
                    indexChildren(cursor.key);
                    cursor.continue();
                } else resolve();
            };
            request.onerror = () => reject(request.error);
        });
        // A configured key overrides any previously generated key in IndexedDB.
        if (meta.key_config) {
            overlay.set('key.toml', new TextEncoder().encode(meta.key_config));
            indexChildren('key.toml');
        }
        const { default: init, start_siglus_from_directory } = await import('/galgame/siglus/pkg/game_launcher.js');
        await init();
        canvas.focus();
        await start_siglus_from_directory('siglus-canvas', JSON.stringify(
            Array.from(files, ([path, file]) => ({ path, size: file.size, lastModified: 0 })),
        ));
        if (siglusProbe.renderer.backend === 'software') startSoftwareCanvas();
        if (engineError) throw Error(engineError);
        if (!failed) {
            loading.hidden = true;
            siglusProbe.phase = 'running';
        }
    } catch (reason) {
        fail(reason);
    }
}
canvas.addEventListener('contextmenu', event => event.preventDefault());
canvas.addEventListener('wheel', event => event.preventDefault(), { passive: false });
for (const type of ['keydown', 'keyup']) canvas.addEventListener(type, event => {
    if (['F11', 'F12'].includes(event.code) || ['F11', 'F12'].includes(event.key)
        || ((event.ctrlKey || event.metaKey || event.altKey) && !['Control', 'Alt', 'Meta', 'Shift'].includes(event.key))) {
        event.stopImmediatePropagation();
    }
}, { capture: true });
start();
