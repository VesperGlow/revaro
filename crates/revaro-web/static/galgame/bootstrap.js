// Share metadata selects resources; query strings cannot override games or saves.
(async () => {
    'use strict';
    const match = /^\/g\/([A-Za-z0-9_-]{1,64})\/?$/.exec(location.pathname);
    const loading = document.getElementById('loading');
    const status = document.getElementById('status');
    const overlay = document.getElementById('error-overlay');
    const message = document.getElementById('error-message');
    document.getElementById('error-update-btn').addEventListener('click', () => location.reload());
    function fail(error) {
        loading.classList.add('hidden');
        document.querySelector('#error-box h2').textContent = '游戏加载失败';
        message.textContent = error?.message || String(error);
        overlay.classList.add('visible');
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
    // Report actual runtime failures, without a separate capability check or gate.
    window.addEventListener('unhandledrejection', event => fail(event.reason));
    window.addEventListener('error', event => {
        if (event.filename && new URL(event.filename, location.href).pathname.startsWith('/galgame/')) {
            fail(event.error || event.message);
        }
    });
    const canvas = document.getElementById('canvas');
    canvas.addEventListener('webglcontextcreationerror', () => fail(new Error('游戏画布创建失败，请检查浏览器的图形加速和显卡驱动。')));
    canvas.addEventListener('webglcontextlost', () => fail(new Error('游戏图形上下文已丢失，请重新加载页面。浏览器存档会保留。')));
    try {
        if (!match) throw new Error('游戏分享链接无效');
        status.textContent = '正在加载游戏…';
        const response = await fetch(`/g/${match[1]}/manifest`, {cache: 'no-store'});
        if (!response.ok) {
            let detail;
            try { detail = (await response.json()).error?.message; } catch (_) { /* proxy response */ }
            throw new Error(detail || `游戏分享不可用 (${response.status})`);
        }
        const game = await response.json();
        window.REVARO_GAME = Object.freeze(game);
        document.title = `${game.name} · Revaro`;
        for (const asset of ['jszip.min.js', 'vlfs.js', 'early.js', 'uniforms.js', 'player.js', 'input.js', 'index.js']) {
            await script(`/galgame/kirikiri2/${asset}`);
        }
    } catch (error) { fail(error); }
})();
