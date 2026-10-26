// Probe a temporary canvas that is attached to the document. Some Chromium
// and Edge GPU paths reject a detached canvas even though the real canvas works.
function temporaryCanvas(kind) {
    const canvas = document.createElement('canvas');
    canvas.width = 1;
    canvas.height = 1;
    canvas.dataset.siglusRendererProbe = kind;
    canvas.style.cssText = 'position:fixed;width:1px;height:1px;left:-10px;top:-10px;opacity:0;pointer-events:none';
    document.body.append(canvas);
    return canvas;
}

export async function selectRenderer(gameCanvas = null) {
    let glFailure = '';
    try {
        const probe = gameCanvas || temporaryCanvas('webgl2');
        let gl = null;
        try {
            gl = probe.getContext('webgl2', { alpha: false, antialias: false, preserveDrawingBuffer: false });
        } catch (error) {
            glFailure = String(error?.message || error);
        }
        if (!gl) {
            try { gl = probe.getContext('webgl2'); } catch (error) {
                glFailure ||= String(error?.message || error);
            }
        }
        if (gl) {
            const maxTextureDimension2D = gl.getParameter(gl.MAX_TEXTURE_SIZE);
            gl.getExtension('WEBGL_lose_context')?.loseContext();
            if (!gameCanvas) probe.remove();
            return { backend: 'gl', maxTextureDimension2D };
        }
        if (!gameCanvas) probe.remove();
    } catch (_) {
        // Report both unavailable backends below without claiming a VM failure.
    }
    let gpuFailure = '';
    if (navigator.gpu) {
        try {
            const adapter = await navigator.gpu.requestAdapter({ powerPreference: 'high-performance' });
            if (!adapter) gpuFailure = 'WebGPU API 存在，但 requestAdapter 返回空值';
            const probe = gameCanvas || temporaryCanvas('webgpu');
            if (adapter && probe.getContext('webgpu')) {
                if (!gameCanvas) probe.remove();
                return { backend: 'webgpu', maxTextureDimension2D: adapter.limits.maxTextureDimension2D };
            }
            if (!gameCanvas) probe.remove();
        } catch (_) {
            gpuFailure = 'WebGPU 初始化失败';
        }
    }
    const environment = `User agent: ${navigator.userAgent}\nWebGPU API: ${navigator.gpu ? 'available' : 'unavailable'}\nCross-origin isolated: ${crossOriginIsolated ? 'yes' : 'no'}`;
    const detail = [glFailure && `WebGL2: ${glFailure}`, gpuFailure, environment].filter(Boolean).join('\n');
    return { backend: 'software', maxTextureDimension2D: 0, diagnostics: detail };
}
