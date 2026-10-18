// Real headless/new and headed Chrome both report the native pair
// (reduce=false, no-preference=true). Wrapping matchMedia used to fabricate
// an impossible both-false state, so it is intentionally left untouched.
// The pointer/hover mismatch real headless X servers have is fixed natively
// with `--blink-settings=primaryPointerType=4,...` in the launch arguments.

// PerformanceObserver is fully implemented in current headless Chrome. The one
// observable difference older headless builds had was an empty
// `supportedEntryTypes`, which a detector can read without constructing an
// observer. Restore the desktop list only in that case; never stub observe()
// or delivery, because a silent observer is itself a headless signal.
try {
    if (typeof PerformanceObserver === 'function') {
        const types = PerformanceObserver.supportedEntryTypes;
        if (!Array.isArray(types) || types.length === 0) {
            Object.defineProperty(PerformanceObserver, 'supportedEntryTypes', {
                get: _eoka_mark(function () {
                    return ['element', 'event', 'first-input', 'interaction-contentful-paint',
                        'largest-contentful-paint', 'layout-shift', 'long-animation-frame',
                        'longtask', 'mark', 'measure', 'navigation', 'paint', 'resource',
                        'soft-navigation', 'visibility-state'];
                }, 'get supportedEntryTypes'),
                configurable: true
            });
        }
    }
} catch(e) {}

// `isSecureContext` must stay native: it follows the scheme (and sandbox
// flags), so forcing it true on an http:// page would contradict
// location.protocol. Only repair the impossible https-but-insecure state.
try {
    if (window.isSecureContext === false && window.location.protocol === 'https:') {
        Object.defineProperty(window, 'isSecureContext', {
            get: _eoka_mark(function () { return true; }, 'get isSecureContext'),
            configurable: true
        });
    }
} catch(e) {}

// `window.sandbox` does not exist in stock Chrome. This script runs at
// document start, before page globals exist, so any value here comes from the
// automation environment and is safe to remove.
try {
    if ('sandbox' in window) {
        delete window.sandbox;
    }
} catch(e) {}

// Ensure window.origin is correct
try {
    if (!window.origin || window.origin === 'null') {
        Object.defineProperty(window, 'origin', {
            get: _eoka_mark(function() { return window.location.origin; }, 'get origin'),
            configurable: true
        });
    }
} catch(e) {}
