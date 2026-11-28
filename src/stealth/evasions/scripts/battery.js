
// Report a charged, plugged-in battery through the real BatteryManager.
if (typeof Navigator.prototype.getBattery === 'function') {
    _eoka_patchMethod(Navigator.prototype, 'getBattery', (original) => function(...args) {
        return original.apply(this, args).then(function(battery) {
            return new Proxy(battery, {
                get(target, prop) {
                    switch (prop) {
                        case 'charging': return true;
                        case 'chargingTime': return 0;
                        case 'dischargingTime': return Infinity;
                        case 'level': return 1;
                    }
                    const v = target[prop];
                    return typeof v === 'function' ? v.bind(target) : v;
                }
            });
        });
    });
}
