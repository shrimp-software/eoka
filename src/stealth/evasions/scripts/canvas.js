
// Canvas noise (gated by `StealthConfig::canvas_spoof`). Readback and export
// use the native getter and apply the same noise once.
const origGetImageData = CanvasRenderingContext2D.prototype.getImageData;
const _eoka_noisePixels = (img) => {
    const data = img.data;
    let s = (__EOKA_NOISE_SEED__ >>> 0) || 1;
    for (let i = 0; i < data.length; i += 4) {
        s = _eoka_lcg(s);
        // Partially transparent RGB is quantized by the canvas backing store.
        // Leave it alone so exporting cannot change the readback noise.
        if (data[i + 3] === 255) data[i] = data[i] ^ (s & 1);
    }
    return img;
};
const _eoka_noiseCanvas = (canvas) => {
    const w = canvas.width | 0, h = canvas.height | 0;
    if (w <= 0 || h <= 0) return canvas;
    try {
        const tmp = document.createElement('canvas');
        tmp.width = w; tmp.height = h;
        const tctx = tmp.getContext('2d');
        tctx.drawImage(canvas, 0, 0);
        const img = origGetImageData.call(tctx, 0, 0, w, h);
        tctx.putImageData(_eoka_noisePixels(img), 0, 0);
        return tmp;
    } catch(e) {
        // Preserve native export failures, including tainted canvas errors.
        return canvas;
    }
};

for (const name of ['toDataURL', 'toBlob']) {
    _eoka_patchMethod(HTMLCanvasElement.prototype, name, (original) => function(...args) {
        // A non-canvas receiver has no size, passes through and throws natively.
        return original.apply(_eoka_noiseCanvas(this), args);
    });
}

_eoka_patchMethod(CanvasRenderingContext2D.prototype, 'getImageData', (original) => function(...args) {
    return _eoka_noisePixels(original.apply(this, args));
});
