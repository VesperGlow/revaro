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
const cacheLimit = 64 * 1024 * 1024;
let cacheBytes = 0;
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
for (const type of ['pointerdown', 'keydown']) document.addEventListener(type, resumeAudio, { capture: true });
const reads = [];
window.siglusProbe = { reads, overlayReads: [], saveWrites: [], phase: 'loading' };

function fail(reason) {
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
    for (const byte of bytes) crc = (crc >>> 8) ^ crcTable[(crc ^ byte) & 255];
    return (crc ^ -1) >>> 0;
}
function record(list, value) {
    if (list.length === 128) list.shift();
    list.push(value);
}
function read(path) {
    const key = norm(path);
    if (overlay.has(key)) {
        record(siglusProbe.overlayReads, key);
        return overlay.get(key);
    }
    const file = files.get(key);
    if (!file) throw Error(`Missing game resource: ${path}`);
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
    const packed = Uint8Array.from(text, character => character.charCodeAt(0) & 255);
    const data = file.method === 0 ? packed : fflate.inflateSync(packed, { out: new Uint8Array(file.size) });
    if (data.length !== file.size || crc32(data) !== file.crc) throw Error(`Resource size/CRC mismatch: ${path}`);
    if (data.length <= cacheLimit) {
        while (cacheBytes + data.length > cacheLimit && cache.size) {
            const oldest = cache.keys().next().value;
            cacheBytes -= cache.get(oldest).length;
            cache.delete(oldest);
        }
        cache.set(file.path, data);
        cacheBytes += data.length;
    }
    record(reads, { path, source: file.path, bytes: packed.length, status: xhr.status });
    return data;
}
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
