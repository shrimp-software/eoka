
// Audio noise (gated by `StealthConfig::audio_spoof`): deterministic, applied
// once per channel buffer.
const _eoka_audioSeen = new WeakSet();
_eoka_patchMethod(AudioBuffer.prototype, 'getChannelData', (original) => function(...args) {
    const d = original.apply(this, args);
    if (!_eoka_audioSeen.has(d)) {
        let s = ((__EOKA_NOISE_SEED__ >>> 0) ^ (args[0] >>> 0)) >>> 0 || 1;
        for (let i = 0; i < d.length; i += 100) {
            s = _eoka_lcg(s);
            d[i] += (s / 4294967296 - 0.5) * 0.0001;
        }
        _eoka_audioSeen.add(d);
    }
    return d;
});
