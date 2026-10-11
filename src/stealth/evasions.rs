//! Evasion scripts to bypass bot detection
//!
//! These scripts are injected before any page content loads to patch
//! detectable browser properties.

use crate::page::escape_js_string;
use crate::stealth::fingerprint::{Fingerprint, GENERIC_FONTS};
use crate::StealthConfig;

/// Native-function masking: makes `_eoka_mark`ed functions report native code from `.toString()`.
pub const NATIVE_TOSTRING_EVASION: &str = r#"
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
"#;

/// Core WebDriver evasion - define webdriver=false on prototype (more realistic)
pub const WEBDRIVER_EVASION: &str = r#"
const _eoka_webdriverGet = _eoka_mark(() => false, 'get webdriver');
Object.defineProperty(Object.getPrototypeOf(navigator), 'webdriver', {
    get: _eoka_webdriverGet,
    configurable: true,
    enumerable: true
});
try {
    Object.defineProperty(Navigator.prototype, 'webdriver', {
        get: _eoka_webdriverGet,
        configurable: true,
        enumerable: true
    });
} catch(e) {}

// Remove automation-related properties from window
const automationProps = [
    'callPhantom', '_phantom', 'phantom', '__nightmare', 'domAutomation',
    'domAutomationController', '_selenium', '_Selenium_IDE_Recorder',
    'callSelenium', '__webdriver_script_fn', '__driver_evaluate',
    '__webdriver_evaluate', '__fxdriver_evaluate', '__driver_unwrapped',
    '__webdriver_unwrapped', '__fxdriver_unwrapped', '__selenium_unwrapped',
    '_WEBDRIVER_ELEM_CACHE', 'ChromeDriverw', 'driver-evaluate',
    'webdriver-evaluate', 'selenium-evaluate', 'webdriverCommand',
    'webdriver-evaluate-response', '__webdriverFunc', '__lastWatirAlert',
    '__lastWatirConfirm', '__lastWatirPrompt', '$chrome_asyncScriptInfo',
    '__selenium_evaluate', '__webdriver_script_function'
];

automationProps.forEach(prop => {
    try {
        if (prop in window) delete window[prop];
    } catch(e) {}
});

// Fix for Chrome's runtime.bindingsCalled check
if (window.chrome && window.chrome.runtime) {
    try {
        delete window.chrome.runtime.bindingsCalled;
    } catch(e) {}
}
"#;

/// CDP marker cleanup - simple and effective approach from chaser-oxide
pub const CDP_EVASION: &str = r#"
// Pattern to match CDP/automation markers
const cdcPattern = /^cdc_|^\$cdc_|^__webdriver|^__selenium|^__driver|^\$chrome_|^\$wdc_/;

// Clean up CDP markers from window
const cleanupWindow = () => {
    for (const prop of Object.getOwnPropertyNames(window)) {
        if (cdcPattern.test(prop)) {
            try { delete window[prop]; } catch(e) {}
        }
    }
};

// Clean up CDP markers from document
const cleanupDocument = () => {
    for (const prop of Object.getOwnPropertyNames(document)) {
        if (cdcPattern.test(prop)) {
            try { delete document[prop]; } catch(e) {}
        }
    }
};

// Run cleanup immediately
cleanupWindow();
cleanupDocument();

// Override Object.getOwnPropertyNames to filter out CDP markers
const originalGetOwnPropertyNames = Object.getOwnPropertyNames;
Object.getOwnPropertyNames = _eoka_mark(function getOwnPropertyNames(obj) {
    const names = originalGetOwnPropertyNames.call(this, obj);
    if (obj === window || obj === document) {
        return names.filter(name => !cdcPattern.test(name));
    }
    return names;
}, 'getOwnPropertyNames');

// Override Object.keys to filter out CDP markers
const originalKeys = Object.keys;
Object.keys = _eoka_mark(function keys(obj) {
    const keys = originalKeys.call(this, obj);
    if (obj === window || obj === document) {
        return keys.filter(key => !cdcPattern.test(key));
    }
    return keys;
}, 'keys');

// Override hasOwnProperty to hide CDP markers
const originalHasOwnProperty = Object.prototype.hasOwnProperty;
Object.prototype.hasOwnProperty = _eoka_mark(function hasOwnProperty(prop) {
    if ((this === window || this === document) && cdcPattern.test(prop)) {
        return false;
    }
    return originalHasOwnProperty.call(this, prop);
}, 'hasOwnProperty');

// Override Object.getOwnPropertyDescriptor to hide CDP markers
const originalGetOwnPropertyDescriptor = Object.getOwnPropertyDescriptor;
Object.getOwnPropertyDescriptor = _eoka_mark(function getOwnPropertyDescriptor(obj, prop) {
    if ((obj === window || obj === document) && cdcPattern.test(prop)) {
        return undefined;
    }
    return originalGetOwnPropertyDescriptor.call(this, obj, prop);
}, 'getOwnPropertyDescriptor');

