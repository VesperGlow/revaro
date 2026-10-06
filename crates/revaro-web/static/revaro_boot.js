// Initialize interception before mounting any native file consumer.
import init from './revaro_web.js'
import { initializeTransport } from './transport-client.js'

try {
  await initializeTransport()
  await init()
} catch (error) {
  document.getElementById('app').textContent = error.message || '无法初始化文件传输，请刷新重试。'
}
