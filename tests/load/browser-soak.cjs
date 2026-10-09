// Disposable repeated editor/menu operations with measured browser resource bounds.
const { chromium, expect } = require('../e2e/node_modules/@playwright/test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const origin = process.env.E2E_BASE_URL || 'http://127.0.0.1:18080';
const output = process.env.SOAK_OUTPUT || '/tmp/revaro-browser-soak.json';
(async () => {
  const browser = await chromium.launch({ executablePath: process.env.PLAYWRIGHT_EXECUTABLE_PATH || undefined });
  const context = await browser.newContext();
  const page = await context.newPage();
  let folder;
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  try {
    await page.addInitScript(() => {
      for (const name of ['instantiate', 'instantiateStreaming']) {
        const original = WebAssembly[name].bind(WebAssembly);
        WebAssembly[name] = async (...args) => {
          const result = await original(...args);
          window.__soakMemory = (result.instance || result).exports.memory;
          return result;
        };
      }
    });
    const headers = { origin };
    const login = await context.request.post(`${origin}/api/auth/login`, { headers, data: {
      username: process.env.E2E_USERNAME || 'admin', password: process.env.E2E_PASSWORD || 'revaro-e2e-password',
    } });
    assert.equal(login.status(), 200);
    const created = await context.request.post(`${origin}/api/directories`, { headers, data: {
      parent_id: '00000000-0000-0000-0000-000000000000', name: `soak-${Date.now()}`,
    } });
    assert.equal(created.status(), 201); folder = (await created.json()).id;
    const document = await context.request.post(`${origin}/api/documents`, { headers, data: { parent_id: folder, name: 'soak.md', content: 'saved content' } });
    assert.equal(document.status(), 201);
    await page.goto(`${origin}/f/${folder}`);
    const card = page.locator('.file-card').filter({ hasText: 'soak.md' });
    await card.waitFor();
    const cdp = await context.newCDPSession(page);
    await cdp.send('Performance.enable');
    async function snapshot() {
      for (let i = 0; i < 3; i++) {
        await cdp.send('HeapProfiler.collectGarbage');
        await page.waitForTimeout(100);
      }
      const { metrics } = await cdp.send('Performance.getMetrics');
      const metric = name => metrics.find(value => value.name === name)?.value;
      return { heap_bytes: metric('JSHeapUsedSize'), dom_nodes: metric('Nodes'), documents: metric('Documents'),
        wasm_bytes: await page.evaluate(() => window.__soakMemory.buffer.byteLength) };
    }
    async function rounds(count) {
      for (let i = 0; i < count; i++) {
        const prefix = `${i}\n`;
        const text = prefix + 'x'.repeat(262144 - prefix.length);
        await card.click();
        const editor = page.getByLabel('文档内容', { exact: true });
        await expect(editor).toHaveValue('saved content');
        await editor.evaluate((element, value) => { element.value = value; element.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertFromPaste' })); }, text);
        await page.getByRole('button', { name: '关闭编辑器', exact: true }).click();
        await page.getByRole('button', { name: '放弃修改', exact: true }).click();
        await page.locator('.document-editor').waitFor({ state: 'detached' });
        await page.getByLabel('更多操作', { exact: true }).click();
        await page.getByLabel('上传', { exact: true }).click();
        await page.keyboard.press('Escape');
        await page.keyboard.press('Escape');
      }
    }
    await rounds(10);
    const warm = await snapshot();
    await rounds(75);
    const middle = await snapshot();
    await rounds(75);
    const end = await snapshot();
    const report = { rounds: 160, edited_bytes_per_round: 262144, warm, middle, end, errors };
    fs.writeFileSync(output, `${JSON.stringify(report, null, 2)}\n`);
    console.log(JSON.stringify(report));
    assert.equal(errors.length, 0);
    assert.ok(end.heap_bytes - middle.heap_bytes < 16 * 1024 * 1024, 'JS heap keeps growing');
    assert.ok(end.wasm_bytes - middle.wasm_bytes < 16 * 1024 * 1024, 'WASM memory keeps growing');
    assert.ok(end.dom_nodes - middle.dom_nodes <= 50, 'detached DOM nodes accumulate');
  } catch (error) {
    await page.screenshot({ path: '/tmp/revaro-browser-soak-failure.png' }).catch(() => {});
    console.log(await page.evaluate(() => [...document.querySelectorAll('.document-editor,textarea,.editor-draft-recovery')].map(el => ({ tag: el.tagName, rect: el.getBoundingClientRect().toJSON(), readonly: el.readOnly, text: el.tagName === 'TEXTAREA' ? el.value.slice(0,60) : el.textContent.slice(0,100) }))));
    throw error;
  } finally {
    if (folder) {
      await context.request.delete(`${origin}/api/files/${folder}`, { headers: { origin } });
      await context.request.delete(`${origin}/api/trash/${folder}`, { headers: { origin } });
    }
    await browser.close();
  }
})().catch(error => { console.error(String(error).slice(0, 600)); process.exitCode = 1; });
