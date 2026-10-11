
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
