import { initializeTransport } from './transport-client.js';
initializeTransport().then(() => {
  // Ordinary navigation passes through the newly claimed worker, preserving
  // the server's attachment/inline disposition and safe filename.
  document.getElementById('status').textContent = '文件将自动打开或下载。';
  const open = () => {
    const link = document.createElement('a');
    link.href = location.href;
    document.body.appendChild(link); link.click(); link.remove();
  };
  if (document.readyState === 'complete') open();
  else window.addEventListener('load', open, { once: true });
}).catch(error => {
  document.getElementById('status').textContent = error.message || '无法加载文件，请刷新重试。';
});
