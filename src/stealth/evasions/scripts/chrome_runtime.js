
// Stock Chrome, headless/new included, already exposes `window.chrome` with
// native `loadTimes`, `csi` and `app` (verified against Chrome 156), so this
// fallback only runs where the object is missing. Native `loadTimes`/`csi`
// are anonymous, constructible functions, so they stay function expressions.
// Values derive from the document's own navigation timing: stable for the
// page, and consistent with `performance`.
if (!window.chrome) {
    const _eoka_sec = (ms) => ms / 1000;
    const _eoka_nav = () => performance.getEntriesByType('navigation')[0] || {};
    const _eoka_firstPaint = () => {
        const paint = performance.getEntriesByType('paint').find(e => e.name === 'first-paint');
        return paint ? _eoka_sec(performance.timeOrigin + paint.startTime) : 0;
    };
    // Array elements get no inferred name, matching the native empty name.
    const loadTimes = [function() {
        const t = performance.timing;
        const protocol = _eoka_nav().nextHopProtocol || 'http/1.1';
        const spdy = protocol === 'h2' || protocol === 'h3';
        return {
            commitLoadTime: _eoka_sec(t.responseStart),
            connectionInfo: protocol,
            finishDocumentLoadTime: _eoka_sec(t.domContentLoadedEventEnd),
            finishLoadTime: _eoka_sec(t.loadEventEnd),
            firstPaintAfterLoadTime: 0,
            firstPaintTime: _eoka_firstPaint(),
            navigationType: 'Other',
            npnNegotiatedProtocol: spdy ? protocol : 'unknown',
            requestTime: _eoka_sec(t.navigationStart),
            startLoadTime: _eoka_sec(t.navigationStart),
            wasAlternateProtocolAvailable: false,
            wasFetchedViaSpdy: spdy,
            wasNpnNegotiated: spdy
        };
    }][0];
    const csi = [function() {
        const t = performance.timing;
        return {
            onloadT: t.domContentLoadedEventEnd,
            pageT: performance.now(),
            startE: t.navigationStart,
            tran: 15
        };
    }][0];
    _eoka_mark(loadTimes, '');
    _eoka_mark(csi, '');
    window.chrome = {
        loadTimes,
        csi,
        app: {
            isInstalled: false,
            InstallState: { DISABLED: 'disabled', INSTALLED: 'installed', NOT_INSTALLED: 'not_installed' },
            RunningState: { CANNOT_RUN: 'cannot_run', READY_TO_RUN: 'ready_to_run', RUNNING: 'running' }
        }
    };
}
