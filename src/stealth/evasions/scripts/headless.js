// matchMedia (reduced motion, pointer/hover) stays native; pointer and hover
// are pinned by `--blink-settings` in the launch arguments.

// Older headless builds had an empty `PerformanceObserver.supportedEntryTypes`.
// Restore the desktop list only then; never stub observe() or delivery, since a
// silent observer is itself a headless signal.
try {
    if (typeof PerformanceObserver === 'function') {
        const types = PerformanceObserver.supportedEntryTypes;
        if (!Array.isArray(types) || types.length === 0) {
            // Native returns the same frozen array on every read.
            const desktop = Object.freeze(['element', 'event', 'first-input',
                'interaction-contentful-paint', 'largest-contentful-paint', 'layout-shift',
                'long-animation-frame', 'longtask', 'mark', 'measure', 'navigation', 'paint',
                'resource', 'soft-navigation', 'visibility-state']);
            _eoka_patchGetter(PerformanceObserver, 'supportedEntryTypes', () => () => desktop);
        }
    }
} catch(e) {}

// `isSecureContext` follows the scheme and ancestors, so it stays native. Only
// the impossible https-but-insecure top-level frame is repaired; an https
// iframe under an http ancestor is genuinely insecure.
try {
    if (window.isSecureContext === false && window.location.protocol === 'https:' &&
            window.top === window) {
        _eoka_patchGetter(window, 'isSecureContext', () => () => true);
    }
} catch(e) {}

// `window.sandbox` does not exist in stock Chrome; at document start any value
// comes from the automation environment.
try {
    if ('sandbox' in window) {
        delete window.sandbox;
    }
} catch(e) {}

// Ensure window.origin is correct
try {
    if (!window.origin || window.origin === 'null') {
        _eoka_patchGetter(window, 'origin', () => () => window.location.origin);
    }
} catch(e) {}
