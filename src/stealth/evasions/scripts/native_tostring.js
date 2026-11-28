
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

// Shared patch helpers. Every override replaces the property where the browser
// defines it (normally the interface prototype, never the instance), copies the
// native descriptor flags and keeps the native receiver checks by calling the
// original with `this`. Replacement methods use method-shorthand syntax, so
// like native members they have no `prototype` and cannot be constructed;
// their `name`/`length` match the original and `toString()` reports native
// code. `make(original)` returns the implementation.
const _eoka_patchMethod = (target, name, make) => {
    const desc = Object.getOwnPropertyDescriptor(target, name);
    if (!desc || typeof desc.value !== 'function') return false;
    const impl = make(desc.value);
    const fn = { [name](...args) { return impl.apply(this, args); } }[name];
    Object.defineProperty(fn, 'length', { value: desc.value.length, configurable: true });
    _eoka_mark(fn, name);
    Object.defineProperty(target, name, { ...desc, value: fn });
    return true;
};
const _eoka_patchGetter = (target, name, make) => {
    const desc = Object.getOwnPropertyDescriptor(target, name);
    const original = desc && desc.get;
    const impl = make(original);
    // Run the native getter first so a foreign receiver still throws natively.
    const get = Object.getOwnPropertyDescriptor({ get [name]() {
        if (original) original.call(this);
        return impl.call(this);
    } }, name).get;
    _eoka_mark(get, 'get ' + name);
    Object.defineProperty(target, name, {
        get,
        set: desc ? desc.set : undefined,
        enumerable: desc ? desc.enumerable : true,
        configurable: desc ? desc.configurable : true
    });
};

// An object of a native interface carrying read-only enumerable `fields`.
const _eoka_instance = (proto, fields) => {
    const object = Object.create(proto);
    for (const [key, value] of Object.entries(fields)) {
        Object.defineProperty(object, key, { value, enumerable: true });
    }
    return object;
};

// One step of the deterministic noise PRNG (LCG); streams are seeded from Rust.
const _eoka_lcg = (s) => (Math.imul(s >>> 0, 1664525) + 1013904223) >>> 0;