// Intercept direct property access on document using a Proxy (bound fns cached for stable identity).
const realDocument = document;
const _eoka_boundCache = new WeakMap();
try {
    const documentProxy = new Proxy(realDocument, {
        get(target, prop, receiver) {
            if (typeof prop === 'string' && cdcPattern.test(prop)) {
                return undefined;
            }
            const value = Reflect.get(target, prop, target);
            if (typeof value === 'function') {
                let cache = _eoka_boundCache.get(target);
                if (!cache) { cache = new Map(); _eoka_boundCache.set(target, cache); }
                if (!cache.has(prop)) cache.set(prop, value.bind(target));
                return cache.get(prop);
            }
            return value;
        },
        has(target, prop) {
            if (typeof prop === 'string' && cdcPattern.test(prop)) {
                return false;
            }
            return Reflect.has(target, prop);
        },
        getOwnPropertyDescriptor(target, prop) {
            if (typeof prop === 'string' && cdcPattern.test(prop)) {
                return undefined;
            }
            return Reflect.getOwnPropertyDescriptor(target, prop);
        },
        ownKeys(target) {
            return Reflect.ownKeys(target).filter(key =>
                typeof key !== 'string' || !cdcPattern.test(key)
            );
        }
    });

    Object.defineProperty(window, 'document', {
        get: _eoka_mark(() => documentProxy, 'get document'),
        configurable: true
    });
} catch(e) {
    let cleanupCount = 0;
    const cleanupTimer = setInterval(() => {
        cleanupWindow();
        cleanupDocument();
        if (++cleanupCount >= 10) clearInterval(cleanupTimer);
    }, 200);
}

// Clean up document element attributes
const cleanupDocAttributes = () => {
    const attrs = ['selenium', 'webdriver', 'driver'];
    const docElement = document.documentElement;
    if (docElement) {
        attrs.forEach(attr => {
            try {
                if (docElement.hasAttribute(attr)) {
                    docElement.removeAttribute(attr);
                }
            } catch(e) {}
        });
    }
};

if (document.documentElement) {
    cleanupDocAttributes();
}
document.addEventListener('DOMContentLoaded', cleanupDocAttributes);
"#;

/// Chrome runtime object. Matches a real, extension-less Chrome page context:
/// `Object.keys(window.chrome)` is `["loadTimes", "csi", "app"]` and
/// `chrome.runtime` is undefined (verified against stock Chrome 152).
pub const CHROME_RUNTIME_EVASION: &str = r#"
window.chrome = {
    loadTimes: function() {
        return {
            commitLoadTime: Date.now() / 1000 - _eoka_rand() * 2,
            connectionInfo: "h2",
            finishDocumentLoadTime: Date.now() / 1000 - _eoka_rand(),
            finishLoadTime: Date.now() / 1000 - _eoka_rand() * 0.5,
            firstPaintAfterLoadTime: 0,
            firstPaintTime: Date.now() / 1000 - _eoka_rand() * 1.5,
            navigationType: "Other",
            npnNegotiatedProtocol: "h2",
            requestTime: Date.now() / 1000 - _eoka_rand() * 3,
            startLoadTime: Date.now() / 1000 - _eoka_rand() * 2.5,
            wasAlternateProtocolAvailable: false,
            wasFetchedViaSpdy: true,
            wasNpnNegotiated: true
        };
    },
    csi: function() {
        return {
            onloadT: Date.now(),
            pageT: _eoka_rand() * 1000 + 500,
            startE: Date.now() - _eoka_rand() * 3000,
            tran: 15
        };
    },
    app: {
        isInstalled: false,
        InstallState: { DISABLED: "disabled", INSTALLED: "installed", NOT_INSTALLED: "not_installed" },
        RunningState: { CANNOT_RUN: "cannot_run", READY_TO_RUN: "ready_to_run", RUNNING: "running" }
    }
};
"#;

/// Permissions API consistency fix
pub const PERMISSIONS_EVASION: &str = r#"
// Fix Notification/Permissions API consistency
const originalPermissionsQuery = navigator.permissions.query.bind(navigator.permissions);

try {
    Object.defineProperty(Notification, 'permission', {
        get: _eoka_mark(() => 'default', 'get permission'),
        configurable: true,
        enumerable: true
    });
} catch(e) {}

navigator.permissions.query = _eoka_mark(function query(parameters) {
    const name = parameters.name;

    if (name === 'notifications') {
        return Promise.resolve({
            state: 'prompt',
            name: 'notifications',
            onchange: null,
            addEventListener: function() {},
            removeEventListener: function() {},
            dispatchEvent: function() { return true; }
        });
    }

    if (name === 'clipboard-read' || name === 'clipboard-write') {
        return Promise.resolve({
            state: 'prompt',
            name: name,
            onchange: null,
            addEventListener: function() {},
            removeEventListener: function() {},
            dispatchEvent: function() { return true; }
        });
    }

    return originalPermissionsQuery(parameters).then(function(result) {
        return {
            state: result.state,
            name: result.name || name,
            onchange: result.onchange,
            addEventListener: result.addEventListener?.bind(result) || function() {},
            removeEventListener: result.removeEventListener?.bind(result) || function() {},
            dispatchEvent: result.dispatchEvent?.bind(result) || function() { return true; }
        };
    }).catch(function() {
        return {
            state: 'prompt',
            name: name,
            onchange: null,
            addEventListener: function() {},
            removeEventListener: function() {},
            dispatchEvent: function() { return true; }
        };
    });
}, 'query');
"#;

