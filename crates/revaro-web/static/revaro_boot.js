// HTML paints the loader without waiting for this module or the full CSS.
const app = document.getElementById('app')
const splash = app.firstElementChild
const controller = new AbortController()
document.getElementById('boot-retry').addEventListener('click', () => location.reload())

try {
  // Pass the Response straight to wasm-bindgen: instantiateStreaming consumes
  // the decoded network stream and compiles while bytes are still arriving.
  const runtimeReady = Promise.all([
    import(app.dataset.module),
    fetch(app.dataset.wasm, { signal: controller.signal }),
  ]).then(async ([runtime, response]) => {
    await runtime.default({ module_or_path: response })
    return runtime
  })
  const transportReady = import('./transport-client.js')
    .then(({ initializeTransport }) => initializeTransport())
  const stylesReady = new Promise((resolve, reject) => {
    const sheet = document.getElementById('app-styles')
    sheet.onload = resolve
    sheet.onerror = () => reject(new Error('页面样式加载失败，请刷新重试。'))
    sheet.rel = 'stylesheet'
  })
  const [runtime] = await Promise.all([runtimeReady, transportReady, stylesReady])
  // Install API recovery before mounting. Native file consumers use the
  // worker when available; unavailable workers have a bounded startup fallback.
  runtime.start()
} catch (error) {
  controller.abort()
  console.error('Revaro startup failed:', error)
  if (!document.getElementById('boot-status')) app.replaceChildren(splash)
  document.getElementById('boot-status').textContent = error.message || '无法启动 Revaro，请刷新重试。'
  document.getElementById('boot-spinner').hidden = true
  document.getElementById('boot-retry').hidden = false
}
