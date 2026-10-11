
const navProps = {
    languages: { get: () => Object.freeze(__EOKA_LANGUAGES__) },
    language: { get: () => '__EOKA_LANGUAGE__' },
    platform: { get: () => '__EOKA_PLATFORM__' },
    vendor: { get: () => 'Google Inc.' },
    hardwareConcurrency: { get: () => __EOKA_HW__ },
    deviceMemory: { get: () => __EOKA_MEM__ },
    maxTouchPoints: { get: () => 0 }
};
for (const [prop, desc] of Object.entries(navProps)) {
    _eoka_mark(desc.get, 'get ' + prop);
    Object.defineProperty(Navigator.prototype, prop, {
        ...desc,
        configurable: true,
        enumerable: true
    });
}
