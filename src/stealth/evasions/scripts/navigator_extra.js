
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
