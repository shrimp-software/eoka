// Stock Chrome on Linux does not expose `navigator.bluetooth`, so adding it to
// a Linux identity is a platform tell visible to any reflection sweep. Keep the
// property absent there. On platforms whose Chrome ships Web Bluetooth, only
// fill a genuinely missing interface, and mark every method native so a
// `toString()` or descriptor probe sees a normal interface object.
if (!/Linux/i.test('__EOKA_PLATFORM__') && !navigator.bluetooth) {
    const _eoka_bluetooth = {
        getAvailability: _eoka_mark(function getAvailability() {
            return Promise.resolve(false);
        }, 'getAvailability'),
        requestDevice: _eoka_mark(function requestDevice() {
            return Promise.reject(new DOMException('User cancelled', 'NotFoundError'));
        }, 'requestDevice'),
        getDevices: _eoka_mark(function getDevices() {
            return Promise.resolve([]);
        }, 'getDevices'),
        addEventListener: _eoka_mark(function addEventListener() {}, 'addEventListener'),
        removeEventListener: _eoka_mark(function removeEventListener() {}, 'removeEventListener'),
        dispatchEvent: _eoka_mark(function dispatchEvent() { return true; }, 'dispatchEvent')
    };

    Object.defineProperty(Navigator.prototype, 'bluetooth', {
        get: _eoka_mark(function () { return _eoka_bluetooth; }, 'get bluetooth'),
        configurable: true,
        enumerable: true
    });
}
