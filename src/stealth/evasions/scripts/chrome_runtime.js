
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