/// Plugins spoofing - defined on Navigator.prototype with a stable cached reference.
pub const PLUGINS_EVASION: &str = r#"
const _eoka_buildPlugins = () => {
    const plugins = [
        { name: 'PDF Viewer', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
        { name: 'Chrome PDF Viewer', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
        { name: 'Chromium PDF Viewer', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
        { name: 'Microsoft Edge PDF Viewer', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
        { name: 'WebKit built-in PDF', filename: 'internal-pdf-viewer', description: 'Portable Document Format' }
    ];

    const pluginArray = Object.create(PluginArray.prototype);
    plugins.forEach((p, i) => {
        const plugin = Object.create(Plugin.prototype);
        Object.defineProperties(plugin, {
            name: { value: p.name, enumerable: true },
            filename: { value: p.filename, enumerable: true },
            description: { value: p.description, enumerable: true },
            length: { value: 1, enumerable: true }
        });
        pluginArray[i] = plugin;
        pluginArray[p.name] = plugin;
    });

    Object.defineProperty(pluginArray, 'length', { value: plugins.length });
    pluginArray.item = _eoka_mark((i) => pluginArray[i] || null, 'item');
    pluginArray.namedItem = _eoka_mark((name) => pluginArray[name] || null, 'namedItem');
    pluginArray.refresh = _eoka_mark(() => {}, 'refresh');
    return pluginArray;
};

const _eoka_pluginArray = _eoka_buildPlugins();
Object.defineProperty(Navigator.prototype, 'plugins', {
    get: _eoka_mark(() => _eoka_pluginArray, 'get plugins'),
    configurable: true,
    enumerable: true
});
"#;

/// Navigator properties (languages, platform, hardware) filled from the fingerprint.
pub const NAVIGATOR_PROPS_EVASION: &str = r#"
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
"#;

/// Headless detection bypass
pub const HEADLESS_EVASION: &str = r#"
// Fix screen properties. The subtracted band is the OS chrome height
// (taskbar / menu bar) claimed by the fingerprint.
Object.defineProperty(screen, 'availWidth', { get: _eoka_mark(() => screen.width, 'get availWidth'), configurable: true });
Object.defineProperty(screen, 'availHeight', { get: _eoka_mark(() => screen.height - __EOKA_UI_CHROME__, 'get availHeight'), configurable: true });

// Mock window dimensions
Object.defineProperty(window, 'outerWidth', { get: _eoka_mark(() => window.innerWidth, 'get outerWidth'), configurable: true });
Object.defineProperty(window, 'outerHeight', { get: _eoka_mark(() => window.innerHeight + 85, 'get outerHeight'), configurable: true });

// Real headless/new and headed Chrome both report the native pair
// (reduce=false, no-preference=true). Wrapping matchMedia used to fabricate
// an impossible both-false state, so it is intentionally left untouched.

// Ensure window.origin is correct
try {
    if (!window.origin || window.origin === 'null') {
        Object.defineProperty(window, 'origin', {
            get: _eoka_mark(function() { return window.location.origin; }, 'get origin'),
            configurable: true
        });
    }
} catch(e) {}
"#;

/// Battery API fix - Proxy over the real BatteryManager with spoofed values.
pub const BATTERY_EVASION: &str = r#"
const originalGetBattery = Navigator.prototype.getBattery;
if (originalGetBattery) {
    Object.defineProperty(Navigator.prototype, 'getBattery', {
        value: _eoka_mark(function getBattery() {
            return originalGetBattery.call(this).then(function(battery) {
                return new Proxy(battery, {
                    get(target, prop) {
                        switch (prop) {
                            case 'charging': return true;
                            case 'chargingTime': return 0;
                            case 'dischargingTime': return Infinity;
                            case 'level': return 1;
                        }
                        const v = target[prop];
                        return typeof v === 'function' ? v.bind(target) : v;
                    }
                });
            });
        }, 'getBattery'),
        configurable: true,
        writable: true
    });
}
"#;

/// Navigator connection + userAgentData hygiene
pub const NAVIGATOR_EXTRA_EVASION: &str = r#"
if (navigator.userAgentData) {
    const originalGetHighEntropyValues = navigator.userAgentData.getHighEntropyValues?.bind(navigator.userAgentData);
    if (originalGetHighEntropyValues) {
        navigator.userAgentData.getHighEntropyValues = _eoka_mark(function getHighEntropyValues(hints) {
            return originalGetHighEntropyValues(hints).then(function(data) {
                if (data.brands) {
                    data.brands = data.brands.filter(b =>
                        !b.brand.toLowerCase().includes('headless')
                    );
                }
                return data;
            });
        }, 'getHighEntropyValues');
    }
}

// Fix navigator.connection
if (navigator.connection) {
    try {
        const connectionProps = {
            downlink: 10,
            effectiveType: '4g',
            rtt: 50,
            saveData: false
        };

        for (const [key, value] of Object.entries(connectionProps)) {
            try {
                Object.defineProperty(navigator.connection, key, {
                    get: _eoka_mark(() => value, 'get ' + key),
                    configurable: true,
                    enumerable: true
                });
            } catch(e) {}
        }
    } catch(e) {}
}
"#;

/// Combined fingerprint evasion (WebGL + Canvas + Audio).
pub const FINGERPRINT_EVASION: &str = r#"
const _eoka_spoofWebGL = (proto) => {
    const orig = proto.getParameter;
    proto.getParameter = _eoka_mark(function getParameter(p) {
        if (p === 37445) return '__EOKA_WEBGL_VENDOR__';
        if (p === 37446) return '__EOKA_WEBGL_RENDERER__';
        return orig.call(this, p);
    }, 'getParameter');
};
_eoka_spoofWebGL(WebGLRenderingContext.prototype);
if (typeof WebGL2RenderingContext !== 'undefined') _eoka_spoofWebGL(WebGL2RenderingContext.prototype);

// Both readback and export use the native getter and apply noise once.
const origGetImageData = CanvasRenderingContext2D.prototype.getImageData;
const _eoka_noisePixels = (img) => {
    const data = img.data;
    let s = (__EOKA_NOISE_SEED__ >>> 0) || 1;
    for (let i = 0; i < data.length; i += 4) {
        s = _eoka_lcg(s);
        // Partially transparent RGB is quantized by the canvas backing store.
        // Leave it alone so exporting cannot change the readback noise.
        if (data[i + 3] === 255) data[i] = data[i] ^ (s & 1);
    }
    return img;
};
const _eoka_noiseCanvas = (canvas) => {
    const w = canvas.width | 0, h = canvas.height | 0;
    if (w <= 0 || h <= 0) return canvas;
    try {
        const tmp = document.createElement('canvas');
        tmp.width = w; tmp.height = h;
        const tctx = tmp.getContext('2d');
        tctx.drawImage(canvas, 0, 0);
        const img = origGetImageData.call(tctx, 0, 0, w, h);
        tctx.putImageData(_eoka_noisePixels(img), 0, 0);
        return tmp;
    } catch(e) {
        // Preserve native export failures, including tainted canvas errors.
        return canvas;
    }
};

const origToDataURL = HTMLCanvasElement.prototype.toDataURL;
HTMLCanvasElement.prototype.toDataURL = _eoka_mark(function toDataURL(...args) {
    return origToDataURL.apply(_eoka_noiseCanvas(this), args);
}, 'toDataURL');

const origToBlob = HTMLCanvasElement.prototype.toBlob;
if (origToBlob) {
    HTMLCanvasElement.prototype.toBlob = _eoka_mark(function toBlob(...args) {
        return origToBlob.apply(_eoka_noiseCanvas(this), args);
    }, 'toBlob');
}

CanvasRenderingContext2D.prototype.getImageData = _eoka_mark(function getImageData(...args) {
    return _eoka_noisePixels(origGetImageData.apply(this, args));
}, 'getImageData');

// Audio noise — deterministic, applied once per channel buffer.
const origGetChannelData = AudioBuffer.prototype.getChannelData;
const _eoka_audioSeen = new WeakSet();
AudioBuffer.prototype.getChannelData = _eoka_mark(function getChannelData(ch) {
    const d = origGetChannelData.call(this, ch);
    if (!_eoka_audioSeen.has(d)) {
        let s = ((__EOKA_NOISE_SEED__ >>> 0) ^ (ch >>> 0)) >>> 0 || 1;
        for (let i = 0; i < d.length; i += 100) {
            s = _eoka_lcg(s);
            d[i] += (s / 4294967296 - 0.5) * 0.0001;
        }
        _eoka_audioSeen.add(d);
    }
    return d;
}, 'getChannelData');
"#;

/// WebRTC leak protection - prevent real IP from leaking via public STUN.
pub const WEBRTC_EVASION: &str = r#"
if (typeof RTCPeerConnection !== 'undefined') {
    const origRTCPeerConnection = RTCPeerConnection;

    const _eoka_filterServers = (config) => {
        if (config && config.iceServers) {
            config.iceServers = config.iceServers.filter(server => {
                const urls = Array.isArray(server.urls) ? server.urls : [server.urls];
                return !urls.some(url =>
                    /^stuns?:/i.test(url) || /^turns?:/i.test(url)
                );
            });
        }
        return config;
    };

    window.RTCPeerConnection = _eoka_mark(function RTCPeerConnection(config, constraints) {
        return new origRTCPeerConnection(_eoka_filterServers(config), constraints);
    }, 'RTCPeerConnection');

    window.RTCPeerConnection.prototype = origRTCPeerConnection.prototype;
    Object.keys(origRTCPeerConnection).forEach(key => {
        window.RTCPeerConnection[key] = origRTCPeerConnection[key];
    });
}

if (typeof webkitRTCPeerConnection !== 'undefined') {
    window.webkitRTCPeerConnection = window.RTCPeerConnection;
}
"#;

/// Speech synthesis - headless Chrome has 0 voices, real Chrome has many.
/// The fallback list is fingerprint data (`__EOKA_SPEECH_VOICES__`, JSON):
/// platform-aware and stable per identity.
pub const SPEECH_EVASION: &str = r#"
if (typeof speechSynthesis !== 'undefined') {
    const defaultVoices = __EOKA_SPEECH_VOICES__.map(v => {
        const voice = Object.create(SpeechSynthesisVoice.prototype);
        Object.defineProperties(voice, {
            name: { value: v.name, enumerable: true },
            lang: { value: v.lang, enumerable: true },
            localService: { value: v.localService, enumerable: true },
            default: { value: v.default, enumerable: true },
            voiceURI: { value: v.voiceURI, enumerable: true }
        });
        return voice;
    });

    const origGetVoices = speechSynthesis.getVoices.bind(speechSynthesis);
    speechSynthesis.getVoices = _eoka_mark(function getVoices() {
        const voices = origGetVoices();
        return voices.length > 0 ? voices : defaultVoices;
    }, 'getVoices');
}
"#;

/// Media devices - headless returns empty array. The fake device list is
/// fingerprint data (`__EOKA_MEDIA_DEVICES__`, JSON) derived from the noise
/// seed, so IDs are stable per identity and never shared across sessions.
pub const MEDIA_DEVICES_EVASION: &str = r#"
if (navigator.mediaDevices && navigator.mediaDevices.enumerateDevices) {
    const origEnumerateDevices = navigator.mediaDevices.enumerateDevices.bind(navigator.mediaDevices);

    const fakeDevices = __EOKA_MEDIA_DEVICES__;
    navigator.mediaDevices.enumerateDevices = _eoka_mark(async function enumerateDevices() {
        const devices = await origEnumerateDevices();
        if (devices.length === 0) {
            return fakeDevices.map(d => {
                const device = Object.create(MediaDeviceInfo.prototype);
                Object.defineProperties(device, {
                    deviceId: { value: d.deviceId, enumerable: true },
                    groupId: { value: d.groupId, enumerable: true },
                    kind: { value: d.kind, enumerable: true },
                    label: { value: d.label, enumerable: true },
                    toJSON: { value: _eoka_mark(() => d, 'toJSON') }
                });
                return device;
            });
        }
        return devices;
    }, 'enumerateDevices');
}
"#;

/// Bluetooth API - headless throws error, real Chrome returns object
pub const BLUETOOTH_EVASION: &str = r#"
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
"#;

/// System-font probing via `document.fonts.check(font, text)`: stock Chrome
/// resolves installed system fonts, so detectors enumerate font names through
/// it. Only the identity's claimed fonts (plus CSS generic families) resolve;
/// everything else reports missing. The claimed set is fingerprint data
/// (`__EOKA_FONTS__`, JSON array).
pub const FONTS_EVASION: &str = r#"
if (document.fonts && document.fonts.check) {
    const origCheck = document.fonts.check.bind(document.fonts);
    const claimedFonts = new Set(__EOKA_FONTS__.map(f => f.toLowerCase()));
    const genericFonts = new Set(__EOKA_GENERIC_FONTS__);
    const familyAllowed = (family) =>
        claimedFonts.has(family.toLowerCase()) || genericFonts.has(family.toLowerCase());

    document.fonts.check = _eoka_mark(function check(font, text) {
        if (typeof font !== 'string' || font === '') return origCheck(font, text);
        const sizeMatch = font.match(/(?:\d+(?:\.\d+)?(?:px|pt|em|rem|ex|ch|vh|vw|%)|[xx]-?small|medium|[xx]-?large)\s*(.*)$/i);
        const familyList = sizeMatch ? sizeMatch[1] : font;
        if (!familyList.trim()) return origCheck(font, text);
        const families = familyList.split(',').map(f => f.trim().replace(/^['"]|['"]$/g, ''));
        if (!families.length || families.some(f => !f)) return origCheck(font, text);
        return families.every(familyAllowed);
    }, 'check');
}
"#;

/// `navigator.keyboard.getLayoutMap()` — stock Chrome exposes the Keyboard
/// API on Windows/macOS/ChromeOS but not Linux, so Linux passes through.
/// The map is fingerprint data (`__EOKA_KEYBOARD_MAP__`, JSON or null):
/// US ANSI, matching the identity's en-US locale.
pub const KEYBOARD_EVASION: &str = r#"
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
"#;

/// Timezone consistency - ensure Date timezone matches claimed location.
/// The `__EOKA_TIMEZONE__` placeholder is replaced at build time with the
/// actual IANA timezone from StealthConfig.
pub const TIMEZONE_EVASION: &str = r#"
// Ensure Intl.DateTimeFormat returns consistent timezone
const targetTimezone = '__EOKA_TIMEZONE__';
const origDateTimeFormat = Intl.DateTimeFormat;

Intl.DateTimeFormat = _eoka_mark(function DateTimeFormat(locales, options) {
    const merged = options ? Object.assign({}, options) : {};
    if (!merged.timeZone) merged.timeZone = targetTimezone;
    return new origDateTimeFormat(locales, merged);
}, 'DateTimeFormat');
Intl.DateTimeFormat.prototype = origDateTimeFormat.prototype;
Intl.DateTimeFormat.supportedLocalesOf = origDateTimeFormat.supportedLocalesOf;

// Override Date.prototype.getTimezoneOffset to match the target timezone.
// Precompute offsets for Jan and Jul to handle DST, avoiding per-call
// toLocaleString() which is ~10x slower than native and timing-detectable.
{
    const _origGetTZO = Date.prototype.getTimezoneOffset;
    function _calcOffset(date) {
        const utcStr = date.toLocaleString('en-US', { timeZone: 'UTC' });
        const tzStr = date.toLocaleString('en-US', { timeZone: targetTimezone });
        return (new Date(utcStr) - new Date(tzStr)) / 60000;
    }
    const year = new Date().getFullYear();
    const janOffset = _calcOffset(new Date(year, 0, 15));
    const julOffset = _calcOffset(new Date(year, 6, 15));
    // Standard offset is the larger value (more positive = further west)
    const stdOffset = Math.max(janOffset, julOffset);
    const dstOffset = Math.min(janOffset, julOffset);
    // If offsets differ, this timezone observes DST
    const hasDST = janOffset !== julOffset;
    // Which months are DST depends on hemisphere: if Jul is the smaller
    // offset, DST is northern-hemisphere (Mar-Nov); otherwise southern.
    const dstIsJul = julOffset < janOffset;

    Date.prototype.getTimezoneOffset = _eoka_mark(function getTimezoneOffset() {
        if (!hasDST) return stdOffset;
        // Quick month-based check — accurate for the vast majority of dates.
        const m = this.getUTCMonth();
        const isDST = dstIsJul ? (m >= 2 && m <= 10) : (m <= 2 || m >= 10);
        return isDST ? dstOffset : stdOffset;
    }, 'getTimezoneOffset');
}
"#;

/// Common timezones for random selection (balanced across regions)
pub(crate) const COMMON_TIMEZONES: &[&str] = &[
    // Americas (4)
    "America/New_York",
    "America/Chicago",
    "America/Los_Angeles",
    "America/Sao_Paulo",
    // Europe (4)
    "Europe/London",
    "Europe/Paris",
    "Europe/Berlin",
    "Europe/Moscow",
    // Asia-Pacific (4)
    "Asia/Tokyo",
    "Asia/Shanghai",
    "Asia/Kolkata",
    "Australia/Sydney",
];

/// Build the complete evasion script for the given config and fingerprint.
pub fn build_evasion_script_for(config: &StealthConfig, fp: &Fingerprint) -> String {
    let timezone = config
        .timezone
        .clone()
        .unwrap_or_else(|| fp.timezone.clone());

    let mut scripts = vec![
        NATIVE_TOSTRING_EVASION, // must be first: defines _eoka_mark / _eoka_rand
        WEBDRIVER_EVASION,
        CHROME_RUNTIME_EVASION,
        PERMISSIONS_EVASION,
        PLUGINS_EVASION,
        NAVIGATOR_PROPS_EVASION,
        HEADLESS_EVASION,
        BATTERY_EVASION,
        NAVIGATOR_EXTRA_EVASION,
        WEBRTC_EVASION,
        SPEECH_EVASION,
        MEDIA_DEVICES_EVASION,
        FONTS_EVASION,
        KEYBOARD_EVASION,
        BLUETOOTH_EVASION,
        TIMEZONE_EVASION,
    ];

    // This legacy module globally proxies `window.document` and patches Object
    // reflection methods. Keep it available for callers that explicitly need
    // it, but do not make a page's core DOM behavior depend on it by default.
    if config.aggressive_cdp_evasion {
        scripts.push(CDP_EVASION);
    }

    if config.webgl_spoof || config.canvas_spoof || config.audio_spoof {
        scripts.push(FINGERPRINT_EVASION);
    }

    let languages_json = format!(
        "[{}]",
        fp.languages
            .iter()
            .map(|l| format!("'{}'", escape_js_string(l)))
            .collect::<Vec<_>>()
            .join(",")
    );
    let language = fp.languages.first().cloned().unwrap_or_default();
    let noise_seed = fp.noise_seed;

    let script = format!("(function(){{{}}})();", scripts.join("\n"));

    // Single fill-point: every placeholder value comes from the fingerprint
    // (or config). No placeholder may survive this fold — guarded by
    // `test_placeholders_are_filled`.
    let placeholders: Vec<(&str, String)> = vec![
        ("__EOKA_TIMEZONE__", escape_js_string(&timezone)),
        ("__EOKA_PLATFORM__", escape_js_string(fp.nav_platform())),
        ("__EOKA_LANGUAGES__", languages_json),
        ("__EOKA_LANGUAGE__", escape_js_string(&language)),
        ("__EOKA_HW__", fp.hardware_concurrency.to_string()),
        ("__EOKA_MEM__", fp.device_memory.to_string()),
        ("__EOKA_WEBGL_VENDOR__", escape_js_string(&fp.webgl_vendor)),
        (
            "__EOKA_WEBGL_RENDERER__",
            escape_js_string(&fp.webgl_renderer),
        ),
        ("__EOKA_NOISE_SEED__", noise_seed.to_string()),
        (
            "__EOKA_SPEECH_VOICES__",
            serde_json::to_string(&fp.voices).unwrap_or_else(|_| "[]".to_string()),
        ),
        ("__EOKA_MEDIA_DEVICES__", fp.media_devices_json()),
        ("__EOKA_UI_CHROME__", fp.ui_chrome_height().to_string()),
        (
            "__EOKA_FONTS__",
            serde_json::to_string(&fp.fonts).unwrap_or_else(|_| "[]".to_string()),
        ),
        (
            "__EOKA_GENERIC_FONTS__",
            serde_json::to_string(GENERIC_FONTS).unwrap_or_else(|_| "[]".to_string()),
        ),
        ("__EOKA_KEYBOARD_MAP__", fp.keyboard_map_json()),
    ];
    placeholders
        .into_iter()
        .fold(script, |acc, (placeholder, value)| {
            acc.replace(placeholder, &value)
        })
}

/// Build the complete evasion script based on config.
pub fn build_evasion_script(config: &StealthConfig) -> String {
    let fp = Fingerprint::resolve(config.user_agent.as_deref(), config.timezone.as_deref());
    build_evasion_script_for(config, &fp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_full_script() {
        let config = StealthConfig::default();
        let script = build_evasion_script(&config);
        assert!(script.contains("webdriver"));
        assert!(script.contains("WebGLRenderingContext"));
        assert!(script.contains("HTMLCanvasElement"));
        assert!(script.contains("AudioBuffer"));
        assert!(!script.contains("const documentProxy"));
    }

    #[test]
    fn test_aggressive_cdp_evasion_is_opt_in() {
        let config = StealthConfig {
            aggressive_cdp_evasion: true,
            ..StealthConfig::default()
        };
        assert!(build_evasion_script(&config).contains("const documentProxy"));
    }

    #[test]
    fn test_placeholders_are_filled() {
        let script = build_evasion_script(&StealthConfig::default());
        assert!(
            !script.contains("__EOKA_"),
            "unresolved placeholder in script"
        );
        assert!(script.contains("_eoka_mark"));
    }

    #[test]
    fn test_script_is_wrapped_in_iife() {
        let config = StealthConfig::default();
        let script = build_evasion_script(&config);
        assert!(script.starts_with("(function()"));
        assert!(script.ends_with("})();"));
    }

    // Regression: the script must be driven by the fingerprint, not by old
    // hardcoded values. Build for a known Windows fingerprint and assert the
    // Windows-specific fields flow through while the old Mac constants and
    // removed features do not.
    #[test]
    fn test_build_script_for_known_windows_fp() {
        let fp = Fingerprint::resolve(
            Some("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.7312.86 Safari/537.36"),
            None,
        );
        let config = StealthConfig::default();
        let script = build_evasion_script_for(&config, &fp);

        // navigator.platform comes from the fingerprint (Win32), and the old
        // hardcoded MacIntel must be gone.
        assert!(
            script.contains("'Win32'"),
            "script must contain the fingerprint nav_platform 'Win32'"
        );
        assert!(
            !script.contains("'MacIntel'"),
            "script must not contain the old hardcoded 'MacIntel'"
        );

        // WebGL renderer and hardware values flow from the fingerprint.
        assert!(
            script.contains(&fp.webgl_renderer),
            "script must contain the fingerprint webgl_renderer: {}",
            fp.webgl_renderer
        );
        assert!(
            script.contains(&fp.hardware_concurrency.to_string()),
            "script must contain hardware_concurrency"
        );
        assert!(
            script.contains(&fp.device_memory.to_string()),
            "script must contain device_memory"
        );

        // toString-masking machinery is present.
        assert!(script.contains("_eoka_mark"));
        assert!(script.contains("[native code]"));

        // No leftover placeholders, and the removed beacon must be gone.
        assert!(
            !script.contains("__EOKA_"),
            "script must not contain unresolved placeholders"
        );
        assert!(
            !script.contains("__eoka_pending_requests"),
            "removed request-beacon must not reappear"
        );
    }

    // Escaping regression: config values with a single quote must be routed
    // through escape_js_string, producing an escaped `\'` in the output.
    #[test]
    fn test_config_values_are_js_escaped() {
        let config = StealthConfig {
            timezone: Some("Foo'Bar".into()),
            ..StealthConfig::default()
        };
        let script = build_evasion_script(&config);
        assert!(
            script.contains(r"Foo\'Bar"),
            "timezone single quote must be escaped via escape_js_string"
        );
        assert!(
            !script.contains("'Foo'Bar'"),
            "unescaped single quote must not appear"
        );
    }

    // Audit regression: stock Chrome pages expose window.chrome keys as
    // ["loadTimes", "csi", "app"] only — chrome.runtime is extension-only and
    // must never be injected (verified against stock Chrome 152).
    #[test]
    fn test_chrome_object_matches_stock() {
        let script = build_evasion_script(&StealthConfig::default());
        assert!(
            script.contains("loadTimes: function"),
            "chrome.loadTimes must be spoofed (present in stock Chrome)"
        );
        assert!(
            script.contains("csi: function"),
            "chrome.csi must be spoofed (present in stock Chrome)"
        );
        assert!(
            !script.contains("onConnect"),
            "chrome.runtime stub must not be re-added: plain pages have no runtime"
        );
        assert!(
            !script.contains("sendMessage: function"),
            "chrome.runtime stub must not be re-added"
        );
    }

    // Audit regression: the old matchMedia wrapper reported matches:false for
    // BOTH prefers-reduced-motion variants — an impossible state (one always
    // matches in real Chrome) — and returned a non-MediaQueryList object.
    #[test]
    fn test_match_media_is_not_wrapped() {
        let script = build_evasion_script(&StealthConfig::default());
        assert!(
            !script.contains("originalMatchMedia"),
            "matchMedia must not be wrapped: stock headless/new reports the correct pair"
        );
    }

    // Audit regression: fallback voices must match the claimed platform.
    // Alex/Samantha are macOS-only; Microsoft David/Zira are Windows-only;
    // stock Linux Chrome has no built-in voices (pass-through).
    #[test]
    fn test_speech_voices_match_platform() {
        let voices_for = |ua: &str| {
            let fp = Fingerprint::resolve(Some(ua), None);
            let script = build_evasion_script_for(&StealthConfig::default(), &fp);
            let start = script
                .find("const defaultVoices = ")
                .expect("voices array present")
                + "const defaultVoices = ".len();
            let end = script[start..]
                .find(".map(v =>")
                .expect("voices array terminated");
            script[start..start + end].trim().to_string()
        };

        let linux = voices_for("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36");
        assert_eq!(
            linux, "[]",
            "Linux identities must pass through with no voices"
        );

        let mac = voices_for("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36");
        let mac_json: serde_json::Value =
            serde_json::from_str(&mac).expect("macOS voices must be valid JSON");
        let mac_names: Vec<&str> = mac_json
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["name"].as_str().unwrap())
            .collect();
        assert!(mac_names.contains(&"Alex"), "macOS list must contain Alex");
        assert!(!mac_names.iter().any(|n| n.starts_with("Microsoft")));

        let win = voices_for("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36");
        let win_json: serde_json::Value =
            serde_json::from_str(&win).expect("Windows voices must be valid JSON");
        let win_names: Vec<&str> = win_json
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["name"].as_str().unwrap())
            .collect();
        assert!(
            win_names.iter().any(|n| n.starts_with("Microsoft David")),
            "Windows list must contain Microsoft David"
        );
        assert!(!win_names.contains(&"Alex"), "Alex is macOS-only");
    }

    // Audit regression: fake media-device IDs used to be one hardcoded
    // constant shared by every eoka session, linking all identities. They are
    // now fingerprint data (spec + noise seed), stable per identity and
    // distinct across identities.
    #[test]
    fn test_media_device_ids_are_seeded_per_fingerprint() {
        let devices_json = |script: &str| -> serde_json::Value {
            let start = script
                .find("const fakeDevices = ")
                .expect("fake device list present")
                + "const fakeDevices = ".len();
            let end = script[start..].find(';').expect("device list terminated");
            serde_json::from_str(&script[start..start + end])
                .expect("media device list must be valid JSON")
        };

        let script = build_evasion_script(&StealthConfig::default());
        assert!(
            !script.contains("5f8b2c1e9a3d"),
            "hardcoded cross-session groupId must not reappear"
        );

        let devices = devices_json(&script).as_array().cloned().unwrap();
        assert!(!devices.is_empty());
        let mut ids: Vec<&str> = devices
            .iter()
            .map(|d| d["deviceId"].as_str().unwrap())
            .collect();
        let defaults = ids.iter().filter(|id| **id == "default").count();
        ids.retain(|id| *id != "default");
        assert_eq!(
            ids.len(),
            ids.iter().collect::<std::collections::HashSet<_>>().len(),
            "non-default device IDs must be unique within an identity"
        );
        assert!(
            ids.iter().all(|id| id.len() == 12),
            "device IDs must be 12 hex chars like real enumerated IDs"
        );
        assert!(defaults >= 2, "audio in/out pair should include 'default'");

        // Different fingerprints must present different hardware IDs. The
        // identity (and therefore the seed) is randomized per build.
        let fps_a = Fingerprint::random();
        let fps_b = Fingerprint::random();
        let ids_a =
            devices_json(&build_evasion_script_for(&StealthConfig::default(), &fps_a)).to_string();
        let ids_b =
            devices_json(&build_evasion_script_for(&StealthConfig::default(), &fps_b)).to_string();
        assert_ne!(
            ids_a, ids_b,
            "distinct identities must not share device IDs (linkability)"
        );
    }

    // Audit regression: canvas reads through getImageData must carry the same
    // noise as toDataURL/toBlob, or a page comparing both flags the hook.
    #[test]
    fn test_get_image_data_is_noised_with_canvas_spoof() {
        let on = build_evasion_script(&StealthConfig::default());
        assert!(
            on.contains("CanvasRenderingContext2D.prototype.getImageData"),
            "getImageData must be hooked when canvas spoofing is on"
        );

        let off = build_evasion_script(&StealthConfig {
            webgl_spoof: false,
            canvas_spoof: false,
            audio_spoof: false,
            ..StealthConfig::default()
        });
        assert!(
            !off.contains("getImageData"),
            "canvas hooks must be absent when all fingerprint spoofs are off"
        );
    }

    // Audit regression: the Intl shim must not mutate the caller's options
    // object (observable side effect).
    #[test]
    fn test_intl_shim_clones_options() {
        let script = build_evasion_script(&StealthConfig::default());
        assert!(
            script.contains("Object.assign({}, options)"),
            "Intl.DateTimeFormat shim must clone options before injecting timeZone"
        );
    }
}
