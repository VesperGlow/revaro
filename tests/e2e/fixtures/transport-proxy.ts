import { createServer, request as httpRequest } from 'node:http'
import { pipeline, Transform } from 'node:stream'

export type TransferSample = {
  path: string, range: string, ifNoneMatch: string, status: number, start: number, bytes: number,
  began: number, headersAt: number, firstByteAt: number, finishedAt: number, cancelled: boolean,
}

// Measure bytes actually forwarded, including cancelled open-ended ranges.
// Content-Length alone measures the advertised representation, not network use.
export async function transportProxy(baseURL: string, options: { bytesPerSecond?: number, latencyMs?: number, disconnectAtBytes?: number } = {}) {
  const upstream = new URL(baseURL), tracked = new Set<string>(), samples: TransferSample[] = []
  let faults = 0
  const server = createServer((req, res) => {
    const began = Date.now(), path = new URL(req.url!, upstream).pathname
    const headers = { ...req.headers, host: upstream.host }
    if (headers.origin) headers.origin = upstream.origin
    const target = httpRequest(new URL(req.url!, upstream), { method: req.method, headers }, response => {
      res.writeHead(response.statusCode!, response.headers)
      if (!tracked.has(path) || req.method !== 'GET') {
        pipeline(response, res, () => {})
        return
      }
      const sample: TransferSample = {
        path, range: String(req.headers.range || ''), ifNoneMatch: String(req.headers['if-none-match'] || ''), status: response.statusCode!,
        start: Number(String(response.headers['content-range'] || '').match(/^bytes (\d+)-/)?.[1] || 0),
        bytes: 0, began, headersAt: Date.now(), firstByteAt: 0, finishedAt: 0, cancelled: false,
      }
      samples.push(sample)
      let timer: ReturnType<typeof setTimeout>
      const meter = new Transform({
        transform(chunk, _, callback) {
          const delay = (!sample.firstByteAt ? options.latencyMs || 0 : 0)
            + (options.bytesPerSecond ? chunk.length * 1000 / options.bytesPerSecond : 0)
          timer = setTimeout(() => {
            sample.firstByteAt ||= Date.now()
            sample.bytes += chunk.length
            callback(null, chunk)
            if (options.disconnectAtBytes && !faults && sample.bytes >= options.disconnectAtBytes) {
              faults++
              setImmediate(() => res.destroy())
            }
          }, delay)
        },
        destroy(error, callback) { clearTimeout(timer); callback(error) },
      })
      pipeline(response, meter, res, () => {
        sample.finishedAt = Date.now()
        sample.cancelled = !res.writableFinished
      })
    })
    target.on('error', () => res.destroy())
    res.on('close', () => target.destroy())
    req.pipe(target)
  })
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve))
  const origin = `http://127.0.0.1:${(server.address() as { port: number }).port}`
  return {
    origin, tracked, samples, get faults() { return faults },
    close: async () => {
      server.closeAllConnections()
      await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()))
    },
  }
}
