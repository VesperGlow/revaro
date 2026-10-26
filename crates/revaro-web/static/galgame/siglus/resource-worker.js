// Bounded startup resource reads; the synchronous VM consumes verified cache hits.
importScripts('fflate.js');
const crcTable = Uint32Array.from({ length: 256 }, (_, value) => {
    for (let i = 0; i < 8; i++) value = (value >>> 1) ^ ((value & 1) ? 0xedb88320 : 0);
    return value >>> 0;
});
self.onmessage = async ({ data: task }) => {
    const { id, url, offset, packed, size, method, crc } = task;
    try {
        if (![offset, packed, size].every(Number.isSafeInteger) || offset < 0 || packed < 1
            || size < 1 || size > 32 * 1024 * 1024 || packed > 32 * 1024 * 1024 || ![0, 8].includes(method)) {
            throw Error('Invalid prefetch resource');
        }
        const response = await fetch(url, { headers: { Range: `bytes=${offset}-${offset + packed - 1}` } });
        if (response.status !== 206) throw Error(`Prefetch Range failed (${response.status})`);
        const bytes = new Uint8Array(await response.arrayBuffer());
        if (bytes.length !== packed) throw Error('Truncated prefetch resource');
        const result = method === 0 ? bytes : fflate.inflateSync(bytes, { out: new Uint8Array(size) });
        let checksum = -1;
        for (let i = 0; i < result.length; i++) checksum = (checksum >>> 8) ^ crcTable[(checksum ^ result[i]) & 255];
        if (result.length !== size || ((checksum ^ -1) >>> 0) !== crc) throw Error('Prefetch size/CRC mismatch');
        self.postMessage({ id, bytes: result }, [result.buffer]);
    } catch (error) { self.postMessage({ id, error: error.message }); }
};
