
// Brands and high-entropy hints come from the native user-agent metadata
// override, so `navigator.userAgentData` is left untouched here.

// Legacy headless reported an all-zero NetworkInformation; stock Chrome
// (headless/new included) reports measured values, which stay native.
if (navigator.connection && navigator.connection.rtt === 0 && navigator.connection.downlink === 0 &&
        typeof NetworkInformation !== 'undefined') {
    const connectionProps = { downlink: 10, effectiveType: '4g', rtt: 50, saveData: false };
    for (const [key, value] of Object.entries(connectionProps)) {
        try {
            _eoka_patchGetter(NetworkInformation.prototype, key, () => () => value);
        } catch(e) {}
    }
}
