
if (!navigator.bluetooth) {
    Object.defineProperty(Navigator.prototype, 'bluetooth', {
        get: _eoka_mark(() => ({
            getAvailability: () => Promise.resolve(false),
            requestDevice: () => Promise.reject(new DOMException('User cancelled', 'NotFoundError')),
            getDevices: () => Promise.resolve([]),
            addEventListener: () => {},
            removeEventListener: () => {},
            dispatchEvent: () => true
        }), 'get bluetooth'),
        configurable: true,
        enumerable: true
    });
}
