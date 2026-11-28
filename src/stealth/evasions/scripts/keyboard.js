
if (typeof Keyboard !== 'undefined' && Keyboard.prototype.getLayoutMap && __EOKA_KEYBOARD_MAP__) {
    const layoutEntries = __EOKA_KEYBOARD_MAP__;
    const layoutMap = new Map(Object.entries(layoutEntries));
    const fakeMap = {
        get(code) { return layoutMap.get(code); },
        has(code) { return layoutMap.has(code); },
        size: layoutMap.size,
        entries() { return layoutMap.entries(); },
        keys() { return layoutMap.keys(); },
        values() { return layoutMap.values(); },
        forEach(cb, thisArg) { layoutMap.forEach((v, k) => cb.call(thisArg, v, k, fakeMap)); },
        [Symbol.iterator]() { return layoutMap.entries(); },
    };
    for (const method of Object.values(fakeMap)) _eoka_mark(method);
    _eoka_mark(fakeMap[Symbol.iterator], 'entries');
    _eoka_patchMethod(Keyboard.prototype, 'getLayoutMap', () => function() {
        return Promise.resolve(fakeMap);
    });
}
