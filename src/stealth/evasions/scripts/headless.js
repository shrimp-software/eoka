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
