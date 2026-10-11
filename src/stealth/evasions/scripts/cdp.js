
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
