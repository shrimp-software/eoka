
const _eoka_spoofWebGL = (proto) => {
    const orig = proto.getParameter;
    proto.getParameter = _eoka_mark(function getParameter(p) {
        if (p === 37445) return '__EOKA_WEBGL_VENDOR__';
        if (p === 37446) return '__EOKA_WEBGL_RENDERER__';
        return orig.call(this, p);
    }, 'getParameter');
};
_eoka_spoofWebGL(WebGLRenderingContext.prototype);
if (typeof WebGL2RenderingContext !== 'undefined') _eoka_spoofWebGL(WebGL2RenderingContext.prototype);

// Both readback and export use the native getter and apply noise once.
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

const origToDataURL = HTMLCanvasElement.prototype.toDataURL;
HTMLCanvasElement.prototype.toDataURL = _eoka_mark(function toDataURL(...args) {
    return origToDataURL.apply(_eoka_noiseCanvas(this), args);
}, 'toDataURL');

const origToBlob = HTMLCanvasElement.prototype.toBlob;
if (origToBlob) {
    HTMLCanvasElement.prototype.toBlob = _eoka_mark(function toBlob(...args) {
        return origToBlob.apply(_eoka_noiseCanvas(this), args);
    }, 'toBlob');
}

CanvasRenderingContext2D.prototype.getImageData = _eoka_mark(function getImageData(...args) {
    return _eoka_noisePixels(origGetImageData.apply(this, args));
}, 'getImageData');

// Audio noise — deterministic, applied once per channel buffer.
const origGetChannelData = AudioBuffer.prototype.getChannelData;
const _eoka_audioSeen = new WeakSet();
AudioBuffer.prototype.getChannelData = _eoka_mark(function getChannelData(ch) {
    const d = origGetChannelData.call(this, ch);
    if (!_eoka_audioSeen.has(d)) {
        let s = ((__EOKA_NOISE_SEED__ >>> 0) ^ (ch >>> 0)) >>> 0 || 1;
        for (let i = 0; i < d.length; i += 100) {
            s = _eoka_lcg(s);
            d[i] += (s / 4294967296 - 0.5) * 0.0001;
        }
        _eoka_audioSeen.add(d);
    }
    return d;
}, 'getChannelData');
