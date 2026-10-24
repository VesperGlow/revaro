// Only server-checked manifests select resources. Query strings cannot override games or saves.
(async () => {
    'use strict';
    const gate = document.getElementById('revaro-gate');
    const message = document.getElementById('revaro-message');
    const password = document.getElementById('revaro-password');
    const button = document.getElementById('revaro-unlock');
    const match = /^\/g\/([A-Za-z0-9_-]{1,64})\/?$/.exec(location.pathname);
    function fail(error) { message.textContent = error.message || String(error); gate.hidden = false; }
    async function checked(response) {
        if (response.ok) return response;
        let detail;
        try { detail = (await response.json()).error?.message; } catch (_) { /* non-JSON proxy error */ }
        throw new Error(detail || `请求失败 (${response.status})`);
    }
    function script(src) {
        return new Promise((resolve, reject) => {
            const el = document.createElement('script');
            el.src = src;
            el.onload = resolve;
            el.onerror = () => reject(new Error(`播放器资源加载失败：${src}`));
            document.body.appendChild(el);
        });
    }
    let starting = false;
    async function start() {
        const response = await fetch(`/g/${match[1]}/manifest`, {cache: 'no-store'});
        if (response.status === 401) {
            message.textContent = '此游戏需要分享密码';
            password.hidden = button.hidden = false;
            password.focus();
            return;
        }
        const game = await (await checked(response)).json();
        if (!window.isSecureContext || !window.crossOriginIsolated || typeof SharedArrayBuffer === 'undefined' ||
            typeof WebAssembly.Suspending !== 'function' || typeof WebAssembly.promising !== 'function') {
            throw new Error('播放器需要支持 JSPI 的新版 Chrome/Edge、HTTPS（本地可用 localhost）及跨源隔离。');
        }
        if (starting) return;
        starting = true;
        window.REVARO_GAME = Object.freeze(game);
        document.title = `${game.name} · Revaro`;
        for (const asset of ['jszip.min.js', 'vlfs.js', 'early.js', 'uniforms.js', 'player.js', 'input.js', 'index.js']) {
            await script(`/galgame/kirikiri2/${asset}`);
        }
        gate.hidden = true;
    }
    gate.addEventListener('submit', async event => {
        event.preventDefault();
        button.disabled = true;
        try {
            await checked(await fetch(`/g/${match[1]}/unlock`, {
                method: 'POST', headers: {'Content-Type': 'application/json'},
                body: JSON.stringify({password: password.value}), cache: 'no-store'
            }));
            password.value = '';
            await start();
        } catch (error) { fail(error); }
        finally { button.disabled = false; }
    });
    try {
        if (!match) throw new Error('请通过有效的 /g/ 分享链接打开游戏');
        await start();
    } catch (error) { fail(error); }
})();
