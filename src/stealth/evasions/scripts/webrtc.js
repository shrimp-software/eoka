
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
