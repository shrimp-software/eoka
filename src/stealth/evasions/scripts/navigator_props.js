const navProps = {
    languages: { get: () => Object.freeze(__EOKA_LANGUAGES__) },
    language: { get: () => '__EOKA_LANGUAGE__' },
    platform: { get: () => '__EOKA_PLATFORM__' },
    vendor: { get: () => 'Google Inc.' },
    hardwareConcurrency: { get: () => __EOKA_HW__ },
    deviceMemory: { get: () => __EOKA_MEM__ },
    maxTouchPoints: { get: () => 0 }
};

// `navigator.deviceMemory` is [SecureContext]: stock Chrome (Linux included)
// exposes it on https/localhost/file pages and omits it on http ones. Only
// spoof it where the native attribute exists, so an insecure page never gains
// a property stock Chrome would not have there.
if (!('deviceMemory' in Navigator.prototype)) {
    delete navProps.deviceMemory;
}

for (const [prop, desc] of Object.entries(navProps)) {
    _eoka_mark(desc.get, 'get ' + prop);
    Object.defineProperty(Navigator.prototype, prop, {
        ...desc,
        configurable: true,
        enumerable: true
    });
}

// `navigator.cookieEnabled` is native and truthful. Only repair the impossible
// pair "cookies disabled but a cookie write succeeds" (some embedded/automation
// contexts report false while the cookie jar works); never override a browser
// where cookies are genuinely disabled.
try {
    if (navigator.cookieEnabled === false) {
        const probe = '__eoka_cookie_probe__';
        document.cookie = probe + '=1; path=/';
        if (document.cookie.indexOf(probe) !== -1) {
            Object.defineProperty(Navigator.prototype, 'cookieEnabled', {
                get: _eoka_mark(() => true, 'get cookieEnabled'),
                configurable: true,
                enumerable: true
            });
        }
        document.cookie = probe + '=; expires=Thu, 01 Jan 1970 00:00:00 GMT; path=/';
    }
} catch(e) {}
