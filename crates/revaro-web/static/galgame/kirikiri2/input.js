/* Web platform adapter for the pinned Kirikiroid2 runtime. See SOURCE.md.
 * Native window state owns modal routing. Story/menu/log behavior stays in the game.
 * No game scripts, archive entries, save data or scene-specific coordinates are used.
 */
(() => {
    'use strict';
    const canvas = document.getElementById('canvas');
    const container = document.getElementById('container');
    if (!canvas || !container) return;
    canvas.tabIndex = 0;
    canvas.setAttribute('aria-label', 'Galgame 游戏画面');
    const ABI = Object.freeze({current: 5858060, vtable: 5101940, width: 860, height: 864,
        viewWidth: 652, viewHeight: 656, modal: 6503, previous: 868});
    const generated = new WeakSet();
    const keys = new Map();
    const buttons = new Map();
    const commands = [];
    let state = null, modalState = null, nativeEvent = 0, scrollNative, queuedPaint = false;
    let wheelPixels = 0, wheelDirection = 0, disposed = false;
    let lastTouch = -Infinity;
    const counters = {wheel: 0, discarded: 0, cancel: 0, touches: 0, keyDown: 0, keyUp: 0};
    const backdrop = document.createElement('canvas');
    const savedFrame = document.createElement('canvas');
    const modal = document.createElement('div');
    modal.id = 'revaro-modal';
    modal.hidden = true;
    modal.setAttribute('role', 'dialog');
    modal.setAttribute('aria-modal', 'true');
    modal.setAttribute('aria-label', '游戏确认窗口');
    const panel = document.createElement('canvas');
    panel.id = 'revaro-modal-panel';
    panel.setAttribute('aria-label', '游戏原生确认框');
    modal.append(backdrop, panel);
    container.append(modal);

    function nativeState() {
        if (typeof HEAPU32 === 'undefined' || typeof wasmExports === 'undefined' ||
            !wasmExports.revaro_mouse_scroll || !wasmExports.revaro_key_release) return null;
        const id = HEAPU32[ABI.current >>> 2];
        if (!id || id + 6512 > HEAPU8.length || HEAPU32[id >>> 2] !== ABI.vtable) return null;
        const width = HEAP32[(id + ABI.width) >>> 2], height = HEAP32[(id + ABI.height) >>> 2];
        const viewWidth = HEAPF32[(id + ABI.viewWidth) >>> 2], viewHeight = HEAPF32[(id + ABI.viewHeight) >>> 2];
        if (!(width > 0 && height > 0 && width <= 32768 && height <= 32768 && viewWidth > 0 && viewHeight > 0)) return null;
        let overlay = false;
        const scene = HEAPU32[5858088 >>> 2];
        if (scene && scene + 616 < HEAPU8.length) {
            const ui = HEAPU32[(scene + 612) >>> 2];
            if (ui && ui + 4 < HEAPU8.length) {
                const vtable = HEAPU32[ui >>> 2];
                const getter = HEAPU32[(vtable + 288) >>> 2];
                const children = getWasmTableEntry(getter)(ui);
                overlay = HEAPU32[children >>> 2] !== HEAPU32[(children + 4) >>> 2];
            }
        }
        return {id, width, height, viewWidth, viewHeight, overlay, modal: !!HEAPU8[id + ABI.modal]};
    }
    function fit(width, height, contentWidth, contentHeight) {
        const scale = Math.min(width / contentWidth, height / contentHeight);
        return {x: (width - contentWidth * scale) / 2, y: (height - contentHeight * scale) / 2,
            width: contentWidth * scale, height: contentHeight * scale};
    }
    function crop(s) {
        // GLView's design viewport, then the active window's native aspect fit.
        const view = fit(canvas.width, canvas.height, s.viewWidth, s.viewHeight);
        const window = fit(view.width, view.height, s.width, s.height);
        return {x: view.x + window.x, y: view.y + window.y, width: window.width, height: window.height};
    }
    function copyFrame(target) {
        if (target.width !== canvas.width) target.width = canvas.width;
        if (target.height !== canvas.height) target.height = canvas.height;
        target.getContext('2d').drawImage(canvas, 0, 0);
    }
    function layout(s) {
        const rect = container.getBoundingClientRect();
        const scale = Math.min(rect.width * 0.92 / s.width, rect.height * 0.76 / s.height, 1.5);
        panel.style.width = `${s.width * scale}px`;
        panel.style.height = `${s.height * scale}px`;
    }
    function updateState() {
        const next = nativeState();
        state = next;
        if (!next?.modal) {
            modal.hidden = true;
            canvas.classList.remove('revaro-modal-source');
            modalState = null;
        } else {
            if (modalState?.id !== next.id) {
                // The last complete parent frame remains untouched while a modal draws.
                backdrop.width = savedFrame.width || canvas.width;
                backdrop.height = savedFrame.height || canvas.height;
                backdrop.getContext('2d').drawImage(savedFrame, 0, 0);
            }
            modalState = next;
            layout(next);
            modal.hidden = false;
            canvas.classList.add('revaro-modal-source');
        }
        return next;
    }
    function painted(gl) {
        if (disposed || gl.canvas !== canvas || gl.getParameter(gl.FRAMEBUFFER_BINDING) !== null || queuedPaint) return;
        queuedPaint = true;
        queueMicrotask(() => {
            queuedPaint = false;
            const current = updateState();
            if (!current) return;
            if (current.modal) {
                const source = crop(current);
                if (panel.width !== current.width) panel.width = current.width;
                if (panel.height !== current.height) panel.height = current.height;
                panel.getContext('2d').drawImage(canvas, source.x, source.y, source.width, source.height,
                    0, 0, panel.width, panel.height);
            } else copyFrame(savedFrame);
        });
    }
    // Copy only complete draws on the engine's default framebuffer. No global viewport scaling.
    for (const prototype of new Set([globalThis.WebGLRenderingContext?.prototype, globalThis.WebGL2RenderingContext?.prototype])) {
        if (!prototype) continue;
        for (const name of ['drawElements', 'drawArrays']) {
            const original = prototype[name];
            prototype[name] = function (...args) {
                const result = Reflect.apply(original, this, args);
                painted(this);
                return result;
            };
        }
    }
    function focus() { if (!disposed) canvas.focus({preventScroll: true}); }
    function mouse(type, x, y, button = 0, held = 0) {
        // SDL button callbacks use its last cursor position. Touch has no preceding mousemove.
        if (type === 'mousedown') mouse('mousemove', x, y, button, 0);
        const event = new MouseEvent(type, {clientX: x, clientY: y, button, buttons: held,
            bubbles: true, cancelable: true, view: window});
        generated.add(event);
        if (type === 'mousedown') buttons.set(button, nativeState()?.id);
        if (type === 'mouseup') buttons.delete(button);
        canvas.dispatchEvent(event);
    }
    function key(type, code) {
        const names = {Escape: ['Escape', 27], Enter: ['Enter', 13], Space: [' ', 32],
            ArrowUp: ['ArrowUp', 38], ArrowDown: ['ArrowDown', 40], ArrowLeft: ['ArrowLeft', 37], ArrowRight: ['ArrowRight', 39]};
        const [value, number] = names[code] || [code, 0];
        const event = new KeyboardEvent(type, {key: value, code, keyCode: number, which: number, bubbles: true, cancelable: true});
        generated.add(event);
        canvas.dispatchEvent(event);
    }
    function press(code) { focus(); key('keydown', code); key('keyup', code); }
    function right(x, y) {
        const current = nativeState();
        if (!current) return;
        focus();
        if (current.modal || current.overlay) { counters.cancel++; press('Escape'); }
        else { mouse('mousedown', x, y, 2, 2); mouse('mouseup', x, y, 2, 0); }
    }
    function hit(event) {
        const target = event.target;
        return target === canvas || modal.contains(target);
    }
    function panelPoint(x, y) {
        const current = nativeState();
        if (!current?.modal || current.id !== modalState?.id) return null;
        const rect = panel.getBoundingClientRect();
        if (x < rect.left || x > rect.right || y < rect.top || y > rect.bottom) return null;
        const source = crop(current), destination = canvas.getBoundingClientRect();
        return {x: destination.left + (source.x + (x - rect.left) / rect.width * source.width) / canvas.width * destination.width,
            y: destination.top + (source.y + (y - rect.top) / rect.height * source.height) / canvas.height * destination.height};
    }
    for (const type of ['mousedown', 'mouseup', 'mousemove']) {
        container.addEventListener(type, event => {
            if (generated.has(event) || !hit(event)) return;
            const current = nativeState();
            if (!current) return;
            if (performance.now() - lastTouch < 700) { event.preventDefault(); event.stopImmediatePropagation(); return; }
            if (type === 'mousedown') focus();
            if (event.button === 2 && (current.modal || current.overlay)) {
                event.preventDefault(); event.stopImmediatePropagation();
                if (type === 'mouseup') right(event.clientX, event.clientY);
                return;
            }
            if (modal.contains(event.target)) {
                event.preventDefault(); event.stopImmediatePropagation();
                const point = panelPoint(event.clientX, event.clientY);
                if (type === 'mouseup' && !buttons.has(event.button)) return;
                if (point) mouse(type, point.x, point.y, event.button, event.buttons);
                else if (type === 'mouseup' && buttons.has(event.button)) mouse('mouseup', -10000, -10000, event.button, 0);
                return;
            }
            if (type === 'mousedown') buttons.set(event.button, current.id);
            if (type === 'mouseup') {
                const owner = buttons.get(event.button);
                buttons.delete(event.button);
                if (owner && owner !== current.id) {
                    event.preventDefault(); event.stopImmediatePropagation();
                    mouse('mouseup', -10000, -10000, event.button, 0);
                }
            }
        }, true);
    }
    container.addEventListener('contextmenu', event => { if (hit(event)) event.preventDefault(); });
    modal.addEventListener('click', event => { event.preventDefault(); event.stopImmediatePropagation(); }, true);

    function wheel(delta, x, y, mode = 0) {
        const current = nativeState();
        if (!current || current.modal || current.overlay || ![delta, x, y].every(Number.isFinite)) return;
        const direction = Math.sign(delta);
        if (direction !== wheelDirection) wheelPixels = 0;
        wheelDirection = direction;
        wheelPixels += Math.abs(delta) * (mode === 1 ? 16 : mode === 2 ? canvas.clientHeight : 1);
        const steps = Math.min(6, Math.floor(wheelPixels / 40));
        wheelPixels %= 40;
        for (let i = 0; i < steps && commands.length < 12; i++) commands.push({owner: current.id, direction, x, y});
    }
    container.addEventListener('wheel', event => {
        if (!hit(event) || event.ctrlKey || event.metaKey) return;
        event.preventDefault(); event.stopImmediatePropagation();
        wheel(event.deltaY, event.clientX, event.clientY, event.deltaMode);
    }, {capture: true, passive: false});
    async function tick(original) {
        if (disposed) return original();
        updateState();
        for (let i = 0; i < 4 && commands.length; i++) {
            const command = commands.shift(), current = nativeState();
            if (!current || current.modal || current.overlay || command.owner !== current.id) { counters.discarded++; continue; }
            nativeEvent ||= _malloc(64);
            HEAPU8.fill(0, nativeEvent, nativeEvent + 64);
            const rect = canvas.getBoundingClientRect();
            const viewport = fit(rect.width, rect.height, current.viewWidth, current.viewHeight);
            HEAPF32[(nativeEvent + 44) >>> 2] = (command.x - rect.left - viewport.x) / viewport.width * current.viewWidth;
            HEAPF32[(nativeEvent + 48) >>> 2] = (1 - (command.y - rect.top - viewport.y) / viewport.height) * current.viewHeight;
            HEAPF32[(nativeEvent + 56) >>> 2] = command.direction;
            scrollNative ||= WebAssembly.promising(wasmExports.revaro_mouse_scroll);
            await scrollNative(current.id, nativeEvent);
            counters.wheel++;
        }
        return original();
    }
    const mapped = new Set(['Escape', 'Enter', 'Space', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight']);
    function ownsKeys(event) {
        return event.target === canvas || modal.contains(event.target) || (event.target === document.body && document.activeElement === canvas);
    }
    for (const type of ['keydown', 'keyup']) window.addEventListener(type, event => {
        if (generated.has(event) || !nativeState()) return;
        // Browser shortcuts retain their default action, and never reach the game.
        if (event.metaKey || event.altKey || (event.ctrlKey && event.code !== 'ControlLeft' && event.code !== 'ControlRight') || event.code === 'F11') {
            event.stopImmediatePropagation(); return;
        }
        if (!ownsKeys(event)) { event.stopImmediatePropagation(); return; }
        if (mapped.has(event.code)) event.preventDefault();
        if (type === 'keydown') {
            if (keys.has(event.code) && ['Escape', 'Enter', 'Space'].includes(event.code)) { event.stopImmediatePropagation(); return; }
            keys.set(event.code, {key: event.key, keyCode: event.keyCode});
            counters.keyDown++;
        } else { keys.delete(event.code); counters.keyUp++; }
    }, true);
    function release() {
        for (const [code, value] of keys) {
            const event = new KeyboardEvent('keyup', {...value, code, which: value.keyCode, bubbles: true, cancelable: true});
            generated.add(event); canvas.dispatchEvent(event);
            // SDL may defer its queued keyup while a frame is suspended/hidden.
            // The native release path clears pressed state once; SDL's later keyup is ignored
            // by that same path when it observes the already-released bit.
            const vk = value.keyCode;
            if (vk > 0) {
                for (let index = 0; index < 165; index++) {
                    if (HEAPU8[1761720 + index] === vk) {
                        wasmExports.revaro_key_release(0, index + 1, 0);
                        break;
                    }
                }
            }
        }
        keys.clear();
        for (const button of buttons.keys()) mouse('mouseup', -10000, -10000, button, 0);
        buttons.clear(); commands.length = 0; wheelPixels = 0;
    }
    window.addEventListener('blur', release);
    document.addEventListener('visibilitychange', () => { if (document.hidden) release(); });

    // Own touch once: single tap -> left, long/two-finger tap -> native right,
    // vertical swipe -> the same native wheel handler. No synthetic click duplication.
    let gesture = null;
    for (const type of ['touchstart', 'touchmove', 'touchend', 'touchcancel']) container.addEventListener(type, event => {
        const current = nativeState();
        if (!hit(event) || !current) return;
        // Engine-owned Cocos forms keep their native touch scrolling and hit testing.
        if (current.overlay) { lastTouch = performance.now(); return; }
        event.preventDefault(); event.stopImmediatePropagation(); lastTouch = performance.now();
        const contact = event.touches[0] || event.changedTouches[0];
        if (type === 'touchstart') {
            focus();
            if (!gesture) gesture = {x: contact.clientX, y: contact.clientY, lastY: contact.clientY,
                time: performance.now(), moved: false, multi: false, owner: nativeState().id};
            gesture.multi ||= event.touches.length > 1;
        } else if (type === 'touchmove' && gesture) {
            const distance = Math.hypot(contact.clientX - gesture.x, contact.clientY - gesture.y);
            if (distance > 14) gesture.moved = true;
            if (gesture.moved && !gesture.multi && Math.abs(contact.clientY - gesture.y) > Math.abs(contact.clientX - gesture.x)) {
                // Finger down reveals older dialogue, matching a touch-scrolled log.
                wheel(gesture.lastY - contact.clientY, contact.clientX, contact.clientY);
            }
            gesture.lastY = contact.clientY;
        } else if (type === 'touchcancel') { gesture = null; release(); }
        else if (type === 'touchend' && !event.touches.length && gesture) {
            const completed = gesture; gesture = null;
            if (!completed.moved && completed.owner === nativeState()?.id) {
                counters.touches++;
                if (completed.multi || performance.now() - completed.time > 500) right(completed.x, completed.y);
                else {
                    const point = modalState ? panelPoint(completed.x, completed.y) : completed;
                    if (point) { mouse('mousedown', point.x, point.y, 0, 1); mouse('mouseup', point.x, point.y, 0, 0); }
                }
            }
        }
    }, {capture: true, passive: false});
    const touchBar = document.createElement('div');
    touchBar.id = 'revaro-touch-controls';
    touchBar.setAttribute('aria-label', '游戏触控快捷键');
    for (const [label, action] of [['返回', () => { const r = canvas.getBoundingClientRect(); right(r.left + r.width / 2, r.top + r.height / 2); }], ['日志', () => {
        const r = canvas.getBoundingClientRect(); wheel(-40, r.left + r.width / 2, r.top + r.height / 2);
    }], ['确认', () => press('Enter')]]) {
        const button = document.createElement('button'); button.type = 'button'; button.textContent = label;
        button.addEventListener('click', event => { event.preventDefault(); event.stopPropagation(); action(); });
        touchBar.append(button);
    }
    container.append(touchBar);
    new ResizeObserver(() => { if (modalState) layout(modalState); }).observe(container);
    function observe() { if (!disposed) { updateState(); requestAnimationFrame(observe); } }
    requestAnimationFrame(observe);
    window.addEventListener('pagehide', () => { release(); disposed = true; });
    window.addEventListener('pageshow', event => { if (event.persisted) { disposed = false; requestAnimationFrame(observe); } });
    window.RevaroPlayer = Object.freeze({tick, focus, snapshot: () => ({...state, queued: commands.length, heldKeys: keys.size, heldButtons: buttons.size, ...counters})});
})();
