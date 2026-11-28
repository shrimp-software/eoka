// Native `navigator.languages` returns the same frozen array on every read.
const _eoka_languages = Object.freeze(__EOKA_LANGUAGES__);
const navProps = {
    languages: () => _eoka_languages,
    language: () => '__EOKA_LANGUAGE__',
    platform: () => '__EOKA_PLATFORM__',
    vendor: () => 'Google Inc.',
    hardwareConcurrency: () => __EOKA_HW__,
    deviceMemory: () => __EOKA_MEM__,
    maxTouchPoints: () => 0
};

// `navigator.deviceMemory` is [SecureContext]; never add it where stock Chrome
// omits it (http pages).
if (!('deviceMemory' in Navigator.prototype)) {
    delete navProps.deviceMemory;
}

for (const [prop, value] of Object.entries(navProps)) {
    _eoka_patchGetter(Navigator.prototype, prop, () => value);
}

// `navigator.cookieEnabled` stays native unless it reports false while a
// cookie write succeeds, as some embedded/automation contexts do.
try {
    if (navigator.cookieEnabled === false) {
        const probe = '__eoka_cookie_probe__';
        document.cookie = probe + '=1; path=/';
        if (document.cookie.indexOf(probe) !== -1) {
            _eoka_patchGetter(Navigator.prototype, 'cookieEnabled', () => () => true);
        }
        document.cookie = probe + '=; expires=Thu, 01 Jan 1970 00:00:00 GMT; path=/';
    }
} catch(e) {}
