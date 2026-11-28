
// WebGL vendor/renderer (gated by `StealthConfig::webgl_spoof`).
for (const name of ['WebGLRenderingContext', 'WebGL2RenderingContext']) {
    if (typeof window[name] === 'undefined') continue;
    _eoka_patchMethod(window[name].prototype, 'getParameter', (original) => function(...args) {
        if (args[0] === 37445) return '__EOKA_WEBGL_VENDOR__';
        if (args[0] === 37446) return '__EOKA_WEBGL_RENDERER__';
        return original.apply(this, args);
    });
}
