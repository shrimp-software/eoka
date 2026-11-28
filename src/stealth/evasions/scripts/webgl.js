
// WebGL vendor/renderer (gated by `StealthConfig::webgl_spoof`).
for (const name of ['WebGLRenderingContext', 'WebGL2RenderingContext']) {
    if (typeof window[name] === 'undefined') continue;
    _eoka_patchMethod(window[name].prototype, 'getParameter', (original) => function(...args) {
        // The native call keeps receiver checks and GL error state.
        const value = original.apply(this, args);
        if (args[0] === 37445) return '__EOKA_WEBGL_VENDOR__';
        if (args[0] === 37446) return '__EOKA_WEBGL_RENDERER__';
        return value;
    });
}
