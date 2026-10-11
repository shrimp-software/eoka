
if (navigator.keyboard && navigator.keyboard.getLayoutMap && __EOKA_KEYBOARD_MAP__) {
    const layoutEntries = __EOKA_KEYBOARD_MAP__;
    const layoutMap = new Map(Object.entries(layoutEntries));
    const fakeMap = {
        get: _eoka_mark((code) => layoutMap.get(code) || null, 'get'),
        has: _eoka_mark((code) => layoutMap.has(code), 'has'),
        size: layoutMap.size,
        entries: _eoka_mark(() => layoutMap.entries(), 'entries'),
        forEach: _eoka_mark((cb, thisArg) => layoutMap.forEach(cb, thisArg), 'forEach'),
        [Symbol.iterator]: _eoka_mark(function* () {
            yield* layoutMap.entries();
        }, '[Symbol.iterator]'),
    };
    navigator.keyboard.getLayoutMap = _eoka_mark(function getLayoutMap() {
        return Promise.resolve(fakeMap);
    }, 'getLayoutMap');
}
