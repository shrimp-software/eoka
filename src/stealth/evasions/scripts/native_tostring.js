
const _eoka_spoofed = new WeakMap();
const _eoka_origToString = Function.prototype.toString;
const _eoka_mark = (fn, name) => {
    if (typeof fn === 'function') {
        _eoka_spoofed.set(fn, name || fn.name || '');
    }
    return fn;
};
const _eoka_toStringProxy = new Proxy(_eoka_origToString, {
    apply(target, thisArg, args) {
        if (typeof thisArg === 'function' && _eoka_spoofed.has(thisArg)) {
            const name = _eoka_spoofed.get(thisArg);
            return name ? 'function ' + name + '() { [native code] }'
                        : 'function () { [native code] }';
        }
        if (thisArg === _eoka_toStringProxy) {
            return 'function toString() { [native code] }';
        }
        return Reflect.apply(target, thisArg, args);
    }
});
Function.prototype.toString = _eoka_toStringProxy;
_eoka_mark(Function.prototype.toString, 'toString');

// Deterministic per-session PRNG (LCG), seeded from Rust.
const _eoka_lcg = (s) => (Math.imul(s >>> 0, 1664525) + 1013904223) >>> 0;
let _eoka_seed = (__EOKA_NOISE_SEED__ >>> 0) || 1;
const _eoka_rand = () => {
    _eoka_seed = _eoka_lcg(_eoka_seed);
    return _eoka_seed / 4294967296;
};
