// Stock Chrome on Linux does not expose `navigator.bluetooth`, so adding it to
// a Linux identity is a platform tell visible to any reflection sweep. Keep the
// property absent there. On platforms whose Chrome ships Web Bluetooth, only
// fill a genuinely missing interface, and only in secure contexts: the
// interface is [SecureContext], so stock Chrome omits it on http pages. Methods use shorthand syntax and the
// getter is an arrow so, like native interface members, none of them has a
// `prototype` or can be called with `new`; `_eoka_mark` covers `toString()`.
if (!/Linux/i.test('__EOKA_PLATFORM__') && window.isSecureContext && !navigator.bluetooth) {
    const _eoka_bluetooth = {
        getAvailability() { return Promise.resolve(false); },
        requestDevice() { return Promise.reject(new DOMException('User cancelled', 'NotFoundError')); },
        getDevices() { return Promise.resolve([]); },
        addEventListener() {},
        removeEventListener() {},
        dispatchEvent() { return true; }
    };
    for (const method of Object.values(_eoka_bluetooth)) _eoka_mark(method);

    Object.defineProperty(Navigator.prototype, 'bluetooth', {
        get: _eoka_mark(() => _eoka_bluetooth, 'get bluetooth'),
        configurable: true,
        enumerable: true
    });
}
